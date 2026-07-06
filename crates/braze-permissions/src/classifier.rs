use std::path::Path;

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

    /// Explicit allowlist of shell commands considered safe (read-only/
    /// introspection utilities), plus a narrow set of non-mutating
    /// `find`/`git` invocations. Anything not matched here falls through to
    /// `Irreversible` in `classify` — this is the *only* way a
    /// `ShellCommand` becomes Reversible (other than the
    /// `is_git_push`/`is_rm_rf` check, which runs first and always wins the
    /// other way).
    ///
    /// Content-reading commands (`cat`/`head`/`tail`/`grep`/`file`/`diff`/
    /// `find`) additionally require every non-flag argument to resolve
    /// inside the `WorkdirAllowlist` — see
    /// [`Self::all_path_like_args_allowed`]. Without this, `shell_exec`
    /// let any of these read an arbitrary path (`~/.ssh/id_rsa`,
    /// `/etc/shadow`) with no confirmation, even though the same read via
    /// `read_file`/`grep`/`glob` is gated by `ActionDescriptor::ReadPath`.
    /// See docs/AUDITORIA-2026-07-v2.md hallazgo N-8b.
    fn is_safe_shell_command(&self, command: &[String]) -> bool {
        let Some(program) = command.first().map(String::as_str) else {
            return false;
        };
        match program {
            "ls" | "pwd" | "echo" | "wc" | "whoami" | "date" | "which" | "true" | "false" => true,
            "cat" | "head" | "tail" | "file" | "diff" | "grep" => {
                self.all_path_like_args_allowed(command)
            }
            "find" => is_safe_find(command) && self.all_path_like_args_allowed(command),
            "git" => is_safe_git(command),
            "env" => is_safe_env(command),
            _ => false,
        }
    }

    /// Every argument that doesn't look like a flag (doesn't start with
    /// `-`) is treated as a candidate path and must resolve inside the
    /// `WorkdirAllowlist`. This is deliberately conservative: a `grep`
    /// pattern or a numeric flag value (e.g. the `5` in `head -n 5`) gets
    /// checked too and always resolves harmlessly under cwd, so the only
    /// effect is requiring confirmation for genuine out-of-sandbox reads
    /// (`grep -r needle /home/user`, `cat /etc/shadow`) — never a missed
    /// one.
    fn all_path_like_args_allowed(&self, command: &[String]) -> bool {
        command[1..]
            .iter()
            .filter(|arg| !arg.starts_with('-'))
            .all(|arg| self.allowlist.is_allowed(Path::new(arg)))
    }
}

