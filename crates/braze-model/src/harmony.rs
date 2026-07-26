//! Formato **Harmony** (gpt-oss) para el `LocalBackend`: plantilla de
//! prompt (system + developer + historial) y parser incremental del
//! stream de salida (canales `analysis`/`commentary`/`final`, tool calls
//! `to=functions.*`). Emitir el formato con el que gpt-oss fue entrenado
//! — en vez de reciclar el preámbulo ChatML de qwen — es la misma tesis
//! anti-"format tax" que cerró la Fase 1 (schema_fail 17→0 al pasar al
//! preámbulo nativo de qwen).
//!
//! Módulo **puro** (sin dependencia de llama.cpp) y compilado también
//! sin el feature `local`, para que sus tests corran en el `cargo test`
//! normal del workspace (donde llama.cpp no se compila). `local.rs`
//! (feature-gated) mapea tokens especiales → [`HarmonyMarker`] / texto y
//! consume los [`HarmonyEvent`].
//!
//! Diferencia estructural vs. ChatML/qwen: en Harmony los marcadores
//! (`<|channel|>`, `<|message|>`, `<|call|>`, …) son **tokens
//! especiales** del vocabulario o200k_harmony — no sobreviven un
//! `token_to_piece(special=false)` y por eso la escalera de rescate del
//! engine nunca los vería. El parsing vive entonces en el backend (que
//! es dueño de los tokens) y emite `ToolCallRequested` directo; la
//! escalera del engine queda como red de seguridad para el texto visible
//! residual, igual que con cualquier otro backend.
//!
//! Aceptación MVP (misma que la plantilla ChatML de Fase 1): el
//! contenido de usuario/tools se interpola sin escapar, así que un
//! contenido que contenga literalmente `<|end|>` u otro marcador se
//! tokenizará como token de control (la tokenización del prompt parsea
//! specials). Mismo trade-off ya aceptado para `<|im_end|>` en qwen.

use std::collections::{HashMap, HashSet};

use braze_types::{ContentBlock, Role, ToolStub};

use crate::backend::CompletionRequest;

// ---------------------------------------------------------------------
// Plantilla de prompt
// ---------------------------------------------------------------------

