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

/// Coarse model family, inferred from the model name, used to pick a
/// proactive tool-call-syntax hint for [`default_system_prompt`] —
/// D1 (docs/AUDITORIA-2026-07-v3.md), the gap this project's own cited
/// evidence (the Qwen3-Coder technical report) says matters most for
/// small models: they overfit hard to the tool-template they were
/// trained/fine-tuned on. Before this, the *only* family-aware code in
/// `braze` was reactive — `braze-engine`'s textual rescue
/// (`extract_tagged_tool_calls`/`extract_function_xml_tool_calls`) only
/// ever runs after a model has already failed to emit a structured tool
/// call. This gives the same two formats a proactive path: a short
/// example in the system prompt, in the model's own native syntax,
/// before it generates anything.
///
/// `Generic` — everything unrecognized — adds no hint: guessing wrong
/// for an unrecognized model risks nudging it toward a syntax it was
/// never trained on. That includes Gemma, deliberately (I-4,
/// docs/AUDITORIA-2026-07-v6.md): no leak grammar has been observed for
/// it in any live session (U-20 was a capability failure, not a template
/// leak), so there's nothing evidence-based to hint.
///
/// Note the family inference is name-based, NOT backend-based — an
/// OpenRouter-served Qwen or GLM overfits to its native template exactly
/// like an Ollama-served one does (the GLM leak U-15/U-16 was observed
/// *via OpenRouter*); which backend transports the tokens doesn't change
/// what the weights were trained on. The old backend gating lived at the
/// call sites and is gone (I-4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelFamily {
    /// qwen2.5/Hermes-style tagged JSON:
    /// `<tool_call>{"name":...,"arguments":{...}}</tool_call>`.
    QwenTagged,
    /// qwen3-coder's bare XML grammar (designed to avoid JSON escaping in
    /// code-carrying arguments):
    /// `<function=name><parameter=key>value</parameter></function>`.
    Qwen3CoderXml,
    /// GLM's arg-tag grammar, observed live leaking as text via
    /// OpenRouter (`z-ai/glm-5.2`, U-15/U-16,
    /// docs/usability-log-2026-07-07-si2.md — braze-engine carries a
    /// dedicated rescue rung for it, `parse_glm_arg_tag_tool_call`):
    /// `tool_name<arg_key>key</arg_key><arg_value>value</arg_value>`.
    GlmArgTags,
    Generic,
}

impl ModelFamily {
    /// Ollama naming convention: `family[:tag]` (e.g. `"qwen2.5:3b"`,
    /// `"qwen3.5-coder:latest"`). Checked in most-specific-first order —
    /// `"qwen3-coder"`/`"qwen3.5-coder"` before the bare `"qwen"`
    /// substring both would otherwise match.
    fn from_model_name(name: &str) -> Self {
        let name = name.to_lowercase();
        if name.contains("qwen3-coder") || name.contains("qwen3.5-coder") {
            ModelFamily::Qwen3CoderXml
        } else if name.contains("qwen") {
            ModelFamily::QwenTagged
        } else if name.contains("glm") {
            ModelFamily::GlmArgTags
        } else {
            ModelFamily::Generic
        }
    }

    /// A one-line, proactive example of this family's native tool-call
    /// syntax, phrased as a fallback ("if your tool-calling template
    /// isn't active") so it's inert noise for the common case where the
    /// backend's structured `tool_calls` mechanism already works, and a
    /// concrete example for the leak-mode case the rescue exists for —
    /// deliberately short given small models' weak, non-monotonic ICL
    /// (docs/SOTA-2026-07.md's SLM survey adenda: few-shot content in the
    /// prompt can *hurt* small models, and every token here also competes
    /// with the tiny prompt budget Grupo P just calibrated for
    /// `num_ctx`-constrained backends).
    fn tool_call_hint(self) -> Option<&'static str> {
        match self {
            ModelFamily::QwenTagged => Some(
                "If your tool-calling template isn't active, emit a call as \
                 <tool_call>{\"name\": \"tool_name\", \"arguments\": {...}}</tool_call> \
                 instead of describing it in prose.",
            ),
            ModelFamily::Qwen3CoderXml => Some(
                "If your tool-calling template isn't active, emit a call as \
                 <function=tool_name><parameter=arg_name>value</parameter></function> \
                 instead of describing it in prose (the parser tolerates the \
                 grammar's usual newlines between tags too).",
            ),
            ModelFamily::GlmArgTags => Some(
                "If your tool-calling template isn't active, emit a call as \
                 tool_name<arg_key>arg_name</arg_key><arg_value>value</arg_value> \
                 instead of describing it in prose.",
            ),
            ModelFamily::Generic => None,
        }
    }
}

