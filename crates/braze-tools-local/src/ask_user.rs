//! `ask_user` — clarificación estructurada de opción múltiple (E′ I.5,
//! docs/harness-engineering-hooks-skills-2026-07-10.md).
//!
//! Un [`ToolProvider`] propio (no parte de `LocalToolsProvider`) que
//! expone una sola tool, `ask_user`, y bloquea en un
//! [`QuestionPrompt`](braze_permissions::QuestionPrompt) inyectado para
//! traer la respuesta del humano. Al ser un provider aparte, se compone
//! en el `ToolRegistry` SOLO en sesiones interactivas — el bench y `run`
//! nunca lo agregan, así que la tool ni siquiera aparece en el
//! inventario ahí (no hay a quién preguntar sin un humano al teclado).

use std::sync::Arc;

use async_trait::async_trait;
use braze_permissions::QuestionPrompt;
use braze_tools_core::{ToolError, ToolProvider, ToolSchema};
use braze_types::{ToolCall, ToolResult, ToolStub};
use serde::Deserialize;

/// Límites de opciones: menos de 2 no es una elección, más de 4 es un
/// menú que un modelo chico arma mal y un humano lee peor — el mismo
/// rango que la guía del estudio (2..=4).
const MIN_OPTIONS: usize = 2;
const MAX_OPTIONS: usize = 4;

pub const ASK_USER_TOOL: &str = "ask_user";

#[derive(Debug, Deserialize)]
struct AskUserArgs {
    question: String,
    options: Vec<String>,
}

/// El provider de `ask_user`. Sostiene el canal al humano.
pub struct AskUserProvider {
    prompt: Arc<dyn QuestionPrompt>,
}

impl AskUserProvider {
    pub fn new(prompt: Arc<dyn QuestionPrompt>) -> Self {
        Self { prompt }
    }

    fn stub() -> ToolStub {
        ToolStub {
            name: ASK_USER_TOOL.to_string(),
            summary: "Ask the user a multiple-choice question when a genuine decision is \
                      theirs to make and you cannot resolve it from the request or context. \
                      Prefer a sensible default over asking; reserve this for real branch \
                      points (e.g. which of two matching files to edit)."
                .to_string(),
            source: "harness".to_string(),
            input_schema: Some(schema()),
        }
    }
}

fn schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "question": {
                "type": "string",
                "description": "The decision to put to the user, phrased as a question."
            },
            "options": {
                "type": "array",
                "items": { "type": "string" },
                "minItems": MIN_OPTIONS,
                "maxItems": MAX_OPTIONS,
                "description": "2 to 4 distinct choices for the user to pick from."
            }
        },
        "required": ["question", "options"],
        "additionalProperties": false
    })
}

#[async_trait]
impl ToolProvider for AskUserProvider {
    fn provider_id(&self) -> &str {
        "harness:ask_user"
    }

    async fn list_stubs(&self) -> Result<Vec<ToolStub>, ToolError> {
        Ok(vec![Self::stub()])
    }

    async fn resolve_schema(&self, name: &str) -> Result<Option<ToolSchema>, ToolError> {
        if name == ASK_USER_TOOL {
            Ok(Some(ToolSchema {
                name: ASK_USER_TOOL.to_string(),
                description: Self::stub().summary,
                input_schema: schema(),
            }))
        } else {
            Ok(None)
        }
    }

    async fn invoke(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
        let args: AskUserArgs = serde_json::from_value(call.arguments.clone()).map_err(|err| {
            ToolError::InvocationFailed {
                name: ASK_USER_TOOL.to_string(),
                message: format!("invalid ask_user arguments: {err}"),
            }
        })?;

        // Option-count validation is a RECOVERABLE tool error (the model
        // can retry with a fixed call), not a hard InvocationFailed.
        if args.options.len() < MIN_OPTIONS || args.options.len() > MAX_OPTIONS {
            return Ok(ToolResult {
                tool_call_id: call.id.clone(),
                content: format!(
                    "ask_user needs between {MIN_OPTIONS} and {MAX_OPTIONS} options; got {}. \
                     Re-ask with a valid number of choices, or proceed with a sensible default.",
                    args.options.len()
                ),
                is_error: true,
            });
        }

        let content = match self.prompt.ask(&args.question, &args.options).await {
            Some(index) => format!(
                "The user chose: {}",
                args.options
                    .get(index)
                    .map(String::as_str)
                    .unwrap_or("(unknown)")
            ),
            None => "The user did not answer. Proceed with your best judgment or a safe \
                     default."
                .to_string(),
        };
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

    /// A prompt that always returns a fixed choice — the injectable
    /// stand-in a real front-end (stdin/TUI) replaces.
    struct FixedChoice(Option<usize>);

    #[async_trait]
    impl QuestionPrompt for FixedChoice {
        async fn ask(&self, _question: &str, _options: &[String]) -> Option<usize> {
            self.0
        }
    }

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "c1".to_string(),
            name: ASK_USER_TOOL.to_string(),
            arguments: args,
        }
    }

    #[tokio::test]
    async fn a_valid_question_returns_the_chosen_option_text() {
        let provider = AskUserProvider::new(Arc::new(FixedChoice(Some(1))));
        let result = provider
            .invoke(&call(serde_json::json!({
                "question": "¿Cuál archivo edito?",
                "options": ["config.toml", "config.yaml"]
            })))
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(
            result.content.contains("config.yaml"),
            "got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn no_answer_tells_the_model_to_use_its_judgment() {
        let provider = AskUserProvider::new(Arc::new(FixedChoice(None)));
        let result = provider
            .invoke(&call(serde_json::json!({
                "question": "¿A o B?",
                "options": ["A", "B"]
            })))
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(
            result.content.contains("did not answer"),
            "got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn too_few_or_too_many_options_is_a_recoverable_error() {
        let provider = AskUserProvider::new(Arc::new(FixedChoice(Some(0))));
        let one = provider
            .invoke(&call(
                serde_json::json!({"question": "q", "options": ["solo"]}),
            ))
            .await
            .unwrap();
        assert!(one.is_error);
        assert!(one.content.contains("between 2 and 4"));

        let five = provider
            .invoke(&call(serde_json::json!({
                "question": "q",
                "options": ["a", "b", "c", "d", "e"]
            })))
            .await
            .unwrap();
        assert!(five.is_error);
    }

    #[tokio::test]
    async fn the_stub_is_advertised_and_the_schema_resolves() {
        let provider = AskUserProvider::new(Arc::new(FixedChoice(Some(0))));
        let stubs = provider.list_stubs().await.unwrap();
        assert_eq!(stubs.len(), 1);
        assert_eq!(stubs[0].name, ASK_USER_TOOL);
        assert!(
            provider
                .resolve_schema(ASK_USER_TOOL)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            provider
                .resolve_schema("read_file")
                .await
                .unwrap()
                .is_none()
        );
    }
}
