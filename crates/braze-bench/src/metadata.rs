//! Run-level metadata attached to a sweep's JSON output (E6,
//! docs/AUDITORIA-2026-07-v3.md) — without this, a `results.json` file is
//! effectively unreproducible: nothing on disk records which sampling
//! params produced it, which suite version it ran against, which exact
//! Ollama model *weights* (not just the tag, which Ollama can silently
//! re-pull to something else under the same name) answered, or which
//! `braze` commit built the harness that ran it. A pass-rate number
//! without this context can't be compared against a later run with any
//! confidence that "same config" is actually true.

use serde::{Deserialize, Serialize};

use crate::backend_spec::SamplingSpec;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunMetadata {
    pub sampling: SamplingSpec,
    pub repetitions: u32,
    /// Backstop de infraestructura por intento de tarea. NO es el
    /// presupuesto experimental — ver `turn_wall_clock_secs`.
    pub task_timeout_secs: u64,
    /// Presupuesto de wall-clock por turno de la línea round-economics
    /// (`--turn-wall-clock-secs`), cuando el sweep corrió con uno.
    /// Sin este campo, un `results.json` de un sweep a tiempo fijo es
    /// indistinguible de uno a rondas fijas — que es la diferencia entera
    /// entre los dos regímenes que esa línea compara.
    pub turn_wall_clock_secs: Option<u64>,
    /// Deadline de wall-clock por ronda (`--round-wall-clock-secs`),
    /// cuando el sweep corrió con uno — misma razón de procedencia que
    /// `turn_wall_clock_secs`: sin él, un sweep con rondas acotadas a
    /// nivel de streaming es indistinguible de uno sin cota.
    pub round_wall_clock_secs: Option<u64>,
    pub suite_path: String,
    /// Non-cryptographic fingerprint (`std::hash::DefaultHasher` over the
    /// suite file's raw bytes, hex-encoded) — enough to detect "the suite
    /// changed between two runs claiming to use it", not a security
    /// primitive; collision resistance isn't the point here.
    pub suite_fingerprint: String,
    /// El commit de `braze` que construyó el binario del bench, con
    /// sufijo `-dirty` si el árbol tenía cambios sin commitear al
    /// compilar.
    ///
    /// Se embebe en tiempo de build (`build.rs`) y solo cae a
    /// `git rev-parse HEAD` del cwd si el build-time no está disponible —
    /// ver [`resolve_git_commit`]. Antes se capturaba SOLO en runtime, lo
    /// que dejaba sin procedencia todo sweep corrido desde un directorio
    /// que no fuera un checkout git: en Nitro, `~/braze` es una copia sin
    /// `.git`, así que el campo salía `null` en los sweeps del nodo donde
    /// corre el grueso de los experimentos (verificado en el A/B de
    /// weight-quant, 2026-08). `None` solo si ambos caminos fallan.
    pub braze_git_commit: Option<String>,
    /// Identidad del motor de inferencia in-process (`llama-cpp-2 <ver>`,
    /// más `+cuda` en el build con offload GPU) cuando el sweep corrió
    /// algún backend `local:`; `None` si no.
    ///
    /// La condición es que ALGUNA mitad de algún spec use el
    /// `LocalBackend` —no que el binario traiga el feature `local`
    /// compilado— porque el campo describe la corrida, no el ejecutable:
    /// un sweep enteramente servido no debe registrar (ni driftear por)
    /// un motor que nunca generó un token. Ver
    /// `BackendSpec::uses_local_backend`.
    ///
    /// Contraparte de `ollama_server_version` para el `LocalBackend`: ese
    /// campo identifica la capa de servicio cuando el modelo vive detrás
    /// de un servidor, pero con llama.cpp linkeado in-process no hay
    /// servidor al que preguntarle — la versión solo existe en el binario.
    /// Sin este campo, un sweep `local:` sub-especificaba su propio motor,
    /// que es la variable que más se mueve del stack: llama.cpp cambia
    /// kernels, cuantización y decodificación entre releases, así que dos
    /// corridas con bindings distintos no son la misma condición aunque
    /// coincidan modelo, seed y sampling. Omitido del JSON cuando es
    /// `None`, igual que el resto de campos best-effort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_version: Option<String>,
    /// One entry per distinct Ollama model referenced by any backend spec
    /// in this sweep (executor, planner, and/or lead), resolved via
    /// `braze_model::ollama_model_digest`. Empty when the sweep touches
    /// no Ollama backend.
    pub ollama_model_digests: Vec<OllamaModelDigest>,
    /// The Ollama server's own version (`GET /api/version`), when the
    /// sweep touches an Ollama backend and the server answered — the
    /// serving-layer identity earlier sweeps' metadata was missing
    /// (EMSE blind review b2, Issue 3, 2026-07-19): chat-template
    /// rendering changes across Ollama releases, and braze's own
    /// planner findings locate a mechanism precisely in that layer, so
    /// model digests without a server version under-specify the
    /// serving stack. `None` when no Ollama backend is involved or the
    /// lookup failed — best-effort, same posture as the digests.
    pub ollama_server_version: Option<String>,
    /// The full display name of every backend row this sweep ran —
    /// executor, `+plan:`/`+lead:` halves, AND the `+ablate:` suffix
    /// with every active ablation key (H-17,
    /// docs/AUDITORIA-2026-07-v5.md): without this, nothing at the run
    /// level said "this sweep was an ablation experiment at all" — the
    /// suffix only survived inside each row's `backend` string, where a
    /// reader aggregating across sweeps can silently mix ablated and
    /// unablated rows. Same order `--backends` listed them in.
    pub backend_specs: Vec<String>,
    /// v9 L-1 (docs/AUDITORIA-2026-07-v9.md): every `BRAZE_LOCAL_*`
    /// variable set in the sweep's environment, plus
    /// `BRAZE_VERIFY_COMMAND` — the deliberately env-only deployment
    /// tier (per-machine tuning: GPU layers, VRAM margin, KV type,
    /// sampling overrides, the verification lever). These knobs change
    /// results but never pass through the config file, so without this
    /// map a sweep's JSON under-specified the configuration that
    /// produced it — the same class of gap as the missing
    /// `ollama_server_version` before EMSE b2/Issue 3. Deterministic
    /// order (BTreeMap) so two sweeps with the same env serialize
    /// identically. Empty when none are set (the common case away from
    /// the LocalBackend), and omitted from the JSON then.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub local_env: std::collections::BTreeMap<String, String>,
    /// El `keep_alive` por-request efectivo del sweep (`--keep-alive` o
    /// `ollama_keep_alive` de config/env), cuando hubo uno — procedencia
    /// de la política de residencia: bajo presión de RAM cambia qué
    /// modelos conviven residentes, o sea la clase de [Timeout]/OOM del
    /// incidente Nitro 2026-08-10, sin tocar ni un token de la
    /// generación. `None` (omitido del JSON) = mandó la config del
    /// server, el régimen de todo sweep anterior a este campo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ollama_keep_alive: Option<String>,
    /// Semántica de grading del sweep (decisión de banco 2026-08-12):
    /// `Some(GRADING_FUNCTIONAL_DUAL)` desde que `passed` es la métrica
    /// funcional y `passed_strict` viaja aparte. `None` = results.json
    /// anterior al cambio, donde `passed` ERA estricto — comparar un ref
    /// viejo contra una corrida nueva cruza semánticas distintas
    /// exactamente en las filas clase e4b/ornith, así que DBV lo trata
    /// como drift.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grading: Option<String>,
}

