//! Run-level metadata attached to a sweep's JSON output (E6,
//! docs/AUDITORIA-2026-07-v3.md) — without this, a `results.json` file is
//! effectively unreproducible: nothing on disk records which sampling
//! params produced it, which suite version it ran against, which exact
//! Ollama model *weights* (not just the tag, which Ollama can silently
//! re-pull to something else under the same name) answered, or which
//! `braze` commit built the harness that ran it. A pass-rate number
//! without this context can't be compared against a later run with any
//! confidence that "same config" is actually true.

use serde::Serialize;

use crate::backend_spec::SamplingSpec;

#[derive(Debug, Clone, Serialize)]
pub struct RunMetadata {
    pub sampling: SamplingSpec,
    pub repetitions: u32,
    pub task_timeout_secs: u64,
    pub suite_path: String,
    /// Non-cryptographic fingerprint (`std::hash::DefaultHasher` over the
    /// suite file's raw bytes, hex-encoded) — enough to detect "the suite
    /// changed between two runs claiming to use it", not a security
    /// primitive; collision resistance isn't the point here.
    pub suite_fingerprint: String,
    /// `git rev-parse HEAD` of the working directory the sweep ran from,
    /// if available — `None` outside a git checkout (e.g. a source
    /// tarball) or if `git` itself isn't on `PATH`.
    pub braze_git_commit: Option<String>,
    /// One entry per distinct Ollama model referenced by any backend spec
    /// in this sweep (executor and/or planner), resolved via
    /// `braze_model::ollama_model_digest`. Empty when the sweep touches
    /// no Ollama backend.
    pub ollama_model_digests: Vec<OllamaModelDigest>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OllamaModelDigest {
    pub model: String,
    /// `None` when the model isn't installed under that exact name, or
    /// the Ollama server wasn't reachable — best-effort, never fails the
    /// sweep.
    pub digest: Option<String>,
}

/// Fingerprints `bytes` for [`RunMetadata::suite_fingerprint`] — see that
/// field's doc comment for why `DefaultHasher` (not a cryptographic hash)
/// is enough here.
pub fn fingerprint_bytes(bytes: &[u8]) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Best-effort `git rev-parse HEAD` — `None` on any failure (not a git
/// checkout, `git` missing, detached weirdness), never propagated as an
/// error: metadata is a diagnostic nicety, not something worth failing a
/// sweep over.
pub async fn current_git_commit() -> Option<String> {
    let output = tokio::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let commit = String::from_utf8(output.stdout).ok()?;
    let commit = commit.trim();
    if commit.is_empty() {
        None
    } else {
        Some(commit.to_string())
    }
}

/// Looks up the digest of every model in `models` against `base_url` —
/// best-effort per model (a lookup failure or missing model just yields
/// `digest: None` for that entry, never aborts the rest).
pub async fn collect_ollama_model_digests(
    base_url: &str,
    models: &[String],
) -> Vec<OllamaModelDigest> {
    let mut out = Vec::with_capacity(models.len());
    for model in models {
        let digest = braze_model::ollama_model_digest(base_url, model)
            .await
            .ok()
            .flatten();
        out.push(OllamaModelDigest {
            model: model.clone(),
            digest,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_stable_for_the_same_bytes() {
        let a = fingerprint_bytes(b"hola mundo");
        let b = fingerprint_bytes(b"hola mundo");
        assert_eq!(a, b);
    }

    #[test]
    fn fingerprint_differs_for_different_bytes() {
        let a = fingerprint_bytes(b"hola mundo");
        let b = fingerprint_bytes(b"chau mundo");
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn current_git_commit_inside_this_repo_is_a_40_char_hex_string() {
        // This test only runs meaningfully inside a real git checkout
        // (true for `cargo test` in this workspace) — asserts the
        // best-effort path returns something well-formed rather than
        // `None`, without asserting a specific commit hash.
        if let Some(commit) = current_git_commit().await {
            assert_eq!(commit.len(), 40, "expected a full SHA-1 hex string");
            assert!(commit.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }

    #[tokio::test]
    async fn collect_ollama_model_digests_is_none_for_an_unreachable_server() {
        let digests =
            collect_ollama_model_digests("http://127.0.0.1:1", &["qwen2.5:3b".to_string()]).await;
        assert_eq!(digests.len(), 1);
        assert_eq!(digests[0].model, "qwen2.5:3b");
        assert_eq!(digests[0].digest, None);
    }
}