/// Arma el prompt completo en formato Harmony: mensaje `system` canónico
/// (identidad + cutoff + fecha + reasoning + canales válidos), mensaje
/// `developer` (`# Instructions` = system prompt de braze + `# Tools`
/// como namespace TypeScript), el historial mapeado a mensajes Harmony,
/// y el turno del assistant abierto (`<|start|>assistant` — el modelo
/// genera `<|channel|>…` a continuación).
///
/// `reasoning` es el esfuerzo de razonamiento (`low`/`medium`/`high`)
/// que gpt-oss lee del system message; `current_date` (YYYY-MM-DD) se
/// omite si es `None`.
pub(crate) fn build_harmony_prompt(
    req: &CompletionRequest,
    reasoning: &str,
    current_date: Option<&str>,
) -> String {
    let mut out = String::new();

    // Mensaje system canónico de Harmony. La identidad "You are ChatGPT"
    // es parte del formato entrenado (cambiarla es format tax); la
    // identidad operativa de braze va en el developer message.
    out.push_str(
        "<|start|>system<|message|>You are ChatGPT, a large language model trained by OpenAI.\n\
         Knowledge cutoff: 2024-06\n",
    );
    if let Some(date) = current_date {
        out.push_str("Current date: ");
        out.push_str(date);
        out.push('\n');
    }
    out.push_str("\nReasoning: ");
    out.push_str(reasoning);
    out.push_str(
        "\n\n# Valid channels: analysis, commentary, final. \
         Channel must be included for every message.",
    );
    if !req.tool_stubs.is_empty() {
        out.push_str("\nCalls to these tools must go to the commentary channel: 'functions'.");
    }
    out.push_str("<|end|>");

    // Mensaje developer: instrucciones de braze + namespace de tools.
    if !req.system_prompt.is_empty() || !req.tool_stubs.is_empty() {
        out.push_str("<|start|>developer<|message|>");
        if !req.system_prompt.is_empty() {
            out.push_str("# Instructions\n\n");
            out.push_str(&req.system_prompt);
        }
        if !req.tool_stubs.is_empty() {
            if !req.system_prompt.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(&render_harmony_tools(&req.tool_stubs));
        }
        out.push_str("<|end|>");
    }

    // Historial. Los ToolResult llegan como bloques dentro de turnos
    // `user` (convención del engine); Harmony los quiere como mensajes
    // del rol `functions.<name>`, así que el mapeo es por bloque, no por
    // mensaje, y el nombre se recupera del ToolUse previo vía su id.
    let mut tool_names: HashMap<&str, &str> = HashMap::new();
    for msg in &req.messages {
        let has_tool_use = msg
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
        for block in &msg.content {
            match block {
                ContentBlock::Text { text } => match msg.role {
                    Role::User => {
                        out.push_str("<|start|>user<|message|>");
                        out.push_str(text);
                        out.push_str("<|end|>");
                    }
                    Role::System => {
                        out.push_str("<|start|>developer<|message|>");
                        out.push_str(text);
                        out.push_str("<|end|>");
                    }
                    Role::Assistant => {
                        // Texto que acompaña a una tool call = razonamiento
                        // previo a la call → canal analysis. Texto solo =
                        // respuesta al usuario → canal final.
                        let channel = if has_tool_use { "analysis" } else { "final" };
                        out.push_str("<|start|>assistant<|channel|>");
                        out.push_str(channel);
                        out.push_str("<|message|>");
                        out.push_str(text);
                        out.push_str("<|end|>");
                    }
                },
                ContentBlock::ToolUse { id, name, input } => {
                    tool_names.insert(id.as_str(), name.as_str());
                    out.push_str("<|start|>assistant<|channel|>commentary to=functions.");
                    out.push_str(name);
                    out.push_str(" <|constrain|>json<|message|>");
                    out.push_str(&input.to_string());
                    out.push_str("<|call|>");
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    let name = tool_names
                        .get(tool_use_id.as_str())
                        .copied()
                        .unwrap_or("tool");
                    out.push_str("<|start|>functions.");
                    out.push_str(name);
                    out.push_str(" to=assistant<|channel|>commentary<|message|>");
                    if *is_error {
                        out.push_str("[tool error] ");
                    }
                    out.push_str(content);
                    out.push_str("<|end|>");
                }
            }
        }
    }

    out.push_str("<|start|>assistant");
    out
}

/// Sección `# Tools` del developer message: el namespace `functions` en
/// la sintaxis TypeScript-like con la que gpt-oss fue entrenado (misma
/// razón que el preámbulo `<tools>` de qwen: formato entrenado > formato
/// ad-hoc). Cuando el `ToolStub` trae `input_schema` se traduce a un
/// object type con comentarios `//` por propiedad; sin schema cae a
/// `(_: any)` (tools diferidos/MCP sin resolver — mismo fallback
/// nombre+summary de la Fase 1).
fn render_harmony_tools(stubs: &[ToolStub]) -> String {
    let mut s = String::from("# Tools\n\n## functions\n\nnamespace functions {\n\n");
    for stub in stubs {
        if !stub.summary.is_empty() {
            s.push_str("// ");
            s.push_str(&stub.summary.replace('\n', "\n// "));
            s.push('\n');
        }
        s.push_str("type ");
        s.push_str(&stub.name);
        s.push_str(" = ");
        s.push_str(&render_signature(stub));
        s.push_str(";\n\n");
    }
    s.push_str("} // namespace functions");
    s
}

fn render_signature(stub: &ToolStub) -> String {
    let Some(schema) = &stub.input_schema else {
        return "(_: any) => any".to_string();
    };
    if let Some(props) = render_props(schema) {
        return format!("(_: {{\n{props}}}) => any");
    }
    // Schema de objeto sin propiedades = tool sin argumentos.
    if schema.get("type").and_then(serde_json::Value::as_str) == Some("object") {
        return "() => any".to_string();
    }
    format!("(_: {}) => any", ts_type(schema))
}

