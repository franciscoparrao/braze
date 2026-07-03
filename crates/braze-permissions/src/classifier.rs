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

/// MVP fixed "always confirm" table: git push/--force, rm -rf (and flag-
/// order variants), and any WriteFile/DeleteFile whose path escapes the
/// WorkdirAllowlist. A DeleteFile INSIDE the allowlist is Reversible for
/// MVP (PLAN.md's table only names "escrituras fuera del cwd", not
/// deletes in scope — do not silently expand beyond spec).
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
                } else {
                    Reversibility::Reversible
                }
            }
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
        assert_eq!(
            classifier().classify(&action),
            Reversibility::Reversible
        );
    }

    #[test]
    fn write_file_outside_allowlist_is_irreversible() {
        let action = ActionDescriptor::WriteFile {
            path: PathBuf::from("/etc/passwd"),
        };
        assert_eq!(
            classifier().classify(&action),
            Reversibility::Irreversible
        );
    }

    #[test]
    fn delete_file_inside_allowlist_is_reversible() {
        let action = ActionDescriptor::DeleteFile {
            path: PathBuf::from("src/main.rs"),
        };
        assert_eq!(
            classifier().classify(&action),
            Reversibility::Reversible
        );
    }

    #[test]
    fn delete_file_outside_allowlist_is_irreversible() {
        let action = ActionDescriptor::DeleteFile {
            path: PathBuf::from("/etc/passwd"),
        };
        assert_eq!(
            classifier().classify(&action),
            Reversibility::Irreversible
        );
    }

    #[test]
    fn other_is_always_reversible() {
        let action = ActionDescriptor::Other {
            label: "mcp tool call".to_string(),
        };
        assert_eq!(
            classifier().classify(&action),
            Reversibility::Reversible
        );
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
        assert!(is_rm_rf(&cmd(&["rm", "--recursive", "--force", "/tmp/foo"])));
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
        assert_eq!(
            classifier().classify(&action),
            Reversibility::Irreversible
        );
    }

    #[test]
    fn shell_command_git_push_is_irreversible_via_classifier() {
        let action = ActionDescriptor::ShellCommand {
            command: cmd(&["git", "push"]),
        };
        assert_eq!(
            classifier().classify(&action),
            Reversibility::Irreversible
        );
    }

    #[test]
    fn shell_command_rm_plain_is_reversible_via_classifier() {
        let action = ActionDescriptor::ShellCommand {
            command: cmd(&["rm", "archivo.txt"]),
        };
        assert_eq!(
            classifier().classify(&action),
            Reversibility::Reversible
        );
    }

    #[test]
    fn shell_command_git_status_is_reversible_via_classifier() {
        let action = ActionDescriptor::ShellCommand {
            command: cmd(&["git", "status"]),
        };
        assert_eq!(
            classifier().classify(&action),
            Reversibility::Reversible
        );
    }
}
