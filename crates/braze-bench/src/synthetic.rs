//! Provider sintético de herramientas OBJETIVO — el fixture del ancla
//! externa BFCL (docs/bfcl-anchor-design-2026-07-18.md).
//!
//! A diferencia de `noise.rs` (catálogo inerte de distractores que
//! ninguna tarea necesita), estas tools las declara cada tarea con su
//! schema real y son el objetivo de la aserción `expect_tool_call`:
//! el modelo DEBE encontrarlas y llamarlas. El resultado es enlatado
//! (`result`, default un ack neutro) porque BFCL califica el AST de la
//! llamada, no una ejecución real — el grading fino de argumentos
//! contra `possible_answer` ocurre offline sobre transcripciones
//! preservadas (`BRAZE_BENCH_KEEP_SESSIONS`), no acá.
//!
//! El schema viaja como STRING JSON (`parameters_json`) y no como tabla
//! TOML anidada: garantiza fidelidad byte-a-byte con el JSON Schema de
//! BFCL (una tabla TOML re-serializada puede reordenar/retipar) y
//! simplifica el conversor.

use async_trait::async_trait;
use braze_tools_core::{ToolError, ToolProvider, ToolSchema};
use braze_types::{ToolCall, ToolResult, ToolStub};
use serde::Deserialize;

/// Una tool objetivo declarada por la tarea en el TOML del suite.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SyntheticToolDef {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// JSON Schema del input, como string JSON (ver nota del módulo).
    /// Ausente ⇒ `{"type": "object"}`.
    #[serde(default)]
    pub parameters_json: Option<String>,
    /// Resultado enlatado que devuelve `invoke`. Ausente ⇒ ack neutro.
    #[serde(default)]
    pub result: Option<String>,
}

impl SyntheticToolDef {
    fn schema(&self) -> serde_json::Value {
        self.parameters_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_else(|| serde_json::json!({ "type": "object" }))
    }

    /// Primera línea de la descripción — el resumen que viaja en el stub
    /// (mismo rol que el summary de una tool real bajo carga diferida).
    fn summary(&self) -> String {
        self.description
            .lines()
            .next()
            .unwrap_or_default()
            .to_string()
    }
}

/// El provider: las tools objetivo de UNA tarea, tal cual las declaró.
pub struct SyntheticToolsProvider {
    tools: Vec<SyntheticToolDef>,
}

impl SyntheticToolsProvider {
    pub fn new(tools: Vec<SyntheticToolDef>) -> Self {
        Self { tools }
    }

    fn find(&self, name: &str) -> Option<&SyntheticToolDef> {
        self.tools.iter().find(|t| t.name == name)
    }
}

#[async_trait]
impl ToolProvider for SyntheticToolsProvider {
    fn provider_id(&self) -> &str {
        "bench:synthetic"
    }

    async fn list_stubs(&self) -> Result<Vec<ToolStub>, ToolError> {
        Ok(self
            .tools
            .iter()
            .map(|t| ToolStub {
                name: t.name.clone(),
                summary: t.summary(),
                source: "bench:synthetic".to_string(),
                input_schema: Some(t.schema()),
            })
            .collect())
    }

    async fn resolve_schema(&self, name: &str) -> Result<Option<ToolSchema>, ToolError> {
        Ok(self.find(name).map(|t| ToolSchema {
            name: t.name.clone(),
            description: t.description.clone(),
            input_schema: t.schema(),
        }))
    }

    async fn invoke(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
        let content = self
            .find(&call.name)
            .and_then(|t| t.result.clone())
            .unwrap_or_else(|| "ok (synthetic tool executed)".to_string());
        Ok(ToolResult {
            tool_call_id: call.id.clone(),
            content,
            is_error: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(name: &str, params: Option<&str>) -> SyntheticToolDef {
        SyntheticToolDef {
            name: name.to_string(),
            description: "Calculate the area of a triangle.\nSecond line.".to_string(),
            parameters_json: params.map(|s| s.to_string()),
            result: Some("42.0".to_string()),
        }
    }

    #[tokio::test]
    async fn stubs_carry_declared_schema_and_first_line_summary() {
        let params =
            r#"{"type":"object","properties":{"base":{"type":"integer"}},"required":["base"]}"#;
        let provider =
            SyntheticToolsProvider::new(vec![def("calculate_triangle_area", Some(params))]);
        let stubs = provider.list_stubs().await.unwrap();
        assert_eq!(stubs.len(), 1);
        assert_eq!(stubs[0].name, "calculate_triangle_area");
        assert_eq!(stubs[0].summary, "Calculate the area of a triangle.");
        let schema = stubs[0].input_schema.as_ref().unwrap();
        assert_eq!(schema["required"][0], "base");
        let resolved = provider
            .resolve_schema("calculate_triangle_area")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            resolved.input_schema["properties"]["base"]["type"],
            "integer"
        );
    }

    #[tokio::test]
    async fn invoke_returns_canned_result_and_unknown_names_stay_unclaimed() {
        let provider = SyntheticToolsProvider::new(vec![def("t", None)]);
        assert!(
            provider
                .resolve_schema("read_file")
                .await
                .unwrap()
                .is_none()
        );
        let call = ToolCall {
            id: "c1".to_string(),
            name: "t".to_string(),
            arguments: serde_json::json!({}),
        };
        let result = provider.invoke(&call).await.unwrap();
        assert_eq!(result.content, "42.0");
        assert!(!result.is_error);
    }

    #[test]
    fn malformed_parameters_json_falls_back_to_open_object() {
        let d = def("t", Some("not json"));
        assert_eq!(d.schema(), serde_json::json!({"type": "object"}));
    }
}
