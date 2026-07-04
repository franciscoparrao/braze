use crate::action::ActionDescriptor;
use crate::allowlist::WorkdirAllowlist;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reversibility {
    Reversible,
    Irreversible,
}

impl Reversibility {
    pub fn is_reversible(self) -> bool {
        self == Self::Reversible
    }
}

/// Sync, pure computation — no I/O, so no async-trait here.
pub trait ActionClassifier: Send + Sync {
    fn classify(&self, action: &ActionDescriptor) -> Reversibility;
}

/// WriteFile/DeleteFile: Reversible inside the WorkdirAllowlist, else
/// Irreversible. A DeleteFile INSIDE the allowlist is Reversible for MVP
/// (PLAN.md's table only names "escrituras fuera del cwd", not deletes in
/// scope — do not silently expand beyond spec).
///
/// ShellCommand: default-deny. `git push`/`rm -rf` (and flag-order
/// variants) are always Irreversible; otherwise a command is Reversible
/// only if it matches the explicit `is_safe_shell_command` allowlist
/// (read-only/introspection commands, plus a narrow, non-mutating subset of
/// `find`/`git`). Everything else — `mv`, `dd`, `curl`, `chmod -R`, a bare
/// `rm` with no flags, any unrecognized program — is Irreversible. This
/// replaced an earlier "allow by default, deny two patterns" table that
/// left most destructive/networked commands unconfirmed.
///
/// McpToolCall: always Irreversible — an MCP server is arbitrary code the
/// user chose to wire up, with no safe-by-construction subset to allowlist.
pub struct DefaultClassifier {
    allowlist: WorkdirAllowlist,
}

impl DefaultClassifier {
    pub fn new(allowlist: WorkdirAllowlist) -> Self {
        Self { allowlist }
    }
}

impl ActionClassifier for DefaultClassifier {
    fn classify(&self, action: &ActionDescriptor) -> Reversibility {
        match action {
            ActionDescriptor::WriteFile { path } | ActionDescriptor::DeleteFile { path } => {
                if self.allowlist.is_allowed(path) {
                    Reversibility::Reversible
                } else {
                    Reversibility::Irreversible
                }
            }
            ActionDescriptor::ShellCommand { command } => {
                if is_git_push(command) || is_rm_rf(command) {
                    Reversibility::Irreversible
                } else if is_safe_shell_command(command) {
                    Reversibility::Reversible
                } else {
                    // Default-deny: anything not on the explicit safe
                    // allowlist above (mv, dd, curl, chmod, a bare `rm`
                    // with no flags, ...) is treated as irreversible.
                    Reversibility::Irreversible
                }
            }
            // An MCP server is arbitrary, unaudited code — there is no
            // safe-by-construction subset to allowlist, unlike shell.
            ActionDescriptor::McpToolCall { .. } => Reversibility::Irreversible,
            ActionDescriptor::Other { .. } => Reversibility::Reversible,
        }
    }
}

/// Matches `git push` and any `git push ...` invocation carrying a
/// `--force`/`-f` flag anywhere in the remaining args (order-independent).
/// Plain `git push` with no force flag is still flagged irreversible: a
/// push mutates a shared remote regardless of force.
fn is_git_push(command: &[String]) -> bool {
    matches!(command.first().map(String::as_str), Some("git"))
        && matches!(command.get(1).map(String::as_str), Some("push"))
}

/// Matches `rm` invocations that carry both "recursive" and "force"
/// semantics, in any flag order/grouping: `-rf`, `-fr`, `-r -f`, `-f -r`,
/// `--recursive --force` (and mixed short/long forms). A bare `rm file`
/// (no flags) or `rm -r` alone (no force) is NOT irreversible under this
/// heuristic.
fn is_rm_rf(command: &[String]) -> bool {
    if command.first().map(String::as_str) != Some("rm") {
        return false;
    }
    let mut has_recursive = false;
    let mut has_force = false;
    for arg in &command[1..] {
        match arg.as_str() {
            "-r" | "-R" | "--recursive" => has_recursive = true,
            "-f" | "--force" => has_force = true,
            short if short.starts_with('-') && !short.starts_with("--") => {
                // Combined short flags, e.g. "-rf", "-fr", "-rfv".
                if short.contains('r') || short.contains('R') {
                    has_recursive = true;
                }
                if short.contains('f') {
                    has_force = true;
                }
            }
            _ => {}
        }
    }
    has_recursive && has_force
}

