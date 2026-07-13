//! Renders a [`ProjectMemory`] into the system-prompt section a
//! composition root passes as `default_system_prompt`'s
//! `project_memory` parameter — the same shape as that function's
//! existing `environment` parameter (a pre-built snapshot string the
//! library only formats verbatim), not a new injection mechanism.
//!
//! Token-budgeted like every other prompt-side cost in this workspace
//! (`ollama_context_budget_tokens`'s own doc comment): a machine-written,
//! ever-growing file must never be allowed to silently eat an unbounded
//! share of a small model's context window. Truncates whole-line, never
//! mid-line — same principle as `extract_next_ndjson_line`'s "no
//! complete line yet" framing elsewhere in this workspace.

use crate::memory::ProjectMemory;

/// ~4 chars/token — the same coarse estimator
/// `braze_engine::estimate_dropped_tokens` already uses for natural-language
/// content (not the denser 3-chars/token this workspace reserves for
/// JSON specifically, since this section is prose/paths, not schemas).
const CHARS_PER_TOKEN: usize = 4;

/// A conservative default budget for the injected section — a fraction
/// of a typical small local model's context window, not the whole
/// margin `ollama_context_budget_tokens` reserves (that margin already
/// covers this section once it's part of the system prompt the caller
/// measures).
pub const DEFAULT_PROJECT_MEMORY_BUDGET_TOKENS: usize = 400;

/// Renders `memory` into a section body (no heading — the caller's
/// `default_system_prompt` supplies `"Project memory:\n"`), or `None`
/// if there's nothing worth injecting (a brand-new project, or
/// everything trimmed away by an unreasonably small budget). Lines are
/// ordered most-recent-first within each group and dropped once the
/// budget is spent — never truncated mid-line.
pub fn render_project_memory_section(
    memory: &ProjectMemory,
    budget_tokens: usize,
) -> Option<String> {
    if memory.objective.is_none()
        && memory.notes.is_none()
        && memory.touched_files.is_empty()
        && memory.completed_signals.is_empty()
    {
        return None;
    }

    let budget_chars = budget_tokens.saturating_mul(CHARS_PER_TOKEN);
    let mut out = String::new();
    let mut used = 0usize;

    if let Some(objective) = &memory.objective {
        push_line(&mut out, &mut used, budget_chars, &format!("Objective: {objective}"));
    }
    if let Some(notes) = &memory.notes {
        push_line(&mut out, &mut used, budget_chars, &format!("Notes: {notes}"));
    }

    if !memory.completed_signals.is_empty() {
        push_line(&mut out, &mut used, budget_chars, "Completed in earlier sessions:");
        for signal in memory.completed_signals.iter().rev() {
            if !push_line(
                &mut out,
                &mut used,
                budget_chars,
                &format!("- {}", signal.description),
            ) {
                break;
            }
        }
    }

    if !memory.touched_files.is_empty() {
        push_line(&mut out, &mut used, budget_chars, "Files touched in earlier sessions:");
        for file in memory.touched_files.iter().rev() {
            if !push_line(
                &mut out,
                &mut used,
                budget_chars,
                &format!("- {} ({})", file.path, file.last_tool),
            ) {
                break;
            }
        }
    }

    let trimmed = out.trim_end();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Appends `line` plus a trailing newline to `out` if doing so stays
/// within `budget_chars`; returns whether it fit. A line that alone
/// would exceed the WHOLE budget (pathological, but not impossible with
/// a tiny budget) also returns `false` rather than pushing a partial —
/// truncating mid-line would render broken prose to the model.
fn push_line(out: &mut String, used: &mut usize, budget_chars: usize, line: &str) -> bool {
    let len = line.len() + 1;
    if *used + len > budget_chars {
        return false;
    }
    out.push_str(line);
    out.push('\n');
    *used += len;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::SignalSource;

    #[test]
    fn a_brand_new_memory_renders_nothing() {
        let memory = ProjectMemory::new("proj");
        assert_eq!(render_project_memory_section(&memory, 400), None);
    }

    #[test]
    fn renders_touched_files_most_recent_first() {
        let mut memory = ProjectMemory::new("proj");
        memory.record_touched_file("a.rs", "write_file", "t1");
        memory.record_touched_file("b.rs", "edit_file", "t2");

        let section = render_project_memory_section(&memory, 400).unwrap();
        let a_pos = section.find("a.rs").unwrap();
        let b_pos = section.find("b.rs").unwrap();
        assert!(b_pos < a_pos, "most recently touched file must render first");
    }

    #[test]
    fn renders_completed_signals_and_objective() {
        let mut memory = ProjectMemory::new("proj");
        memory.objective = Some("build the CLI".to_string());
        memory.record_completed_signal("wrote parser.py", SignalSource::TaskListCompletion, "t1");

        let section = render_project_memory_section(&memory, 400).unwrap();
        assert!(section.contains("Objective: build the CLI"));
        assert!(section.contains("wrote parser.py"));
    }

    /// The core guarantee: a tiny budget truncates whole lines, never
    /// mid-line — a partial line would be broken prose fed to the model.
    #[test]
    fn a_small_budget_drops_whole_lines_never_mid_line() {
        let mut memory = ProjectMemory::new("proj");
        for i in 0..20 {
            memory.record_touched_file(format!("file_{i}_with_a_longer_name.rs"), "write_file", "t");
        }

        // Budget tight enough that not everything fits, generous enough
        // that at least the heading + one entry does.
        let section = render_project_memory_section(&memory, 20).unwrap();
        for line in section.lines() {
            assert!(
                line.ends_with(')') || line == "Files touched in earlier sessions:",
                "no line should be cut mid-word: {line:?}"
            );
        }
        assert!(
            section.lines().count() < 21,
            "a tight budget must drop some of the 20 entries, not fit them all"
        );
    }

    #[test]
    fn a_zero_budget_renders_nothing_even_with_content() {
        let mut memory = ProjectMemory::new("proj");
        memory.record_touched_file("a.rs", "write_file", "t1");
        assert_eq!(render_project_memory_section(&memory, 0), None);
    }
}