/// Default system prompt, used unless `Config::system_prompt` overrides
/// it. `model_name` (Ollama's `family[:tag]` naming; `None` for
/// Anthropic/OpenRouter or when no specific model is pinned) picks an
/// optional family-specific tool-call hint — see [`ModelFamily`].
/// `environment` (E′ I.6, docs/harness-engineering-hooks-skills-2026-07-10.md)
/// is a pre-built snapshot of the session's surroundings (git branch/status,
/// date, OS) appended verbatim when `Some` — generated by the COMPOSITION
/// ROOT, not here: the binary knows how to run git; this library only
/// formats. `None` (braze-bench always, braze-cli unless
/// `Config::environment_block`) appends nothing.
/// `references` (opencode-10, docs/opencode-a-braze.md § 10) advertises
/// the configured external reference directories that carry a
/// `description` — "un SLM no sabe dónde buscar"; a directory it was
/// never told about might as well not exist. Description-less entries
/// are allowlisted but not advertised (see [`crate::ReferenceConfig`]);
/// callers with no references (braze-bench's sandbox) pass `&[]`.
/// `project_memory` (docs/project-memory-design.md) is a pre-rendered
/// cross-session summary — `braze_memory::render_project_memory_section`
/// already applied its own token budget, this library only formats it
/// verbatim, same posture as `environment`. `None` (braze-bench always;
/// braze-cli unless `Config::enable_project_memory`) appends nothing —
/// off by default, same posture as the typed task list.
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
pub fn default_system_prompt(
    cwd: &Path,
    model_name: Option<&str>,
    references: &[crate::ReferenceConfig],
    environment: Option<&str>,
    project_memory: Option<&str>,
) -> String {
    let family_hint = model_name
        .map(ModelFamily::from_model_name)
        .and_then(ModelFamily::tool_call_hint)
        .map(|hint| format!("\n- {hint}"))
        .unwrap_or_default();

    let environment_section = match environment {
        Some(snapshot) if !snapshot.trim().is_empty() => {
            format!("\n\nEnvironment:\n{}", snapshot.trim_end())
        }
        _ => String::new(),
    };

    let project_memory_section = match project_memory {
        Some(section) if !section.trim().is_empty() => {
            format!("\n\nProject memory (from earlier sessions):\n{}", section.trim_end())
        }
        _ => String::new(),
    };

    let described: Vec<&crate::ReferenceConfig> = references
        .iter()
        .filter(|reference| reference.description.is_some())
        .collect();
    let references_section = if described.is_empty() {
        String::new()
    } else {
        let mut section =
            String::from("\n\nReference directories (outside the working directory, also allowed):\n");
        for reference in described {
            section.push_str(&format!(
                "- {}: {}\n",
                reference.path.display(),
                reference.description.as_deref().unwrap_or_default()
            ));
        }
        // The trailing newline would dangle at the very end of the
        // prompt; the section reads as a block either way.
        section.pop();
        section
    };

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
         - Relative paths are resolved against the working directory above.\n\
         - Old tool results may appear collapsed to one line to save space — \
         re-run the tool if you need their full content.{family_hint}{references_section}{environment_section}{project_memory_section}",
        cwd.display()
    )
}

/// Floor under the dynamic margin below, for a tiny/empty system prompt
/// with no tools configured — a defensive minimum, not the primary
/// mechanism (see [`ollama_context_budget_tokens`]).
const MIN_CONTEXT_BUDGET_MARGIN_TOKENS: u32 = 256;

/// Chars-per-token divisor for the prompt-side margin only (system prompt
/// text + tool schema JSON) — denser than the ~4 chars/token
/// `braze-engine`'s event-text estimator uses for natural-language
/// content, since tool schemas are JSON, which tokenizes more tightly.
/// Deliberately conservative (a smaller divisor rounds the margin up):
/// underestimating this margin is the dangerous direction — a silent
/// overflow past `num_ctx` — not overestimating it
/// (docs/AUDITORIA-2026-07-v3.md, hallazgo B4).
const PROMPT_SIDE_CHARS_PER_TOKEN: u32 = 3;

