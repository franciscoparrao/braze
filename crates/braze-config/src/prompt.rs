//! The default system prompt and Ollama context-budget formula every
//! *real* `braze` turn is built with — shared by `braze-cli` (the
//! production binary) and `braze-bench` (which drives the same
//! `braze_engine::Engine` against canned tasks). Kept here, rather than
//! duplicated in each binary, precisely so they can't drift apart: N-36
//! (docs/AUDITORIA-2026-07-v2.md) found `braze-bench` measuring a
//! one-line system prompt with no anti-loop guidance and no context
//! budget at all, while production used both — meaning a bench pass rate
//! didn't actually say anything about what `braze chat` would do.

use std::path::Path;

/// Default system prompt, used unless `Config::system_prompt` overrides
/// it.
///
/// Earlier versions of this shipped a single generic sentence with no
/// tool-use guidance, no anti-loop rules, and no working directory — the
/// cheapest lever for small/local models left completely unused (see
/// docs/AUDITORIA-2026-07.md, hallazgo A10). The rules below target
/// dominant small-model failure modes this project has observed
/// empirically: repeating an identical tool call instead of using its
/// result; over-elaborating instead of answering once enough information
/// has been gathered (arXiv 2604.02155's finding that longer reasoning
/// *degrades* tool-calling accuracy in small models); and narrating an
/// intended action instead of actually calling the tool for it — observed
/// live against `qwen2.5:3b` via `braze chat --tui` (2026-07-05): asked to
/// save a file, it kept restating the plan ("Voy a proceder con estos
/// pasos ahora") across several turns, even after explicit confirmation,
/// without ever emitting the `write_file` call.
pub fn default_system_prompt(cwd: &Path) -> String {
    format!(
        "You are braze, an agentic CLI assistant. Working directory: {}.\n\
         \n\
         Rules:\n\
         - Never call the same tool with the same arguments twice in one turn — \
         if you already have that result, use it instead of calling again.\n\
         - Once you have enough information to answer, stop calling tools and \
         answer in plain text. Do not keep exploring after you already have \
         what was asked.\n\
         - Keep reasoning brief before acting — a sentence or two, not an \
         extended chain of thought.\n\
         - When the user asks you to perform an action (write a file, run a \
         command, edit something), call the tool for it in the same turn. \
         Do not just describe or restate the plan — an action you only \
         narrate never actually happens.\n\
         - Relative paths are resolved against the working directory above.",
        cwd.display()
    )
}

/// Headroom reserved out of `ollama_num_ctx` for the system prompt + tool
/// schemas when computing `Engine::with_context_budget` — neither is part
/// of what `Engine::load_messages`'s token estimate measures (durable
/// summary + durable/tactical events only).
const CONTEXT_BUDGET_MARGIN_TOKENS: u32 = 1024;

/// The token budget a real `braze` invocation passes to
/// `Engine::with_context_budget` when the active backend is Ollama (the
/// only backend with a small, fixed context window worth budgeting for —
/// Anthropic's/OpenRouter's underlying models are large enough that raw
/// event count remains a fine proxy on their own). Saturates rather than
/// underflows if `max_tokens` and the margin together exceed `num_ctx`.
pub fn ollama_context_budget_tokens(num_ctx: u32, max_tokens: u32) -> u32 {
    num_ctx
        .saturating_sub(max_tokens)
        .saturating_sub(CONTEXT_BUDGET_MARGIN_TOKENS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_system_prompt_includes_cwd_and_anti_loop_guidance() {
        let prompt = default_system_prompt(Path::new("/home/user/project"));
        assert!(prompt.contains("/home/user/project"));
        assert!(prompt.contains("Never call the same tool"));
    }

    #[test]
    fn default_system_prompt_tells_the_model_to_act_not_just_narrate() {
        let prompt = default_system_prompt(Path::new("/home/user/project"));
        assert!(prompt.contains("call the tool for it in the same turn"));
    }

    #[test]
    fn ollama_context_budget_reserves_max_tokens_and_margin() {
        assert_eq!(ollama_context_budget_tokens(8192, 4096), 8192 - 4096 - 1024);
    }

    #[test]
    fn ollama_context_budget_saturates_instead_of_underflowing() {
        assert_eq!(ollama_context_budget_tokens(100, 1000), 0);
    }
}