/// Explicit allowlist of shell commands considered safe regardless of
/// their arguments (read-only/introspection utilities), plus a narrow set
/// of non-mutating `find`/`git` invocations. Anything not matched here
/// falls through to `Irreversible` in `DefaultClassifier::classify` —
/// this function is the *only* way a `ShellCommand` becomes Reversible
/// (other than the `is_git_push`/`is_rm_rf` check, which runs first and
/// always wins the other way).
fn is_safe_shell_command(command: &[String]) -> bool {
    let Some(program) = command.first().map(String::as_str) else {
        return false;
    };
    match program {
        "ls" | "pwd" | "cat" | "echo" | "wc" | "diff" | "whoami" | "date" | "env" | "which"
        | "true" | "false" | "head" | "tail" | "file" | "grep" => true,
        "find" => is_safe_find(command),
        "git" => is_safe_git(command),
        _ => false,
    }
}

/// `find` is safe unless any argument requests a mutating/side-effecting
/// action: `-delete`, `-exec`, `-execdir`, `-ok`, `-okdir`, `-fprint`,
/// `-fprint0`, `-fprintf`. Same flag-scanning technique as `is_rm_rf`:
/// walk every argument and look for the dangerous ones by exact match
/// (these are all long-form `find` primaries, never combined into short
/// clusters the way `rm -rf` is).
fn is_safe_find(command: &[String]) -> bool {
    const MUTATING_PRIMARIES: &[&str] = &[
        "-delete", "-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fprint0", "-fprintf",
    ];
    !command[1..]
        .iter()
        .any(|arg| MUTATING_PRIMARIES.contains(&arg.as_str()))
}

