//! Fixture tests de chat template contra el render de REFERENCIA — el
//! patrón de ferrumox/rabbit (`tests/qwen38_chat_template_fixture.rs`),
//! adoptado 2026-08-15: los unit tests de cada plantilla fijan la forma
//! *intencionada*; estos fijan que esa forma sea (o difiera de forma
//! DOCUMENTADA de) la forma con la que el modelo fue entrenado, porque
//! el modo de falla de un template equivocado no es un error — es un
//! modelo que razona en el lugar equivocado o nunca emite stop token.
//!
//! Los casos dorados los genera `tools/gen_chat_cases.py` (dev-time-only,
//! jinja2) renderizando el `chat_template.jinja` real de cada familia;
//! viven en `tests/fixtures/chat/<familia>_cases.json` y están
//! COMMITEADOS (regenerables con el script — ver su docstring por las
//! fuentes). Política de ausencia: skip con instrucción, nunca fail
//! (misma que rabbit).
//!
//! ## Desviaciones conocidas, fijadas aquí (2026-08-15)
//!
//! Las plantillas actuales difieren de la referencia y ESAS diferencias
//! están medidas implícitamente en todos los números del proyecto (el
//! 57/57 de gpt-oss, la paridad de Fase 1) — corregirlas sin A/B
//! invalidaría esas mediciones. Cada desviación se aplica como
//! transformación quirúrgica y un test compañero afirma que la
//! divergencia cruda EXISTE: si alguien corrige la plantilla (post-A/B),
//! ese test se rompe y obliga a borrar la transformación — el fixture se
//! auto-limpia.
//!
//! - **D1 (ChatML)**: braze emite `\n` antes del `<|im_end|>` de cada
//!   turno del historial (`render_blocks` cierra cada bloque con `\n`);
//!   la referencia qwen2.5 pega `<|im_end|>` directo al contenido.
//! - **D2 (Harmony)**: la referencia gpt-oss cierra el developer message
//!   con `\n\n` tras las instrucciones; braze lo omite.
//! - **D3 (Gemma)**: divergencia de DIALECTO completo — el template
//!   embebido en el GGUF gemma-4 QAT que braze corre es el formato nuevo
//!   `<|turn>role … <turn|>` (con rol system real, `<|tool>` y canal
//!   `thought`); braze renderiza el `<start_of_turn>` de gemma2/3.
//!   Format tax por construcción — candidato directo a explicar los 3
//!   fallos sistemáticos de single_tool de gemma4:e4b (A/B pendiente en
//!   la línea Gemma; no se "corrige" aquí). El fixture afirma la
//!   divergencia, no un match.
//!
//! Fuera de alcance deliberado: rondas de tools de Harmony (las dos
//! referencias públicas discrepan entre sí — el jinja de HF pone
//! `to=functions.X` tras `assistant`, la librería openai-harmony
//! canónica tras `<|channel|>commentary`, que es lo que braze implementa
//! y lo que gpt-oss emite) y el campo `tools` de qwen (el preámbulo de
//! braze es una desviación deliberada medida: Fase 1, schema_fail 17→0).

use braze_types::{ContentBlock, Message, Role};

use crate::backend::CompletionRequest;
use crate::chatml::build_chatml_prompt;
use crate::gemma::build_gemma_prompt;
use crate::harmony::build_harmony_prompt;

/// Carga un fixture, o `None` (el caller imprime skip) si falta.
fn load_fixture(family: &str) -> Option<serde_json::Value> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/chat")
        .join(format!("{family}_cases.json"));
    if !path.is_file() {
        eprintln!(
            "SKIP fixture de chat template '{family}': falta {} — regenerar con \
             tools/gen_chat_cases.py (ver su docstring por las fuentes de referencia)",
            path.display()
        );
        return None;
    }
    Some(serde_json::from_str(&std::fs::read_to_string(&path).expect("leer fixture")).expect("parsear fixture"))
}