/// The token budget a real `braze` invocation passes to
/// `Engine::with_context_budget` when the active backend is Ollama (the
/// only backend with a small, fixed context window worth budgeting for —
/// Anthropic's/OpenRouter's underlying models are large enough that raw
/// event count remains a fine proxy on their own). Saturates rather than
/// underflows if `max_tokens` and the margin together exceed `num_ctx`.
///
/// The margin reserved for the system prompt + tool definitions (neither
/// is part of what `Engine::load_messages`'s token estimate measures) used
/// to be a fixed constant — but a fixed margin can't grow with the number
/// of MCP tools configured, so enough of them push the real prompt past
/// `num_ctx` while this budget still reports "under"
/// (docs/AUDITORIA-2026-07-v3.md, hallazgo B4). `system_prompt` and
/// `tool_definitions_bytes` (see `braze_tools_core::tool_stub_definition_bytes`
/// for the latter — this crate doesn't depend on `braze-tools-core`, so
/// the caller sums it and passes a plain byte count) size the margin from
/// what's actually being sent, with [`MIN_CONTEXT_BUDGET_MARGIN_TOKENS`]
/// as a floor for the degenerate empty case.
pub fn ollama_context_budget_tokens(
    num_ctx: u32,
    max_tokens: u32,
    system_prompt: &str,
    tool_definitions_bytes: usize,
) -> u32 {
    let prompt_side_chars = system_prompt.len().saturating_add(tool_definitions_bytes);
    let margin = ((prompt_side_chars as u32) / PROMPT_SIDE_CHARS_PER_TOKEN)
        .max(MIN_CONTEXT_BUDGET_MARGIN_TOKENS);
    num_ctx.saturating_sub(max_tokens).saturating_sub(margin)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_system_prompt_includes_cwd_and_anti_loop_guidance() {
        let prompt = default_system_prompt(Path::new("/home/user/project"), None, &[], None, None);
        assert!(prompt.contains("/home/user/project"));
        assert!(prompt.contains("Never call the same tool"));
    }

    #[test]
    fn default_system_prompt_tells_the_model_to_act_not_just_narrate() {
        let prompt = default_system_prompt(Path::new("/home/user/project"), None, &[], None, None);
        assert!(prompt.contains("call the tool for it in the same turn"));
    }

    // --- D1 (docs/AUDITORIA-2026-07-v3.md): family-specific tool-call hint ---

    #[test]
    fn no_model_name_gets_no_family_hint() {
        let prompt = default_system_prompt(Path::new("/p"), None, &[], None, None);
        assert!(!prompt.contains("tool_call"));
        assert!(!prompt.contains("<function="));
    }

    #[test]
    fn an_unrecognized_model_name_gets_no_family_hint() {
        let prompt = default_system_prompt(Path::new("/p"), Some("llama3.1"), &[], None, None);
        assert!(!prompt.contains("tool_call"));
        assert!(!prompt.contains("<function="));
    }

    #[test]
    fn a_qwen2_model_gets_the_tagged_json_hint() {
        let prompt = default_system_prompt(Path::new("/p"), Some("qwen2.5:3b"), &[], None, None);
        assert!(prompt.contains("<tool_call>"));
        assert!(!prompt.contains("<function="));
    }

    #[test]
    fn a_qwen3_coder_model_gets_the_xml_hint_not_the_tagged_one() {
        let prompt = default_system_prompt(Path::new("/p"), Some("qwen3.5-coder:latest"), &[], None, None);
        assert!(prompt.contains("<function="));
        assert!(!prompt.contains("<tool_call>"));
    }

    #[test]
    fn model_family_matching_is_case_insensitive() {
        let prompt = default_system_prompt(Path::new("/p"), Some("QWEN2.5:3B"), &[], None, None);
        assert!(prompt.contains("<tool_call>"));
    }

    /// I-4 (docs/AUDITORIA-2026-07-v6.md): GLM — whose template leak was
    /// observed live via OpenRouter (U-15/U-16) — gets its arg-tag
    /// grammar as a proactive hint, matching the reactive rescue rung
    /// braze-engine already carries for it. The OpenRouter-style name is
    /// the one that matters: the old backend gating withheld the hint
    /// exactly there.
    #[test]
    fn a_glm_model_gets_the_arg_tag_hint_including_openrouter_style_names() {
        let prompt = default_system_prompt(Path::new("/p"), Some("z-ai/glm-5.2"), &[], None, None);
        assert!(prompt.contains("<arg_key>"), "got: {prompt}");
        assert!(prompt.contains("<arg_value>"));
        assert!(!prompt.contains("<tool_call>"));
        assert!(!prompt.contains("<function="));
    }

    /// Gemma stays Generic deliberately — no leak grammar has been
    /// observed for it in any live session (U-20 was a capability
    /// failure, not a template leak), and hinting a guessed syntax risks
    /// nudging the model toward something it was never trained on.
    #[test]
    fn a_gemma_model_gets_no_hint_no_observed_leak_grammar() {
        let prompt = default_system_prompt(Path::new("/p"), Some("gemma4:e4b"), &[], None, None);
        assert!(!prompt.contains("<arg_key>"));
        assert!(!prompt.contains("<tool_call>"));
        assert!(!prompt.contains("<function="));
    }

    /// opencode-10 (docs/opencode-a-braze.md § 10): references with a
    /// description are advertised in the system prompt; description-less
    /// ones are allowlisted but NOT advertised (braze's equivalent of
    /// OpenCode's `hidden`), and no references at all means no section.
    #[test]
    fn references_with_descriptions_are_advertised_and_bare_ones_are_not() {
        let references = vec![
            crate::ReferenceConfig {
                path: std::path::PathBuf::from("/home/user/api-docs"),
                description: Some("API reference docs for this project".to_string()),
            },
            crate::ReferenceConfig {
                path: std::path::PathBuf::from("/home/user/scratch"),
                description: None,
            },
        ];
        let prompt = default_system_prompt(Path::new("/p"), None, &references, None, None);
        assert!(prompt.contains("Reference directories"), "got: {prompt}");
        assert!(prompt.contains("/home/user/api-docs: API reference docs"));
        assert!(!prompt.contains("/home/user/scratch"));

        let without = default_system_prompt(Path::new("/p"), None, &[], None, None);
        assert!(!without.contains("Reference directories"));
    }

    /// E′ I.6: a `Some` snapshot lands verbatim under an "Environment:"
    /// header; `None` (bench, and production default) appends nothing.
    #[test]
    fn an_environment_snapshot_is_appended_verbatim_when_provided() {
        let with = default_system_prompt(
            Path::new("/p"),
            None,
            &[],
            Some("- date: 2026-07-10\n- git branch: main"),
            None,
        );
        assert!(with.contains("Environment:\n- date: 2026-07-10"), "got: {with}");
        assert!(with.contains("- git branch: main"));

        let without = default_system_prompt(Path::new("/p"), None, &[], None, None);
        assert!(!without.contains("Environment:"));

        let blank = default_system_prompt(Path::new("/p"), None, &[], Some("   "), None);
        assert!(!blank.contains("Environment:"), "blank snapshot adds nothing");
    }

    /// `project_memory` mirrors `environment`'s contract exactly: `Some`
    /// lands verbatim under its own heading, blank/`None` appends
    /// nothing, and it's independent of whether `environment` is also
    /// set (docs/project-memory-design.md).
    #[test]
    fn a_project_memory_section_is_appended_verbatim_when_provided() {
        let with = default_system_prompt(
            Path::new("/p"),
            None,
            &[],
            None,
            Some("Objective: build the CLI\n- src/main.rs (write_file)"),
        );
        assert!(
            with.contains("Project memory (from earlier sessions):\nObjective: build the CLI"),
            "got: {with}"
        );
        assert!(with.contains("- src/main.rs (write_file)"));

        let without = default_system_prompt(Path::new("/p"), None, &[], None, None);
        assert!(!without.contains("Project memory"));

        let blank = default_system_prompt(Path::new("/p"), None, &[], None, Some("   "));
        assert!(!blank.contains("Project memory"), "blank section adds nothing");
    }

    #[test]
    fn model_family_from_name_prioritizes_qwen3_coder_over_bare_qwen() {
        assert_eq!(
            ModelFamily::from_model_name("qwen3-coder:30b"),
            ModelFamily::Qwen3CoderXml
        );
        assert_eq!(
            ModelFamily::from_model_name("qwen2.5:3b"),
            ModelFamily::QwenTagged
        );
        assert_eq!(
            ModelFamily::from_model_name("llama3.2:3b"),
            ModelFamily::Generic
        );
    }

    #[test]
    fn ollama_context_budget_uses_the_floor_margin_for_a_tiny_prompt_and_no_tools() {
        assert_eq!(
            ollama_context_budget_tokens(8192, 4096, "", 0),
            8192 - 4096 - MIN_CONTEXT_BUDGET_MARGIN_TOKENS
        );
    }

    #[test]
    fn ollama_context_budget_margin_grows_with_the_system_prompt_and_tool_definitions() {
        let system_prompt = "x".repeat(900);
        let tool_definitions_bytes = 300;
        // prompt_side_chars = 1200, margin = 1200 / 3 = 400 (above the floor).
        assert_eq!(
            ollama_context_budget_tokens(8192, 4096, &system_prompt, tool_definitions_bytes),
            8192 - 4096 - 400
        );
    }

    #[test]
    fn ollama_context_budget_shrinks_as_more_mcp_tools_are_configured() {
        let system_prompt = "some real system prompt text";
        let few_tools = ollama_context_budget_tokens(8192, 4096, system_prompt, 200);
        let many_tools = ollama_context_budget_tokens(8192, 4096, system_prompt, 10_000);
        assert!(
            many_tools < few_tools,
            "more tool-definition bytes must shrink the budget, not leave it flat"
        );
    }

    #[test]
    fn ollama_context_budget_saturates_instead_of_underflowing() {
        assert_eq!(ollama_context_budget_tokens(100, 1000, "", 0), 0);
    }
}
