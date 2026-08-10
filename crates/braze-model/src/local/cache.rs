//! Caché de modelo cargado (capacidad 1) y su clave. La clave captura
//! TODO lo que cambiaría la carga — ruta canónica, n_ctx, capas GPU
//! del caller y el entorno relevante; el bug #1 del piloto de
//! round-economics salió de omitir un eje. L-4: extraído VERBATIM de
//! `local.rs`.

use super::*;

/// Identidad de un modelo YA cargado. Dos `LocalBackend` que pidan el mismo
/// GGUF con la misma configuración de offload pueden compartir el
/// `LlamaModel`: los pesos son read-only (por eso ya viajaban en `Arc`) y el
/// contexto se sigue creando fresco por generación, así que reusar el modelo
/// no comparte estado de inferencia entre tareas.
///
/// El entorno entra en la clave porque el auto-fit lo consulta: cambiar
/// `BRAZE_LOCAL_GPU_LAYERS` o el margen debe forzar una recarga, no devolver
/// el modelo repartido con la configuración anterior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ModelCacheKey {
    path: PathBuf,
    n_ctx: u32,
    /// Override de capas GPU del llamador. Parte de la clave por la misma
    /// razón que `BRAZE_LOCAL_GPU_LAYERS`: dos brazos del mismo sweep que
    /// solo difieren en cuántas capas van a la GPU son DOS cargas
    /// distintas, y reusar la del primero le daría al segundo el precio
    /// de ronda del primero — el experimento entero medido al revés y sin
    /// una sola señal de que pasó.
    gpu_layers_override: Option<u32>,
    env: Vec<Option<String>>,
}

impl ModelCacheKey {
    fn new(gguf: &Path, n_ctx: u32, gpu_layers_override: Option<u32>) -> Self {
        const VARS: [&str; 6] = [
            "BRAZE_LOCAL_GPU_LAYERS",
            "BRAZE_LOCAL_AUTOFIT",
            "BRAZE_LOCAL_VRAM_MARGIN_MB",
            "BRAZE_LOCAL_KV_TYPE",
            "BRAZE_LOCAL_KV_OFFLOAD",
            "BRAZE_LOCAL_UBATCH",
        ];
        Self {
            // Canonicalizar para que `~/models/x.gguf` y una ruta relativa al
            // mismo archivo no carguen dos veces.
            path: gguf.canonicalize().unwrap_or_else(|_| gguf.to_path_buf()),
            n_ctx,
            gpu_layers_override,
            env: VARS.iter().map(|v| std::env::var(v).ok()).collect(),
        }
    }
}

/// Modelo cacheado. Capacidad **1 a propósito**: braze-bench corre un backend
/// a la vez y los modelos de esta escala pesan 6-12GB — mantener dos vivos
/// revienta la RAM/VRAM de Nitro, que es justo el fallo que el auto-fit vino
/// a eliminar.
type CachedModel = (
    ModelCacheKey,
    Arc<LlamaModel>,
    i32,
    GpuLayersSource,
    KvPlacement,
);
pub(super) static MODEL_CACHE: Mutex<Option<CachedModel>> = Mutex::new(None);

/// Carga el GGUF reusando el modelo cacheado si la clave coincide.
///
/// **Por qué existe**: braze-bench crea un `LocalBackend` por tarea, así que
/// un sweep de 57 tareas pagaba 57 veces el probe del auto-fit (que carga el
/// modelo con `no_alloc`) **y** 57 recargas del GGUF entero con su re-subida
/// de capas a la GPU — medido en vivo el 2026-07-25: la VRAM caía a ~177 MiB
/// entre tareas y volvía a ~4.7GB en cada una.
///
/// **Caveat metodológico**: con el caché activo solo la primera tarea de un
/// brazo paga la carga, así que el `wall_time_ms` promedio deja de ser
/// comparable contra sweeps anteriores. Para reproducir números viejos está
/// `BRAZE_LOCAL_MODEL_CACHE=off`.
pub(super) fn load_model_cached(
    backend: &Arc<LlamaBackend>,
    gguf: &Path,
    n_ctx: u32,
    gpu_layers_override: Option<u32>,
) -> Result<(Arc<LlamaModel>, i32, GpuLayersSource, KvPlacement), ModelError> {
    let load_fresh =
        || -> Result<(Arc<LlamaModel>, i32, GpuLayersSource, KvPlacement), ModelError> {
            let (params, gpu_layers, source, placement) =
                resolve_model_params(gguf, n_ctx, gpu_layers_override);
            let model = LlamaModel::load_from_file(backend, gguf, &params).map_err(|e| {
                ModelError::Request(format!("failed to load GGUF '{}': {e}", gguf.display()))
            })?;
            Ok((Arc::new(model), gpu_layers, source, placement))
        };

    if std::env::var("BRAZE_LOCAL_MODEL_CACHE").as_deref() == Ok("off") {
        return load_fresh();
    }

    let key = ModelCacheKey::new(gguf, n_ctx, gpu_layers_override);
    // El lock se sostiene durante la carga a propósito: si dos hilos piden el
    // mismo modelo a la vez, el segundo espera y reusa en vez de cargar un
    // duplicado de 12GB.
    let mut guard = MODEL_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((cached_key, model, gpu_layers, source, placement)) = guard.as_ref()
        && *cached_key == key
    {
        tracing::debug!(path = %gguf.display(), "local: modelo reusado del caché");
        return Ok((Arc::clone(model), *gpu_layers, *source, *placement));
    }
    // Soltar el modelo viejo ANTES de cargar el nuevo. Si algún `LocalBackend`
    // vivo todavía tiene su `Arc`, el modelo sobrevive hasta que lo suelte —
    // no se puede liberar lo que está en uso.
    *guard = None;
    let (model, gpu_layers, source, placement) = load_fresh()?;
    *guard = Some((key, Arc::clone(&model), gpu_layers, source, placement));
    Ok((model, gpu_layers, source, placement))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_clave_del_cache_distingue_modelo_y_contexto() {
        // Reusar un modelo cargado solo es correcto si la clave captura todo
        // lo que cambiaría su carga. Ruta, n_ctx y las capas GPU del caller
        // (`+ablate:gpu-layers` — el bug #1 del piloto de round-economics:
        // sin este eje, el segundo brazo del sweep reusaba el modelo del
        // primero y los dos precios de ronda se medían al mismo precio) son
        // los tres ejes; el entorno entra en `ModelCacheKey::new` y no se
        // testea acá porque mutarlo es `unsafe` en la edición 2024 y
        // contaminaría otros tests.
        let a = ModelCacheKey::new(Path::new("/models/x.gguf"), 8192, None);
        assert_eq!(
            a,
            ModelCacheKey::new(Path::new("/models/x.gguf"), 8192, None)
        );
        assert_ne!(
            a,
            ModelCacheKey::new(Path::new("/models/x.gguf"), 4096, None)
        );
        assert_ne!(
            a,
            ModelCacheKey::new(Path::new("/models/y.gguf"), 8192, None)
        );
        assert_ne!(
            a,
            ModelCacheKey::new(Path::new("/models/x.gguf"), 8192, Some(99))
        );
    }
}