/// Propiedades de un schema de objeto como líneas `nombre?: tipo,` con
/// el `description` como comentario encima y el `default` como
/// comentario al final — el layout del renderer de Harmony.
fn render_props(schema: &serde_json::Value) -> Option<String> {
    let props = schema.get("properties")?.as_object()?;
    if props.is_empty() {
        return None;
    }
    let required: HashSet<&str> = schema
        .get("required")
        .and_then(serde_json::Value::as_array)
        .map(|r| r.iter().filter_map(serde_json::Value::as_str).collect())
        .unwrap_or_default();
    let mut s = String::new();
    for (name, prop) in props {
        if let Some(desc) = prop.get("description").and_then(serde_json::Value::as_str) {
            s.push_str("// ");
            s.push_str(&desc.replace('\n', "\n// "));
            s.push('\n');
        }
        s.push_str(name);
        if !required.contains(name.as_str()) {
            s.push('?');
        }
        s.push_str(": ");
        s.push_str(&ts_type(prop));
        s.push(',');
        if let Some(default) = prop.get("default") {
            s.push_str(" // default: ");
            match default {
                serde_json::Value::String(d) => s.push_str(d),
                other => s.push_str(&other.to_string()),
            }
        }
        s.push('\n');
    }
    Some(s)
}

/// Traducción pragmática JSON Schema → tipo TS. Cubre lo que los schemas
/// de tools de braze usan de verdad (primitivos, enums, arrays, objetos
/// anidados simples); lo demás degrada a `any` — el modelo igual recibe
/// el `description`.
fn ts_type(schema: &serde_json::Value) -> String {
    if let Some(vals) = schema
        .get("enum")
        .and_then(serde_json::Value::as_array)
        .filter(|vals| !vals.is_empty())
    {
        return vals
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join(" | ");
    }
    match schema.get("type") {
        Some(serde_json::Value::String(t)) => ts_scalar(t, schema),
        Some(serde_json::Value::Array(ts)) => {
            let parts: Vec<String> = ts
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(|t| ts_scalar(t, schema))
                .collect();
            if parts.is_empty() {
                "any".to_string()
            } else {
                parts.join(" | ")
            }
        }
        _ => "any".to_string(),
    }
}

fn ts_scalar(t: &str, schema: &serde_json::Value) -> String {
    match t {
        "string" => "string".to_string(),
        "number" | "integer" => "number".to_string(),
        "boolean" => "boolean".to_string(),
        "null" => "null".to_string(),
        "array" => {
            let inner = schema.get("items").map_or("any".to_string(), ts_type);
            if inner.contains('|') {
                format!("({inner})[]")
            } else {
                format!("{inner}[]")
            }
        }
        "object" => {
            render_props(schema).map_or_else(|| "object".to_string(), |p| format!("{{\n{p}}}"))
        }
        _ => "any".to_string(),
    }
}

// ---------------------------------------------------------------------
// Parser incremental del stream de salida
// ---------------------------------------------------------------------

/// Los tokens especiales de Harmony que estructuran la salida. `local.rs`
/// resuelve sus ids en el vocabulario del GGUF y los mapea acá; el resto
/// de los tokens llega como texto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HarmonyMarker {
    Start,
    End,
    Message,
    Channel,
    Constrain,
    Call,
    Return,
}

impl HarmonyMarker {
    pub(crate) const ALL: [HarmonyMarker; 7] = [
        HarmonyMarker::Start,
        HarmonyMarker::End,
        HarmonyMarker::Message,
        HarmonyMarker::Channel,
        HarmonyMarker::Constrain,
        HarmonyMarker::Call,
        HarmonyMarker::Return,
    ];