/// Traduce los `messages` de un caso del fixture al `CompletionRequest`
/// que el engine armaría: el `system` inicial viaja como `system_prompt`
/// (no como mensaje), los turnos `tool` como `ToolResult` dentro de un
/// turno user (la convención del engine), y un `content` vacío en un
/// turno con tool_calls no agrega bloque de texto.
fn request_of(case: &serde_json::Value) -> (CompletionRequest, String) {
    let raw = case["messages"].as_array().expect("messages");
    let mut system_prompt = String::new();
    let mut messages: Vec<Message> = Vec::new();
    let mut call_counter = 0usize;
    let mut last_call_id = String::new();

    for (i, msg) in raw.iter().enumerate() {
        let role = msg["role"].as_str().expect("role");
        let content = msg["content"].as_str().unwrap_or_default();
        match role {
            "system" => {
                assert_eq!(i, 0, "los casos ponen el system solo al inicio");
                system_prompt = content.to_string();
            }
            "user" => messages.push(Message::text(Role::User, content)),
            "assistant" => {
                let mut blocks = Vec::new();
                if !content.is_empty() {
                    blocks.push(ContentBlock::Text {
                        text: content.to_string(),
                    });
                }
                if let Some(calls) = msg.get("tool_calls").and_then(|c| c.as_array()) {
                    for call in calls {
                        let function = &call["function"];
                        last_call_id = format!("fixture-call-{call_counter}");
                        call_counter += 1;
                        blocks.push(ContentBlock::ToolUse {
                            id: last_call_id.clone(),
                            name: function["name"].as_str().expect("name").to_string(),
                            input: function["arguments"].clone(),
                        });
                    }
                }
                messages.push(Message {
                    role: Role::Assistant,
                    content: blocks,
                });
            }
            "tool" => messages.push(Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: last_call_id.clone(),
                    content: content.to_string(),
                    is_error: false,
                }],
            }),
            other => panic!("rol inesperado en el fixture: {other}"),
        }
    }

    (
        CompletionRequest {
            messages,
            tool_stubs: vec![],
            system_prompt: system_prompt.clone(),
            max_tokens: 256,
        },
        system_prompt,
    )
}

/// D1: colapsa el `\n` que braze emite antes de cada `<|im_end|>`. Los
/// contenidos de los casos no terminan en `\n` (invariante del
/// generador), así que la transformación solo toca la desviación.
fn apply_d1(braze_render: &str) -> String {
    braze_render.replace("\n<|im_end|>", "<|im_end|>")
}

