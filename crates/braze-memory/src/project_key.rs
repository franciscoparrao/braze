//! Project root detection — same problem
//! `~/.claude/session_state/context_manager.py`'s "Detección de
//! proyecto" already solves for the human-operated system this crate
//! takes inspiration from, adapted to what `braze` has on hand: a `cwd`
//! and (usually) a git repo, no directory of pre-registered project
//! names to match against.

use std::path::{Path, PathBuf};

/// Walks up from `cwd` looking for a `.git` entry (directory or file —
/// a worktree's `.git` is a file pointing at the real gitdir, and either
/// is a valid "this is a project root" signal). Returns the first
/// ancestor that has one, canonicalized; falls back to `cwd` itself
/// (also canonicalized, or as-is if canonicalization fails — e.g. a
/// bench sandbox that may not exist on disk in every caller) when no
/// `.git` is found anywhere up the tree.
///
/// Deliberately simple: no `.hg`/`.svn`/workspace-file heuristics. If
/// this turns out to matter for non-git projects, extend it then —
/// guessing now would be unvalidated complexity.
pub fn resolve_project_root(cwd: &Path) -> PathBuf {
    let mut candidate = cwd;
    loop {
        if candidate.join(".git").exists() {
            return candidate.canonicalize().unwrap_or_else(|_| candidate.to_path_buf());
        }
        match candidate.parent() {
            Some(parent) => candidate = parent,
            None => break,
        }
    }
    cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf())
}

/// The identity string stored as `ProjectMemory::project_key` — the
/// resolved root's path, as a `String`. A separate function (not just
/// "call `resolve_project_root` and `.display()` it inline") so the one
/// formatting decision lives in one place.
pub fn project_key_for(cwd: &Path) -> String {
    resolve_project_root(cwd).display().to_string()
}

/// Where `FileProjectMemoryStore` should persist the memory file for a
/// project rooted at `project_root` — `.braze/memory.json`, mirroring
/// the design doc's recommendation to version it alongside the project
/// rather than in a global XDG directory (§ "mejor opción para nuestra
/// configuración").
pub fn default_memory_path(project_root: &Path) -> PathBuf {
    project_root.join(".braze").join("memory.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_nearest_ancestor_with_a_dot_git() {
        let tmp = std::env::temp_dir().join(format!("braze-memory-test-{}", uuid_like()));
        let repo_root = tmp.join("repo");
        let subdir = repo_root.join("src").join("nested");
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::create_dir_all(repo_root.join(".git")).unwrap();

        let resolved = resolve_project_root(&subdir);
        assert_eq!(resolved, repo_root.canonicalize().unwrap());

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn falls_back_to_cwd_when_no_dot_git_exists_anywhere() {
        let tmp = std::env::temp_dir().join(format!("braze-memory-test-nogit-{}", uuid_like()));
        std::fs::create_dir_all(&tmp).unwrap();

        // This only holds if none of tmp's ancestors happen to have a
        // .git (true for the system temp dir in every sane environment,
        // including CI).
        let resolved = resolve_project_root(&tmp);
        assert_eq!(resolved, tmp.canonicalize().unwrap());

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn default_memory_path_is_dot_braze_memory_json() {
        let root = PathBuf::from("/some/project");
        assert_eq!(
            default_memory_path(&root),
            PathBuf::from("/some/project/.braze/memory.json")
        );
    }

    /// Tiny process-unique suffix for test tmp dirs, without pulling in
    /// the `uuid` crate as a dev-dependency for one test helper — the
    /// workspace already forbids `Math.random()`-shaped nondeterminism
    /// in a different context (workflow scripts); here it's plain std,
    /// so a thread-id + address-of-local mix is enough to avoid
    /// collisions between parallel test runs.
    fn uuid_like() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("{:?}-{nanos}", std::thread::current().id())
    }
}