    /// El literal del token especial en el vocabulario o200k_harmony.
    pub(crate) fn literal(self) -> &'static str {
        match self {
            HarmonyMarker::Start => "<|start|>",
            HarmonyMarker::End => "<|end|>",
            HarmonyMarker::Message => "<|message|>",
            HarmonyMarker::Channel => "<|channel|>",
            HarmonyMarker::Constrain => "<|constrain|>",
            HarmonyMarker::Call => "<|call|>",
            HarmonyMarker::Return => "<|return|>",
        }
    }
}

/// Lo que el parser le entrega a `local.rs` para traducir a
/// `CompletionEvent`.
#[derive(Debug, PartialEq)]
pub(crate) enum HarmonyEvent {
    /// Texto visible al usuario: canal `final`, o `commentary` sin
    /// destinatario (los "preámbulos" user-visible de gpt-oss). Se emite
    /// como `TextDelta` — streaming, no buffered.
    Visible(String),
    /// Tool call completa (cerrada por `<|call|>`, o por `<|end|>` como
    /// lenidad ante un cierre off-spec). `raw_args` va a la escalera de
    /// reparación de args compartida (`parse_arguments_with_repair`).
    ToolCall { name: String, raw_args: String },
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    /// Acumulando el header de un mensaje (rol/canal/destinatario) hasta
    /// `<|message|>`.
    Header,
    /// Dentro del contenido de un mensaje.
    Body,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Collect {
    Role,
    Channel,
    Constrain,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Channel {
    Analysis,
    Commentary,
    Final,
    /// Sin header parseado (o canal desconocido): el texto se trata como
    /// visible — degradación elegante si el modelo no sigue Harmony (el
    /// texto fluye como con qwen y la escalera de rescate del engine
    /// sigue aplicando).
    Unknown,
}

/// Máquina de estados incremental sobre la salida de gpt-oss. La
/// generación arranca justo después del `<|start|>assistant` del prompt,
/// así que el primer token esperado es `<|channel|>`; el estado inicial
/// es sin embargo `Body`/`Unknown` para que un modelo que ignore el
/// formato degrade a texto visible en vez de desaparecer.
pub(crate) struct HarmonyParser {
    state: State,
    collect: Collect,
    role_buf: String,
    channel_buf: String,
    channel: Channel,
    recipient: Option<String>,
    args_buf: String,
}

impl HarmonyParser {
    pub(crate) fn new() -> Self {
        Self {
            state: State::Body,
            collect: Collect::Role,
            role_buf: String::new(),
            channel_buf: String::new(),
            channel: Channel::Unknown,
            recipient: None,
            args_buf: String::new(),
        }
    }

    /// Un fragmento de texto normal (piece de un token no-especial).
    pub(crate) fn feed_text(&mut self, piece: &str) -> Option<HarmonyEvent> {
        match self.state {
            State::Header => {
                match self.collect {
                    Collect::Role => self.role_buf.push_str(piece),
                    Collect::Channel => self.channel_buf.push_str(piece),
                    // El tipo de constrain ("json") no aporta nada.
                    Collect::Constrain => {}
                }
                None
            }
            State::Body => {
                if self.recipient.is_some() {
                    self.args_buf.push_str(piece);
                    return None;
                }
                match self.channel {
                    // El razonamiento no es texto de usuario — se
                    // suprime del stream (análogo al campo `thinking`
                    // separado que Ollama nunca mezcla en `content`).
                    Channel::Analysis => None,
                    Channel::Commentary | Channel::Final | Channel::Unknown => {
                        Some(HarmonyEvent::Visible(piece.to_string()))
                    }
                }
            }
        }
    }

    /// Un token especial de Harmony. Puede cerrar una tool call.
    pub(crate) fn feed_marker(&mut self, marker: HarmonyMarker) -> Option<HarmonyEvent> {
        match marker {
            HarmonyMarker::Start => {
                self.reset_message();
                self.state = State::Header;
                self.collect = Collect::Role;
                None
            }
            HarmonyMarker::Channel => {
                // Un mensaje puede llegar sin <|start|> previo (el primer
                // mensaje generado): el canal abre header igual.
                if self.state != State::Header {
                    self.reset_message();
                }
                self.state = State::Header;
                self.collect = Collect::Channel;
                None
            }
            HarmonyMarker::Constrain => {
                if self.state == State::Header {
                    self.collect = Collect::Constrain;
                }
                None
            }
            HarmonyMarker::Message => {
                self.parse_header();
                self.state = State::Body;
                self.args_buf.clear();
                None
            }
            HarmonyMarker::Call | HarmonyMarker::End => {
                let event = self.take_tool_call();
                self.reset_message();
                self.state = State::Header;
                self.collect = Collect::Role;
                event
            }
            HarmonyMarker::Return => None,
        }
    }

    /// Si el mensaje que se está cerrando era una tool call, la emite.
    /// `<|end|>` en vez de `<|call|>` es off-spec pero se acepta
    /// (lenidad: mejor despachar la call que perderla en silencio).
    fn take_tool_call(&mut self) -> Option<HarmonyEvent> {
        let name = self.recipient.take()?;
        let raw_args = std::mem::take(&mut self.args_buf);
        Some(HarmonyEvent::ToolCall { name, raw_args })
    }

    fn reset_message(&mut self) {
        self.role_buf.clear();
        self.channel_buf.clear();
        self.channel = Channel::Unknown;
        self.recipient = None;
        self.args_buf.clear();
    }

    /// Al ver `<|message|>`: decide canal y destinatario a partir de lo
    /// acumulado. El canal es el prefijo del texto post-`<|channel|>`
    /// (`starts_with`, no igualdad: puede venir pegado al ` to=…`); el
    /// destinatario es cualquier palabra `to=functions.<name>` del
    /// header (Harmony lo admite antes o después del canal).
    fn parse_header(&mut self) {
        let cb = self.channel_buf.trim_start();
        self.channel = if cb.starts_with("analysis") {
            Channel::Analysis
        } else if cb.starts_with("commentary") {
            Channel::Commentary
        } else if cb.starts_with("final") {
            Channel::Final
        } else {
            Channel::Unknown
        };
        self.recipient = self
            .role_buf
            .split_whitespace()
            .chain(self.channel_buf.split_whitespace())
            .find_map(|w| w.strip_prefix("to="))
            .map(|r| r.strip_prefix("functions.").unwrap_or(r).to_string())
            .filter(|r| !r.is_empty());
    }

    /// True si el mensaje en curso es una tool call a medio acumular —
    /// para diagnosticar un presupuesto de tokens agotado a mitad de la
    /// call (el análogo local del `stop_reason` de los wires).
    pub(crate) fn tool_call_in_progress(&self) -> bool {
        self.recipient.is_some()
    }

    /// El destinatario de la call en curso (`to=functions.<name>` ya
    /// parseado del header) — el stencil lo usa para seleccionar la
    /// gramática de args derivada del schema de esa tool.
    pub(crate) fn pending_tool_name(&self) -> Option<&str> {
        self.recipient.as_deref()
    }
}

// ---------------------------------------------------------------------
// Fecha UTC sin chrono
// ---------------------------------------------------------------------

/// `YYYY-MM-DD` (UTC) desde segundos unix, para el `Current date:` del
/// system message. Conversión civil de Howard Hinnant — evita traerle
/// una dependencia de calendario al crate por una línea de prompt.
pub(crate) fn utc_date_string(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (
        if m <= 2 { y + 1 } else { y },
        u32::try_from(m).unwrap_or(1),
        u32::try_from(d).unwrap_or(1),
    )
}

#[cfg(test)]
mod tests {
    use braze_types::Message;

    use super::*;

    fn base_req() -> CompletionRequest {
        CompletionRequest {
            messages: vec![Message::text(Role::User, "hola")],
            tool_stubs: vec![],
            system_prompt: String::new(),
            max_tokens: 256,
        }
    }

    fn stub_with_schema() -> ToolStub {
        ToolStub {
            name: "write_file".to_string(),
            summary: "Write a file to disk".to_string(),
            source: "local".to_string(),
            input_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Target path" },
                    "mode": { "type": "string", "enum": ["create", "append"],
                              "default": "create" }
                },
                "required": ["path"]
            })),
        }
    }

    // ----- plantilla -----

    #[test]
    fn prompt_has_canonical_system_message_and_open_assistant_turn() {
        let prompt = build_harmony_prompt(&base_req(), "medium", Some("2026-07-21"));
        assert!(prompt.starts_with(
            "<|start|>system<|message|>You are ChatGPT, a large language model trained by OpenAI.\n"
        ));
        assert!(prompt.contains("Knowledge cutoff: 2024-06\n"));
        assert!(prompt.contains("Current date: 2026-07-21\n"));
        assert!(prompt.contains("\nReasoning: medium\n"));
        assert!(prompt.contains("# Valid channels: analysis, commentary, final."));
        // Sin tools no se promete el namespace functions.
        assert!(!prompt.contains("commentary channel: 'functions'"));
        assert!(prompt.contains("<|start|>user<|message|>hola<|end|>"));
        assert!(prompt.ends_with("<|start|>assistant"));
    }

    #[test]
    fn prompt_without_date_omits_current_date_line() {
        let prompt = build_harmony_prompt(&base_req(), "low", None);
        assert!(!prompt.contains("Current date:"));
    }

    #[test]
    fn developer_message_renders_instructions_and_ts_namespace() {
        let mut req = base_req();
        req.system_prompt = "You are braze.".to_string();
        req.tool_stubs = vec![stub_with_schema()];
        let prompt = build_harmony_prompt(&req, "medium", None);

        assert!(
            prompt.contains("Calls to these tools must go to the commentary channel: 'functions'.")
        );
        assert!(prompt.contains("<|start|>developer<|message|># Instructions\n\nYou are braze."));
        assert!(prompt.contains("# Tools\n\n## functions\n\nnamespace functions {"));
        assert!(prompt.contains("// Write a file to disk\ntype write_file = (_: {\n"));
        assert!(prompt.contains("// Target path\npath: string,\n"));
        assert!(prompt.contains("mode?: \"create\" | \"append\", // default: create\n"));
        assert!(prompt.contains("}) => any;"));
        assert!(prompt.contains("} // namespace functions<|end|>"));
    }

    #[test]
    fn schemaless_stub_falls_back_to_any_args() {
        let mut req = base_req();
        req.tool_stubs = vec![ToolStub {
            name: "mystery".to_string(),
            summary: "Deferred MCP tool".to_string(),
            source: "mcp".to_string(),
            input_schema: None,
        }];
        let prompt = build_harmony_prompt(&req, "medium", None);
        assert!(prompt.contains("type mystery = (_: any) => any;"));
    }

    #[test]
    fn empty_object_schema_renders_no_arg_signature() {
        let mut req = base_req();
        req.tool_stubs = vec![ToolStub {
            name: "list_tasks".to_string(),
            summary: String::new(),
            source: "local".to_string(),
            input_schema: Some(serde_json::json!({ "type": "object", "properties": {} })),
        }];
        let prompt = build_harmony_prompt(&req, "medium", None);
        assert!(prompt.contains("type list_tasks = () => any;"));
    }

    #[test]
    fn history_maps_tool_use_and_result_to_harmony_messages() {
        let mut req = base_req();
        req.messages = vec![
            Message::text(Role::User, "crea el archivo"),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Text {
                        text: "Voy a escribirlo.".to_string(),
                    },
                    ContentBlock::ToolUse {
                        id: "call-1".to_string(),
                        name: "write_file".to_string(),
                        input: serde_json::json!({"path": "x.txt"}),
                    },
                ],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call-1".to_string(),
                    content: "ok".to_string(),
                    is_error: false,
                }],
            },
        ];
        let prompt = build_harmony_prompt(&req, "medium", None);

        // Texto que acompaña a la call → analysis, no final.
        assert!(
            prompt.contains(
                "<|start|>assistant<|channel|>analysis<|message|>Voy a escribirlo.<|end|>"
            )
        );
        assert!(prompt.contains(
            "<|start|>assistant<|channel|>commentary to=functions.write_file \
             <|constrain|>json<|message|>{\"path\":\"x.txt\"}<|call|>"
        ));
        // El resultado recupera el nombre vía el id de la call.
        assert!(prompt.contains(
            "<|start|>functions.write_file to=assistant<|channel|>commentary<|message|>ok<|end|>"
        ));
    }

    #[test]
    fn assistant_text_without_tool_use_is_final_and_errors_are_tagged() {
        let mut req = base_req();
        req.messages = vec![
            Message::text(Role::Assistant, "listo"),
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "unknown-id".to_string(),
                    content: "boom".to_string(),
                    is_error: true,
                }],
            },
        ];
        let prompt = build_harmony_prompt(&req, "medium", None);
        assert!(prompt.contains("<|start|>assistant<|channel|>final<|message|>listo<|end|>"));
        // Sin ToolUse previo el nombre degrada a "tool"; el error se marca.
        assert!(prompt.contains(
            "<|start|>functions.tool to=assistant<|channel|>commentary<|message|>[tool error] boom<|end|>"
        ));
    }

    // ----- parser -----

    fn feed(
        parser: &mut HarmonyParser,
        script: &[Result<&str, HarmonyMarker>],
    ) -> Vec<HarmonyEvent> {
        let mut events = Vec::new();
        for step in script {
            let ev = match step {
                Ok(text) => parser.feed_text(text),
                Err(marker) => parser.feed_marker(*marker),
            };
            events.extend(ev);
        }
        events
    }

    #[test]
    fn final_channel_streams_visible_deltas() {
        let mut p = HarmonyParser::new();
        let events = feed(
            &mut p,
            &[
                Err(HarmonyMarker::Channel),
                Ok("final"),
                Err(HarmonyMarker::Message),
                Ok("Hola"),
                Ok(" mundo"),
                Err(HarmonyMarker::Return),
            ],
        );
        assert_eq!(
            events,
            vec![
                HarmonyEvent::Visible("Hola".to_string()),
                HarmonyEvent::Visible(" mundo".to_string()),
            ]
        );
    }

    #[test]
    fn analysis_is_suppressed_then_final_visible() {
        let mut p = HarmonyParser::new();
        let events = feed(
            &mut p,
            &[
                Err(HarmonyMarker::Channel),
                Ok("analysis"),
                Err(HarmonyMarker::Message),
                Ok("pensando..."),
                Err(HarmonyMarker::End),
                Err(HarmonyMarker::Start),
                Ok("assistant"),
                Err(HarmonyMarker::Channel),
                Ok("final"),
                Err(HarmonyMarker::Message),
                Ok("respuesta"),
                Err(HarmonyMarker::Return),
            ],
        );
        assert_eq!(events, vec![HarmonyEvent::Visible("respuesta".to_string())]);
    }

    #[test]
    fn tool_call_parses_recipient_and_buffers_args() {
        let mut p = HarmonyParser::new();
        let events = feed(
            &mut p,
            &[
                Err(HarmonyMarker::Channel),
                // El destinatario llega repartido en pieces arbitrarios.
                Ok("commentary to=functions.wri"),
                Ok("te_file "),
                Err(HarmonyMarker::Constrain),
                Ok("json"),
                Err(HarmonyMarker::Message),
                Ok("{\"path\": "),
                Ok("\"x.txt\"}"),
                Err(HarmonyMarker::Call),
            ],
        );
        assert_eq!(
            events,
            vec![HarmonyEvent::ToolCall {
                name: "write_file".to_string(),
                raw_args: "{\"path\": \"x.txt\"}".to_string(),
            }]
        );
    }

    #[test]
    fn commentary_preamble_without_recipient_is_visible() {
        let mut p = HarmonyParser::new();
        let events = feed(
            &mut p,
            &[
                Err(HarmonyMarker::Channel),
                Ok("commentary"),
                Err(HarmonyMarker::Message),
                Ok("Voy a crear tres archivos."),
                Err(HarmonyMarker::End),
            ],
        );
        assert_eq!(
            events,
            vec![HarmonyEvent::Visible(
                "Voy a crear tres archivos.".to_string()
            )]
        );
    }

    #[test]
    fn degenerate_plain_text_without_markers_is_visible() {
        // Modelo que no sigue Harmony: todo fluye como texto (y la
        // escalera de rescate del engine sigue aplicando río arriba).
        let mut p = HarmonyParser::new();
        let events = feed(&mut p, &[Ok("<tool_call>{...}</tool_call>")]);
        assert_eq!(
            events,
            vec![HarmonyEvent::Visible(
                "<tool_call>{...}</tool_call>".to_string()
            )]
        );
    }

    #[test]
    fn end_instead_of_call_still_flushes_the_tool_call() {
        let mut p = HarmonyParser::new();
        let events = feed(
            &mut p,
            &[
                Err(HarmonyMarker::Channel),
                Ok("commentary to=functions.read_file"),
                Err(HarmonyMarker::Message),
                Ok("{}"),
                Err(HarmonyMarker::End),
            ],
        );
        assert_eq!(
            events,
            vec![HarmonyEvent::ToolCall {
                name: "read_file".to_string(),
                raw_args: "{}".to_string(),
            }]
        );
    }

    #[test]
    fn tool_call_in_progress_reports_pending_call_and_name() {
        let mut p = HarmonyParser::new();
        assert!(!p.tool_call_in_progress());
        assert_eq!(p.pending_tool_name(), None);
        feed(
            &mut p,
            &[
                Err(HarmonyMarker::Channel),
                Ok("commentary to=functions.write_file"),
                Err(HarmonyMarker::Message),
                Ok("{\"path\": \"trunc"),
            ],
        );
        assert!(p.tool_call_in_progress());
        // El stencil selecciona la gramática de args con este nombre.
        assert_eq!(p.pending_tool_name(), Some("write_file"));
    }

    #[test]
    fn second_message_after_call_resets_state() {
        let mut p = HarmonyParser::new();
        let events = feed(
            &mut p,
            &[
                Err(HarmonyMarker::Channel),
                Ok("commentary to=functions.read_file"),
                Err(HarmonyMarker::Message),
                Ok("{}"),
                Err(HarmonyMarker::Call),
                Err(HarmonyMarker::Start),
                Ok("assistant"),
                Err(HarmonyMarker::Channel),
                Ok("final"),
                Err(HarmonyMarker::Message),
                Ok("hecho"),
                Err(HarmonyMarker::Return),
            ],
        );
        assert_eq!(
            events,
            vec![
                HarmonyEvent::ToolCall {
                    name: "read_file".to_string(),
                    raw_args: "{}".to_string(),
                },
                HarmonyEvent::Visible("hecho".to_string()),
            ]
        );
    }

    #[test]
    fn marker_literals_are_the_seven_distinct_harmony_specials() {
        // `ALL`/`literal` son el contrato con local.rs (resolución de ids
        // en el vocabulario); este test los fija — y los mantiene vivos
        // en el build sin feature `local`.
        let literals: HashSet<&str> = HarmonyMarker::ALL.iter().map(|m| m.literal()).collect();
        assert_eq!(literals.len(), 7);
        for lit in &literals {
            assert!(
                lit.starts_with("<|") && lit.ends_with("|>"),
                "literal raro: {lit}"
            );
        }
        assert!(literals.contains("<|call|>"));
        assert!(literals.contains("<|return|>"));
    }

    // ----- fecha -----

    #[test]
    fn utc_date_string_known_values() {
        assert_eq!(utc_date_string(0), "1970-01-01");
        assert_eq!(utc_date_string(946_684_800), "2000-01-01");
        // 2000-02-29 existió (año bisiesto divisible por 400).
        assert_eq!(utc_date_string(951_782_400), "2000-02-29");
        assert_eq!(utc_date_string(951_868_800), "2000-03-01");
    }
}