#[test]
fn chatml_matches_the_reference_render_modulo_documented_d1() {
    let Some(fixture) = load_fixture("chatml") else {
        return;
    };
    let mut mismatches = Vec::new();
    for case in fixture["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let (req, _) = request_of(case);
        let expected = case["expected"].as_str().unwrap();
        let got = apply_d1(&build_chatml_prompt(&req));
        if got != expected {
            mismatches.push(format!("caso {name}:\n  esperado {expected:?}\n  braze    {got:?}"));
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} caso(s) difieren de la referencia qwen2.5 más allá de D1:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

/// El compañero auto-limpiante de D1: la divergencia cruda debe EXISTIR.
/// Si esto falla porque los renders ya son idénticos, la plantilla se
/// corrigió — borrar `apply_d1` y este test, y re-validar los números
/// medidos con la plantilla vieja (la paridad de Fase 1 se midió con D1
/// presente).
#[test]
fn chatml_raw_render_still_carries_the_d1_deviation() {
    let Some(fixture) = load_fixture("chatml") else {
        return;
    };
    let case = &fixture["cases"].as_array().unwrap()[0];
    let (req, _) = request_of(case);
    let raw = build_chatml_prompt(&req);
    assert_ne!(
        raw,
        case["expected"].as_str().unwrap(),
        "los renders ya son idénticos: D1 fue corregida — eliminar apply_d1 y este test \
         (ver el doc comment del módulo)"
    );
    assert!(raw.contains("\n<|im_end|>"), "la firma concreta de D1 desapareció");
}

#[test]
fn harmony_matches_the_reference_render_modulo_documented_d2() {
    let Some(fixture) = load_fixture("harmony") else {
        return;
    };
    let date = fixture["date"].as_str().unwrap();
    let mut mismatches = Vec::new();
    for case in fixture["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let (req, system) = request_of(case);
        let expected = case["expected"].as_str().unwrap();
        let raw = build_harmony_prompt(&req, "medium", Some(date));
        // D2, quirúrgica: solo el cierre del developer message gana el
        // `\n\n` de la referencia — anclada al texto exacto del system
        // del caso para no tocar nada más.
        let got = raw.replacen(
            &format!("{system}<|end|>"),
            &format!("{system}\n\n<|end|>"),
            1,
        );
        if got != expected {
            mismatches.push(format!("caso {name}:\n  esperado {expected:?}\n  braze    {got:?}"));
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} caso(s) difieren de la referencia gpt-oss más allá de D2:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

/// Compañero auto-limpiante de D2 — mismo contrato que el de D1 (el
/// 57/57 de gpt-oss se midió con D2 presente; corregirla pasa por A/B).
#[test]
fn harmony_raw_render_still_carries_the_d2_deviation() {
    let Some(fixture) = load_fixture("harmony") else {
        return;
    };
    let date = fixture["date"].as_str().unwrap();
    let case = &fixture["cases"].as_array().unwrap()[0];
    let (req, _) = request_of(case);
    let raw = build_harmony_prompt(&req, "medium", Some(date));
    assert_ne!(
        raw,
        case["expected"].as_str().unwrap(),
        "los renders ya son idénticos: D2 fue corregida — eliminar la transformación \
         y este test (ver el doc comment del módulo)"
    );
}

/// D3: el hallazgo, fijado — braze le habla a gemma-4 en el dialecto
/// gemma2/3 mientras el artefacto declara el dialecto `<|turn>`. Este
/// test NO afirma igualdad (no la hay): afirma que la divergencia es la
/// documentada, para que un cambio en cualquiera de los dos lados
/// (nuevo GGUF con otro template, o braze adoptando el dialecto nuevo
/// tras su A/B) obligue a revisitar esta decisión en vez de derivar en
/// silencio.
#[test]
fn gemma_dialect_divergence_is_the_documented_one() {
    let Some(fixture) = load_fixture("gemma") else {
        return;
    };
    for case in fixture["cases"].as_array().unwrap() {
        let (req, system) = request_of(case);
        let expected = case["expected"].as_str().unwrap();
        let braze_render = build_gemma_prompt(&req);

        // La referencia (GGUF embebido) es el dialecto nuevo…
        assert!(
            expected.contains("<|turn>") && expected.contains("<turn|>"),
            "la referencia gemma ya no es el dialecto <|turn> — regenerar el fixture y \
             revisitar D3: {expected:?}"
        );
        // …y braze sigue en el de gemma2/3, con el system plegado al
        // primer turno user (gemma2/3 no tiene rol system).
        assert!(braze_render.starts_with("<start_of_turn>user\n"));
        assert!(!braze_render.contains("<|turn>"));
        assert!(
            braze_render.contains(&system),
            "el system debe plegarse al primer turno user"
        );
    }
}

// ---------------------------------------------------------------------
// Property tests de buena formación (sin fixture — corren siempre).
// El análogo del `render_turn_concatenation_stays_well_formed_chatml`
// de rabbit: invariantes estructurales que no dependen del oráculo.
// ---------------------------------------------------------------------

fn plain_request() -> CompletionRequest {
    CompletionRequest {
        messages: vec![
            Message::text(Role::User, "lista los archivos"),
            Message::text(Role::Assistant, "a.rs, b.rs y c.rs"),
            Message::text(Role::User, "¿cuál es más grande?"),
        ],
        tool_stubs: vec![],
        system_prompt: "You are braze.".to_string(),
        max_tokens: 256,
    }
}

#[test]
fn chatml_prompt_is_well_formed() {
    let prompt = build_chatml_prompt(&plain_request());
    assert_eq!(
        prompt.matches("<|im_start|>").count(),
        prompt.matches("<|im_end|>").count() + 1,
        "exactamente un bloque abierto (el del assistant): {prompt:?}"
    );
    assert_eq!(prompt.matches("<|im_start|>system").count(), 1);
    assert!(prompt.ends_with("<|im_start|>assistant\n"));
}

#[test]
fn gemma_prompt_is_well_formed() {
    let prompt = build_gemma_prompt(&plain_request());
    assert_eq!(
        prompt.matches("<start_of_turn>").count(),
        prompt.matches("<end_of_turn>").count() + 1,
        "exactamente un turno abierto (el del model): {prompt:?}"
    );
    assert!(!prompt.contains("<start_of_turn>system"), "gemma2/3 no tiene rol system");
    assert_eq!(
        prompt.matches("You are braze.").count(),
        1,
        "el system se pliega exactamente una vez"
    );
    assert!(prompt.ends_with("<start_of_turn>model\n"));
}

#[test]
fn harmony_prompt_is_well_formed() {
    let prompt = build_harmony_prompt(&plain_request(), "medium", Some("2026-08-15"));
    assert!(prompt.starts_with("<|start|>system<|message|>"));
    assert!(prompt.ends_with("<|start|>assistant"));
    assert_eq!(
        prompt.matches("<|start|>").count(),
        prompt.matches("<|end|>").count() + prompt.matches("<|call|>").count() + 1,
        "cada mensaje cerrado con <|end|>/<|call|> y exactamente el turno final abierto: {prompt:?}"
    );
}