/// El identificador de la semántica de grading vigente — ver
/// [`RunMetadata::grading`].
pub const GRADING_FUNCTIONAL_DUAL: &str = "functional-primary+strict-secondary/2026-08-12";

/// Collects the env-only deployment tier for [`RunMetadata::local_env`]
/// from an explicit iterator — the same testability pattern as
/// `braze_config::Config::load_with` (env-var tests must not read or
/// mutate the real process environment; parallel tests race on it).
pub fn collect_local_env(
    vars: impl Iterator<Item = (String, String)>,
) -> std::collections::BTreeMap<String, String> {
    vars.filter(|(k, _)| k.starts_with("BRAZE_LOCAL_") || k == "BRAZE_VERIFY_COMMAND")
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// El commit embebido al compilar el binario (`build.rs`), o `None` si el
/// build no pudo determinarlo (compilado desde un tarball, `git` ausente).
///
/// Es la fuente PREFERIDA de procedencia del harness porque describe el
/// ejecutable, no el directorio desde el que se lo lanzó — ver
/// [`resolve_git_commit`].
pub fn build_git_commit() -> Option<String> {
    let commit = env!("BRAZE_BUILD_GIT_COMMIT");
    (!commit.is_empty()).then(|| commit.to_string())
}

/// Procedencia del harness: el commit de build-time y, solo si ese no
/// existe, el `git rev-parse HEAD` del cwd.
///
/// La precedencia importa y no es arbitraria. El commit de build-time
/// describe *qué código corrió*; el de runtime describe *desde dónde se
/// lanzó*, y los dos se separan en los dos casos que se dan en la
/// práctica: un binario que no se recompiló tras avanzar HEAD (el runtime
/// atribuye el sweep a código que no corrió), y un binario copiado a la
/// máquina de benchmark sin el árbol de fuentes (el runtime no devuelve
/// nada). El fallback a runtime conserva el comportamiento anterior para
/// el caso en que el binario venga de un tarball pero se corra dentro de
/// un checkout.
pub async fn resolve_git_commit() -> Option<String> {
    match build_git_commit() {
        Some(commit) => Some(commit),
        None => current_git_commit().await,
    }
}

/// Best-effort `git rev-parse HEAD` of the working directory — `None` on
/// any failure (not a git checkout, `git` missing, detached weirdness),
/// never propagated as an error: metadata is a diagnostic nicety, not
/// something worth failing a sweep over.
///
/// Fallback de [`resolve_git_commit`]; preferir esa función, que sabe
/// distinguir el commit del binario del commit del cwd.
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

    /// v9 L-1: only the env-only deployment tier is captured — anything
    /// else in the environment (other BRAZE_* vars already covered by
    /// the config system, unrelated vars, secrets like API keys) must
    /// NOT leak into a sweep JSON that gets committed to a public repo.
    #[test]
    fn collect_local_env_captures_the_tier_and_nothing_else() {
        let vars = [
            ("BRAZE_LOCAL_GPU_LAYERS", "25"),
            ("BRAZE_LOCAL_KV_TYPE", "q8_0"),
            ("BRAZE_VERIFY_COMMAND", "cargo check"),
            ("BRAZE_ANTHROPIC_API_KEY", "sk-ant-secret"),
            ("BRAZE_OLLAMA_BASE_URL", "http://192.168.1.8:11434"),
            ("PATH", "/usr/bin"),
        ];
        let got = collect_local_env(vars.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        assert_eq!(got.len(), 3, "got: {got:?}");
        assert_eq!(got["BRAZE_LOCAL_GPU_LAYERS"], "25");
        assert_eq!(got["BRAZE_LOCAL_KV_TYPE"], "q8_0");
        assert_eq!(got["BRAZE_VERIFY_COMMAND"], "cargo check");
        assert!(
            !got.keys().any(|k| k.contains("API_KEY")),
            "an API key must never travel into sweep metadata"
        );
    }

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

    /// El commit embebido debe estar bien formado: SHA-1 hex completo,
    /// opcionalmente con el sufijo `-dirty`. Un valor mal formado acá es
    /// peor que `None` — parece procedencia sin serlo.
    #[test]
    fn build_git_commit_is_well_formed_when_present() {
        let Some(commit) = build_git_commit() else {
            return; // build fuera de un checkout git — caso legítimo
        };
        let sha = commit.strip_suffix("-dirty").unwrap_or(&commit);
        assert_eq!(sha.len(), 40, "expected a full SHA-1 hex string: {commit}");
        assert!(
            sha.chars().all(|c| c.is_ascii_hexdigit()),
            "got: {commit}"
        );
    }

    /// La razón de ser del cambio: compilado dentro de este workspace, la
    /// procedencia del harness NO puede depender de que el cwd del sweep
    /// sea un checkout git — ese era exactamente el modo de falla en Nitro
    /// (`~/braze` es una copia sin `.git` y el campo salía `null`).
    #[tokio::test]
    async fn resolve_git_commit_prefers_the_embedded_build_commit() {
        if let Some(embedded) = build_git_commit() {
            assert_eq!(
                resolve_git_commit().await,
                Some(embedded),
                "el build-time debe ganarle al runtime, no al revés"
            );
        }
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
