//! Resolution of on-disk locations for the config file and session storage.
//!
//! `braze` targets Linux only for the MVP (see `PLAN.md`), so a minimal
//! XDG-aware resolution — `$XDG_CONFIG_HOME`/`$XDG_DATA_HOME` with a
//! `$HOME`-based fallback — covers it without pulling in a `dirs`-style
//! crate. All logic is written against an injectable environment lookup so
//! it can be unit-tested without mutating real process environment
//! variables (which is both unsafe-in-edition-2024 and racy across
//! parallel tests).

use std::path::PathBuf;

/// Path to the on-disk config file: `$XDG_CONFIG_HOME/braze/config.json`,
/// falling back to `$HOME/.config/braze/config.json`.
///
/// Returns `None` only if neither `XDG_CONFIG_HOME` nor `HOME` can be
/// resolved. Callers must treat that as "skip the file layer, use
/// defaults/env instead" — an unresolvable or missing config file is never
/// fatal for [`crate::Config::load`].
pub fn config_file_path() -> Option<PathBuf> {
    resolve_config_file_path(|key| std::env::var(key).ok())
}

/// Default session log directory: `$XDG_DATA_HOME/braze/sessions`, falling
/// back to `$HOME/.local/share/braze/sessions`, falling back to a system
/// temp directory (never a path relative to the process's cwd) if even
/// `HOME` is unavailable.
///
/// Kept infallible (never returns `Result`) because it feeds
/// [`crate::Config::default`], which must never fail.
///
/// The rollout log under this directory is what `braze-cli` replays as
/// pre-approved permission decisions on `--resume` (see
/// `PermissionGuard::seed_remembered`). A cwd-relative fallback here would
/// place that log inside the same working directory the agent's own
/// `WriteFile` tool treats as Reversible-without-confirmation — letting the
/// model silently plant/edit approvals for its own next `--resume`. Falling
/// back to the OS temp directory instead keeps the log outside any
/// `WorkdirAllowlist` built from the project cwd.
pub fn default_session_dir() -> PathBuf {
    resolve_default_session_dir(|key| std::env::var(key).ok())
}

fn resolve_config_file_path(env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(xdg) = non_empty(env("XDG_CONFIG_HOME")) {
        return Some(PathBuf::from(xdg).join("braze").join("config.json"));
    }
    non_empty(env("HOME")).map(|home| {
        PathBuf::from(home)
            .join(".config")
            .join("braze")
            .join("config.json")
    })
}

fn resolve_default_session_dir(env: impl Fn(&str) -> Option<String>) -> PathBuf {
    if let Some(xdg) = non_empty(env("XDG_DATA_HOME")) {
        return PathBuf::from(xdg).join("braze").join("sessions");
    }
    if let Some(home) = non_empty(env("HOME")) {
        return PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("braze")
            .join("sessions");
    }
    // Deliberately NOT a cwd-relative path — see the doc-comment on
    // `default_session_dir` for why that would be a security hole.
    std::env::temp_dir().join("braze-sessions")
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn config_file_path_prefers_xdg_config_home() {
        let env = env_map(&[
            ("XDG_CONFIG_HOME", "/custom/config"),
            ("HOME", "/home/someone"),
        ]);
        let path = resolve_config_file_path(|k| env.get(k).cloned());
        assert_eq!(
            path,
            Some(PathBuf::from("/custom/config/braze/config.json"))
        );
    }

    #[test]
    fn config_file_path_falls_back_to_home() {
        let env = env_map(&[("HOME", "/home/someone")]);
        let path = resolve_config_file_path(|k| env.get(k).cloned());
        assert_eq!(
            path,
            Some(PathBuf::from("/home/someone/.config/braze/config.json"))
        );
    }

    #[test]
    fn config_file_path_none_without_xdg_or_home() {
        let env: HashMap<String, String> = HashMap::new();
        let path = resolve_config_file_path(|k| env.get(k).cloned());
        assert_eq!(path, None);
    }

    #[test]
    fn config_file_path_ignores_empty_xdg_config_home() {
        let env = env_map(&[("XDG_CONFIG_HOME", ""), ("HOME", "/home/someone")]);
        let path = resolve_config_file_path(|k| env.get(k).cloned());
        assert_eq!(
            path,
            Some(PathBuf::from("/home/someone/.config/braze/config.json"))
        );
    }

    #[test]
    fn default_session_dir_prefers_xdg_data_home() {
        let env = env_map(&[("XDG_DATA_HOME", "/custom/data"), ("HOME", "/home/someone")]);
        let dir = resolve_default_session_dir(|k| env.get(k).cloned());
        assert_eq!(dir, PathBuf::from("/custom/data/braze/sessions"));
    }

    #[test]
    fn default_session_dir_falls_back_to_home() {
        let env = env_map(&[("HOME", "/home/someone")]);
        let dir = resolve_default_session_dir(|k| env.get(k).cloned());
        assert_eq!(
            dir,
            PathBuf::from("/home/someone/.local/share/braze/sessions")
        );
    }

    /// Regression test: without HOME/XDG, the fallback must land outside
    /// the process's cwd (the system temp dir), never at a cwd-relative
    /// path — a cwd-relative session dir would let the agent's own
    /// in-allowlist `write_file` plant permission approvals for itself
    /// (see the doc-comment on `default_session_dir`).
    #[test]
    fn default_session_dir_falls_back_to_system_temp_dir_not_cwd_relative() {
        let env: HashMap<String, String> = HashMap::new();
        let dir = resolve_default_session_dir(|k| env.get(k).cloned());
        assert!(dir.is_absolute(), "expected an absolute path, got {dir:?}");
        assert!(
            dir.starts_with(std::env::temp_dir()),
            "expected a path under the system temp dir, got {dir:?}"
        );
        assert_eq!(
            dir.file_name().and_then(|n| n.to_str()),
            Some("braze-sessions")
        );
    }
}