impl ActionClassifier for DefaultClassifier {
    fn classify(&self, action: &ActionDescriptor) -> Reversibility {
        match action {
            ActionDescriptor::WriteFile { path }
            | ActionDescriptor::DeleteFile { path }
            | ActionDescriptor::ReadPath { path } => {
                if self.allowlist.is_allowed(path) {
                    Reversibility::Reversible
                } else {
                    Reversibility::Irreversible
                }
            }
            ActionDescriptor::ShellCommand { command } => {
                if is_git_push(command) || is_rm_rf(command) {
                    Reversibility::Irreversible
                } else if self.is_safe_shell_command(command) {
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

/// `env` is only safe when it has no trailing command to execute — i.e.
/// every argument after `env` is a `NAME=VALUE` assignment (or there are
/// none at all, the "print the environment" form). `env <program> ...`
/// (with or without leading assignments) runs `<program>` as a child
/// process, which is full, unaudited command execution wearing a
/// read-only-looking mask — `env rm -rf /tmp/x` must never slip through
/// as Reversible the way a bare `rm -rf` correctly does not.
fn is_safe_env(command: &[String]) -> bool {
    command[1..].iter().all(|arg| is_env_assignment(arg))
}

fn is_env_assignment(arg: &str) -> bool {
    match arg.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

/// `find` is safe unless any argument requests a mutating/side-effecting
/// action: `-delete`, `-exec`, `-execdir`, `-ok`, `-okdir`, `-fprint`,
/// `-fprint0`, `-fprintf`, `-fls`. Same flag-scanning technique as
/// `is_rm_rf`: walk every argument and look for the dangerous ones by exact
/// match (these are all long-form `find` primaries, never combined into
/// short clusters the way `rm -rf` is).
///
/// `-fls FILE` was missing from the original list: like `-fprint`, it
/// writes (truncating) an arbitrary file — `find . -fls /home/user/.git/config`
/// silently clobbers it. See docs/AUDITORIA-2026-07-v2.md hallazgo N-8a.
fn is_safe_find(command: &[String]) -> bool {
    const MUTATING_PRIMARIES: &[&str] = &[
        "-delete", "-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fprint0", "-fprintf", "-fls",
    ];
    !command[1..]
        .iter()
        .any(|arg| MUTATING_PRIMARIES.contains(&arg.as_str()))
}

/// `git` is safe only for a narrow set of read-only subcommands: `status`
/// (any further arguments allowed), `diff`/`log`/`show` (any further
/// arguments allowed *except* the write-capable ones below), or a bare
/// `git branch` with no arguments at all (`git branch -D foo`/
/// `git branch -m foo` mutate/delete branches and must NOT match).
///
/// `diff`/`log`/`show` share git's diff machinery, which accepts
/// `-o`/`--output[=FILE]` to write its output to an arbitrary file
/// (truncating it if it exists) and `--ext-diff` to shell out to whatever
/// external diff driver is configured — both are write/exec primitives
/// wearing a read-only mask, the same class of bug `env` was for the
/// top-level allowlist. See docs/AUDITORIA-2026-07-v2.md hallazgo N-8a.
fn is_safe_git(command: &[String]) -> bool {
    match command.get(1).map(String::as_str) {
        Some("status") => true,
        Some("diff") | Some("log") | Some("show") => !command[2..].iter().any(|arg| {
            arg == "-o" || arg == "--output" || arg.starts_with("--output=") || arg == "--ext-diff"
        }),
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

    #[test]
    fn bare_env_and_env_with_only_assignments_are_reversible() {
        for parts in [
            &["env"][..],
            &["env", "FOO=bar"][..],
            &["env", "A=1", "B=2"][..],
        ] {
            assert_eq!(
                classifier().classify(&shell(parts)),
                Reversibility::Reversible,
                "expected {parts:?} to be Reversible"
            );
        }
    }

    /// Regression test for the `env`-as-exec-bypass: `env <program> ...`
    /// runs `<program>` as a child process. If this were classified
    /// Reversible, any destructive command could dodge confirmation by
    /// prefixing it with `env` (with or without leading assignments).
    #[test]
    fn env_with_a_trailing_command_is_irreversible() {
        for parts in [
            &["env", "rm", "-rf", "/tmp/x"][..],
            &["env", "VAR=1", "rm", "-rf", "/tmp/x"][..],
            &["env", "bash", "-c", "rm -rf /"][..],
            &["env", "curl", "http://x"][..],
        ] {
            assert_eq!(
                classifier().classify(&shell(parts)),
                Reversibility::Irreversible,
                "expected {parts:?} to be Irreversible"
            );
        }
    }

    /// Regression test for hallazgo N-8a: `find -fls FILE` writes
    /// (truncating) FILE — it must not slip through as Reversible the way
    /// `-fprint`/`-delete` correctly don't.
    #[test]
    fn find_fls_is_irreversible() {
        assert_eq!(
            classifier().classify(&shell(&["find", ".", "-fls", "/tmp/out.txt"])),
            Reversibility::Irreversible
        );
    }

    /// Regression test for hallazgo N-8a: `git diff/log/show --output=FILE`
    /// (or `-o`/`--output FILE`) writes to an arbitrary file; `--ext-diff`
    /// shells out to a configured external diff driver. None of these may
    /// be Reversible.
    #[test]
    fn git_diff_log_show_with_output_or_ext_diff_are_irreversible() {
        for parts in [
            &["git", "diff", "--output=/home/user/.bashrc"][..],
            &["git", "diff", "--output", "/home/user/.bashrc"][..],
            &["git", "diff", "-o", "/home/user/.bashrc"][..],
            &["git", "log", "--output=/tmp/x"][..],
            &["git", "show", "--output=/tmp/x"][..],
            &["git", "diff", "--ext-diff"][..],
        ] {
            assert_eq!(
                classifier().classify(&shell(parts)),
                Reversibility::Irreversible,
                "expected {parts:?} to be Irreversible"
            );
        }
    }

    /// Regression test for hallazgo N-8b: reading an out-of-sandbox path
    /// via a shell command must require confirmation, exactly like
    /// `read_file`/`grep`/`glob` already do via `ActionDescriptor::ReadPath`.
    /// Before this fix, `cat`/`grep`/`find`/`head`/`tail`/`file`/`diff`
    /// were unconditionally Reversible regardless of what path they read.
    #[test]
    fn shell_read_commands_outside_workdir_are_irreversible() {
        for parts in [
            &["cat", "/etc/shadow"][..],
            &["cat", "/home/other/.ssh/id_rsa"][..],
            &["grep", "-r", "AWS_SECRET", "/home/other"][..],
            &["find", "/home/other", "-name", "*.pem"][..],
            &["head", "/etc/shadow"][..],
            &["tail", "/etc/shadow"][..],
            &["file", "/etc/shadow"][..],
            &["diff", "/etc/passwd", "/dev/null"][..],
        ] {
            assert_eq!(
                classifier().classify(&shell(parts)),
                Reversibility::Irreversible,
                "expected {parts:?} to be Irreversible"
            );
        }
    }

    /// The same commands reading only inside the workdir must remain
    /// Reversible — the fix must not regress the common case.
    #[test]
    fn shell_read_commands_inside_workdir_are_still_reversible() {
        for parts in [
            &["cat", "src/main.rs"][..],
            &["cat", "/home/user/project/src/main.rs"][..],
            &["grep", "-r", "needle", "."][..],
            &["find", ".", "-name", "*.rs"][..],
            &["head", "-n", "5", "file.txt"][..],
            &["tail", "-f", "file.txt"][..],
            &["file", "file.txt"][..],
            &["diff", "a", "b"][..],
        ] {
            assert_eq!(
                classifier().classify(&shell(parts)),
                Reversibility::Reversible,
                "expected {parts:?} to be Reversible"
            );
        }
    }
}
