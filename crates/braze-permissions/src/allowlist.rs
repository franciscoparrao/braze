use std::path::{Component, Path, PathBuf};

/// Lexically normalize a path: resolve `.` and `..` components without
/// touching the filesystem (no symlink resolution, no existence check).
/// A leading `..` that would escape the root of an absolute path is kept
/// as-is (there is nothing left to pop).
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                match out.components().next_back() {
                    Some(Component::Normal(_)) => {
                        out.pop();
                    }
                    Some(Component::RootDir) | Some(Component::Prefix(_)) => {
                        // Already at the root, ".." is a no-op.
                    }
                    _ => {
                        // Empty, or another ParentDir/CurDir — keep the ".."
                        out.push(component);
                    }
                }
            }
            Component::CurDir => {
                // Drop "." components entirely.
            }
            other => out.push(other),
        }
    }
    out
}

/// Directory-scoping layer (MVP: no Landlock/OS enforcement yet — a soft
/// gate, not a hard kernel-level boundary).
/// Paths are normalized LEXICALLY (resolve . and .. components), NEVER via
/// std::fs::canonicalize — target paths for write/create actions
/// frequently don't exist yet, and canonicalize errors on a missing path.
/// Symlink escapes are NOT caught in the MVP (documented limitation).
pub struct WorkdirAllowlist {
    roots: Vec<PathBuf>, // lexically normalized, absolute; roots[0] is cwd
}

impl WorkdirAllowlist {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        let root = normalize_lexically(&cwd.into());
        Self { roots: vec![root] }
    }

    /// Forward-compat seam for a future braze-config-driven list of extra
    /// allowed roots — unused by any MVP caller today.
    pub fn with_extra_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.roots.push(normalize_lexically(&root.into()));
        self
    }

    /// Resolves `path` to an absolute, lexically-normalized `PathBuf`: a
    /// relative path is joined against `roots[0]` (cwd) first. Shared by
    /// [`WorkdirAllowlist::is_allowed`] and by `braze-permissions::guard`'s
    /// session-remember key, which needs the same canonical form so that
    /// e.g. `"src/main.rs"` and `"./src/main.rs"` hash to the same key.
    pub(crate) fn resolve(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            normalize_lexically(path)
        } else {
            normalize_lexically(&self.roots[0].join(path))
        }
    }

    /// Relative `path` is resolved against roots[0] (cwd) before comparison.
    pub fn is_allowed(&self, path: &Path) -> bool {
        let resolved = self.resolve(path);
        self.roots.iter().any(|root| resolved.starts_with(root))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_path_resolved_against_cwd_is_allowed() {
        let allowlist = WorkdirAllowlist::new("/home/user/project");
        assert!(allowlist.is_allowed(Path::new("src/main.rs")));
    }

    #[test]
    fn dotdot_escaping_cwd_is_not_allowed() {
        let allowlist = WorkdirAllowlist::new("/home/user/project");
        assert!(!allowlist.is_allowed(Path::new("../../etc/passwd")));
    }

    #[test]
    fn dotdot_staying_inside_cwd_is_allowed() {
        let allowlist = WorkdirAllowlist::new("/home/user/project");
        assert!(allowlist.is_allowed(Path::new("src/../src/main.rs")));
    }

    #[test]
    fn absolute_path_inside_cwd_is_allowed() {
        let allowlist = WorkdirAllowlist::new("/home/user/project");
        assert!(allowlist.is_allowed(Path::new("/home/user/project/src/main.rs")));
    }

    #[test]
    fn absolute_path_outside_cwd_is_not_allowed() {
        let allowlist = WorkdirAllowlist::new("/home/user/project");
        assert!(!allowlist.is_allowed(Path::new("/etc/passwd")));
    }

    #[test]
    fn path_inside_extra_root_is_allowed() {
        let allowlist = WorkdirAllowlist::new("/home/user/project").with_extra_root("/opt/shared");
        assert!(allowlist.is_allowed(Path::new("/opt/shared/data.csv")));
    }

    #[test]
    fn path_outside_all_roots_is_not_allowed() {
        let allowlist = WorkdirAllowlist::new("/home/user/project").with_extra_root("/opt/shared");
        assert!(!allowlist.is_allowed(Path::new("/var/log/syslog")));
    }

    #[test]
    fn normalize_lexically_resolves_dotdot_without_touching_fs() {
        let normalized = normalize_lexically(Path::new("/a/b/../c/./d"));
        assert_eq!(normalized, PathBuf::from("/a/c/d"));
    }
}