/// `git` is safe only for a narrow set of read-only subcommands:
/// `status`, `diff`, `log`, `show` (any further arguments allowed), or a
/// bare `git branch` with no arguments at all (`git branch -D foo`/
/// `git branch -m foo` mutate/delete branches and must NOT match).
fn is_safe_git(command: &[String]) -> bool {
    match command.get(1).map(String::as_str) {
        Some("status") | Some("diff") | Some("log") | Some("show") => true,
        Some("branch") => command.len() == 2,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn classifier() -> DefaultClassifier {
        DefaultClassifier::new(WorkdirAllowlist::new("/home/user/project"))
    }

    fn cmd(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn write_file_inside_allowlist_is_reversible() {
        let action = ActionDescriptor::WriteFile {
            path: PathBuf::from("src/main.rs"),
        };
        assert_eq!(classifier().classify(&action), Reversibility::Reversible);
    }

    #[test]
    fn write_file_outside_allowlist_is_irreversible() {
        let action = ActionDescriptor::WriteFile {
            path: PathBuf::from("/etc/passwd"),
        };
        assert_eq!(classifier().classify(&action), Reversibility::Irreversible);
    }

    #[test]
    fn delete_file_inside_allowlist_is_reversible() {
        let action = ActionDescriptor::DeleteFile {
            path: PathBuf::from("src/main.rs"),
        };
        assert_eq!(classifier().classify(&action), Reversibility::Reversible);
    }

    #[test]
    fn delete_file_outside_allowlist_is_irreversible() {
        let action = ActionDescriptor::DeleteFile {
            path: PathBuf::from("/etc/passwd"),
        };
        assert_eq!(classifier().classify(&action), Reversibility::Irreversible);
    }

    #[test]
    fn other_is_always_reversible() {
        let action = ActionDescriptor::Other {
            label: "custom action".to_string(),
        };
        assert_eq!(classifier().classify(&action), Reversibility::Reversible);
    }

    #[test]
    fn rm_rf_combined_flag_is_irreversible() {
        assert!(is_rm_rf(&cmd(&["rm", "-rf", "/tmp/foo"])));
    }

    #[test]
    fn rm_fr_combined_flag_is_irreversible() {
        assert!(is_rm_rf(&cmd(&["rm", "-fr", "/tmp/foo"])));
    }

    #[test]
    fn rm_separate_flags_is_irreversible() {
        assert!(is_rm_rf(&cmd(&["rm", "-r", "-f", "/tmp/foo"])));
    }

    #[test]
    fn rm_long_flags_is_irreversible() {
        assert!(is_rm_rf(&cmd(&[
            "rm",
            "--recursive",
            "--force",
            "/tmp/foo"
        ])));
    }

    #[test]
    fn rm_without_flags_is_not_matched() {
        assert!(!is_rm_rf(&cmd(&["rm", "x"])));
    }

    #[test]
    fn rm_recursive_only_is_not_matched() {
        assert!(!is_rm_rf(&cmd(&["rm", "-r", "x"])));
    }

    #[test]
    fn git_push_is_matched() {
        assert!(is_git_push(&cmd(&["git", "push"])));
    }

    #[test]
    fn git_push_force_is_matched() {
        assert!(is_git_push(&cmd(&["git", "push", "--force"])));
    }

    #[test]
    fn git_status_is_not_matched() {
        assert!(!is_git_push(&cmd(&["git", "status"])));
    }

    #[test]
    fn shell_command_rm_rf_is_irreversible_via_classifier() {
        let action = ActionDescriptor::ShellCommand {
            command: cmd(&["rm", "-rf", "/tmp/foo"]),
        };
        assert_eq!(classifier().classify(&action), Reversibility::Irreversible);
    }

    #[test]
    fn shell_command_git_push_is_irreversible_via_classifier() {
        let action = ActionDescriptor::ShellCommand {
            command: cmd(&["git", "push"]),
        };
        assert_eq!(classifier().classify(&action), Reversibility::Irreversible);
    }

    #[test]
    fn shell_command_rm_plain_is_irreversible_via_classifier() {
        // `rm` (no flags) is NOT on the safe allowlist anymore — default-deny
        // means any shell command not explicitly known-safe is Irreversible.
        let action = ActionDescriptor::ShellCommand {
            command: cmd(&["rm", "archivo.txt"]),
        };
        assert_eq!(classifier().classify(&action), Reversibility::Irreversible);
    }

    #[test]
    fn shell_command_git_status_is_reversible_via_classifier() {
        let action = ActionDescriptor::ShellCommand {
            command: cmd(&["git", "status"]),
        };
        assert_eq!(classifier().classify(&action), Reversibility::Reversible);
    }

    fn shell(parts: &[&str]) -> ActionDescriptor {
        ActionDescriptor::ShellCommand {
            command: cmd(parts),
        }
    }

    #[test]
    fn safe_readonly_commands_are_reversible() {
        for parts in [
            &["ls", "-la"][..],
            &["pwd"][..],
            &["cat", "file.txt"][..],
            &["echo", "hi"][..],
            &["wc", "-l", "file.txt"][..],
            &["diff", "a", "b"][..],
            &["whoami"][..],
            &["date"][..],
            &["env"][..],
            &["which", "cargo"][..],
            &["true"][..],
            &["false"][..],
            &["head", "-n", "5", "file.txt"][..],
            &["tail", "-f", "file.txt"][..],
            &["file", "file.txt"][..],
            &["grep", "-r", "needle", "."][..],
        ] {
            assert_eq!(
                classifier().classify(&shell(parts)),
                Reversibility::Reversible,
                "expected {parts:?} to be Reversible"
            );
        }
    }

    #[test]
    fn find_without_mutating_flags_is_reversible() {
        assert_eq!(
            classifier().classify(&shell(&["find", ".", "-name", "*.rs"])),
            Reversibility::Reversible
        );
    }

    #[test]
    fn find_delete_is_irreversible() {
        assert_eq!(
            classifier().classify(&shell(&["find", ".", "-delete"])),
            Reversibility::Irreversible
        );
    }

    #[test]
    fn find_exec_is_irreversible() {
        assert_eq!(
            classifier().classify(&shell(&["find", ".", "-exec", "rm", "{}", ";"])),
            Reversibility::Irreversible
        );
    }

    #[test]
    fn git_diff_log_show_are_reversible() {
        for parts in [
            &["git", "diff"][..],
            &["git", "log"][..],
            &["git", "show"][..],
        ] {
            assert_eq!(
                classifier().classify(&shell(parts)),
                Reversibility::Reversible,
                "expected {parts:?} to be Reversible"
            );
        }
    }

    #[test]
    fn git_branch_with_no_args_is_reversible() {
        assert_eq!(
            classifier().classify(&shell(&["git", "branch"])),
            Reversibility::Reversible
        );
    }

    #[test]
    fn git_branch_delete_is_irreversible() {
        assert_eq!(
            classifier().classify(&shell(&["git", "branch", "-D", "foo"])),
            Reversibility::Irreversible
        );
    }

    #[test]
    fn previously_unclassified_dangerous_commands_are_now_irreversible() {
        // The regression-proof for the gap this work closes: these all
        // used to slip through as Reversible under the old "allow by
        // default, deny two patterns" table.
        for parts in [
            &["mv", "a", "b"][..],
            &["curl", "http://x"][..],
            &["chmod", "-R", "777", "/"][..],
            &["dd", "if=/dev/zero", "of=/dev/sda"][..],
        ] {
            assert_eq!(
                classifier().classify(&shell(parts)),
                Reversibility::Irreversible,
                "expected {parts:?} to be Irreversible"
            );
        }
    }

    #[test]
    fn mcp_tool_call_is_always_irreversible() {
        let action = ActionDescriptor::McpToolCall {
            server: "x".to_string(),
            tool: "y".to_string(),
        };
        assert_eq!(classifier().classify(&action), Reversibility::Irreversible);
    }
}
