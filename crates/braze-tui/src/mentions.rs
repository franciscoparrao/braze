//! File listing for `@mention` completion — "fase TUI 2" (PLAN.md).
//! Deliberately simple: a hardcoded exclusion list for the handful of
//! directories that would otherwise blow up the walk (`target/` alone
//! can be hundreds of thousands of entries in a Rust workspace), not
//! full `.gitignore` parsing — that's what the `ignore` crate exists
//! for, and pulling it in for this is more than an MVP needs. A future
//! refinement, not a correctness bug: worst case, a mention popup lists
//! a few more files than a `.gitignore`-aware one would.

use std::path::Path;

const EXCLUDED_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    "dist",
    "build",
];

/// Hard cap on how many files a single walk collects — a pathologically
/// large tree (or a symlink cycle `EXCLUDED_DIRS` doesn't happen to
/// catch) degrades to "the mention list is incomplete", never to an
/// unbounded walk.
const MAX_FILES: usize = 5000;

/// Walks `root` recursively (depth-first, `EXCLUDED_DIRS` pruned before
/// descending into them) and returns every regular file's path relative
/// to `root`, sorted. Called once per `App` (cached — see `app.rs`'s
/// `mentionable_files`), not on every keystroke: a session-long stale
/// list if files change mid-session is an accepted, documented
/// limitation, not silently wrong — the file itself is still read fresh
/// by `read_file` regardless of when the mention was typed.
pub fn list_files(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if files.len() >= MAX_FILES {
                files.sort();
                return files;
            }
            let path = entry.path();
            let is_excluded = entry
                .file_name()
                .to_str()
                .is_some_and(|name| EXCLUDED_DIRS.contains(&name));
            if is_excluded {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(relative) = path.strip_prefix(root) {
                files.push(relative.to_string_lossy().into_owned());
            }
        }
    }

    files.sort();
    files
}

/// Files whose path contains `query` (case-insensitive substring, not
/// prefix — unlike `slash_commands::matching_commands`, a file path is
/// long enough that "contains the typed fragment anywhere" (e.g. typing
/// "main" to find `crates/braze-cli/src/main.rs`) is far more useful
/// than requiring a prefix match against the full relative path.
pub fn matching_files<'a>(files: &'a [String], query: &str) -> Vec<&'a str> {
    let query = query.to_lowercase();
    files
        .iter()
        .filter(|path| path.to_lowercase().contains(&query))
        .map(String::as_str)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "braze-tui-mentions-test-{}-{label}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn lists_files_recursively_and_sorted() {
        let dir = temp_dir("lists_files_recursively_and_sorted");
        std::fs::write(dir.join("z.txt"), "").unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/a.txt"), "").unwrap();

        let files = list_files(&dir);
        assert_eq!(files, vec!["sub/a.txt".to_string(), "z.txt".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn excludes_heavy_directories_like_target_and_dot_git() {
        let dir = temp_dir("excludes_heavy_directories_like_target_and_dot_git");
        std::fs::create_dir_all(dir.join("target/debug")).unwrap();
        std::fs::write(dir.join("target/debug/braze"), "").unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".git/HEAD"), "").unwrap();
        std::fs::write(dir.join("Cargo.toml"), "").unwrap();

        let files = list_files(&dir);
        assert_eq!(files, vec!["Cargo.toml".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn matching_files_is_a_case_insensitive_substring_match() {
        let files = vec![
            "crates/braze-cli/src/main.rs".to_string(),
            "crates/braze-tui/src/app.rs".to_string(),
            "README.md".to_string(),
        ];
        let matches = matching_files(&files, "MAIN");
        assert_eq!(matches, vec!["crates/braze-cli/src/main.rs"]);
    }

    #[test]
    fn empty_query_matches_every_file() {
        let files = vec!["a.rs".to_string(), "b.rs".to_string()];
        assert_eq!(matching_files(&files, "").len(), 2);
    }
}
