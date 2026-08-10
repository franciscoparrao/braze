//! Auto-fit del modelo al hardware: placement del KV cache, escalera
//! de contexto, parámetros de contexto/batch, resolución de capas GPU
//! (`GpuLayersSource`) y el reporte de tuning (`braze tune`). L-4:
//! extraído VERBATIM de `local.rs`.

use super::*;

/// Mapea el valor de `BRAZE_LOCAL_KV_TYPE` a un [`KvCacheType`]. Solo los
/// tipos que llama.cpp acepta para el KV cache (los k-quants por-bloque no
/// aplican al KV); `None` = desconocido → el caller deja el default `f16`.
pub(super) fn parse_kv_cache_type(s: &str) -> Option<KvCacheType> {
    match s.trim().to_ascii_lowercase().as_str() {
        "f16" => Some(KvCacheType::F16),
        "f32" => Some(KvCacheType::F32),
        "q8_0" => Some(KvCacheType::Q8_0),
        "q5_1" => Some(KvCacheType::Q5_1),
        "q5_0" => Some(KvCacheType::Q5_0),
        "q4_1" => Some(KvCacheType::Q4_1),
        "q4_0" => Some(KvCacheType::Q4_0),
        _ => None,
    }
}

/// Micro-batch FÍSICO del contexto (`BRAZE_LOCAL_UBATCH`). El buffer de
/// prompt-processing lo dimensiona `n_ubatch` × contexto y vive en VRAM; con
/// el default de llama.cpp (512) crece hasta reventar los 6GB al llenarse el
/// contexto. Ollama usa micro-batches chicos (VRAM plana) y lo replicamos.
pub(super) fn ubatch_setting() -> u32 {
    std::env::var("BRAZE_LOCAL_UBATCH")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|&u| u > 0)
        .unwrap_or(128)
}

/// Dónde vive el KV cache (y con él, el tamaño del micro-batch).
///
/// **Historia, porque explica el default.** `Host` nació el 2026-07-23 como
/// defensa contra un OOM de VRAM a mitad de sesión agéntica con gpt-oss:20b
/// en la RTX 3050 de 6GB, y se aplicaba SIEMPRE que hubiera offload. Medido
/// el 2026-07-25, ese incondicional costaba caro: gemma-4-12B a 14 capas
/// usaba 2477 MiB de 6144 —el KV cabía holgado en VRAM— y aun así pagaba el
/// camino lento. El brazo de control lo cuantificó: 29.2s por tarea contra
/// 16.1s del sweep del 21-jul, que corrió antes de que existiera esta
/// defensa.
///
/// Ahora la decisión la toma el auto-fit contra la VRAM **medida**: se
/// intenta primero `Device` (el default de llama.cpp, el camino rápido) y
/// solo se cae a `Host` si con el KV en VRAM no entra ninguna capa. Misma
/// jugada que la palanca #1 hizo con `n_gpu_layers`: cambiar una regla fija
/// por una medición.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KvPlacement {
    /// KV en VRAM + micro-batch default. Camino rápido.
    Device,
    /// KV en RAM del host + micro-batch chico. Libera VRAM para offloadear
    /// más capas, a costa de throughput.
    Host,
}

/// Escalera de degradación para crear el contexto, en orden de preferencia.
/// Cada escalón renuncia a una palanca: primero al KV **cuantizado**
/// (requiere flash-attn, que gpt-oss/Harmony no soporta), después a tener el
/// KV en **VRAM** (si no entra va al host: más lento, pero siempre cabe).
///
/// Es la red de seguridad de la medición del fit — cubre que se quede corta,
/// o que las capas vengan fijadas a mano por env sin medición ninguna. Desde
/// `Host` no se propone `Device`: si ya se midió que en VRAM no entra,
/// reintentarlo es chocar contra la misma pared.
pub(super) fn context_ladder(
    placement: KvPlacement,
    requested_kv: Option<KvCacheType>,
) -> Vec<(KvPlacement, Option<KvCacheType>)> {
    let mut ladder = vec![(placement, requested_kv)];
    if requested_kv.is_some() {
        ladder.push((placement, None));
    }
    if placement == KvPlacement::Device {
        if requested_kv.is_some() {
            ladder.push((KvPlacement::Host, requested_kv));
        }
        ladder.push((KvPlacement::Host, None));
    }
    ladder
}

/// Override explícito de `BRAZE_LOCAL_KV_OFFLOAD`: `gpu` fuerza `Device`,
/// `host` fuerza `Host`. Cualquier otra cosa (o ausencia) deja decidir al
/// fit. El valor `host` existe para poder ablacionar la palanca sin volver
/// a compilar.
pub(super) fn forced_kv_placement() -> Option<KvPlacement> {
    match std::env::var("BRAZE_LOCAL_KV_OFFLOAD").as_deref() {
        Ok("gpu") => Some(KvPlacement::Device),
        Ok("host") => Some(KvPlacement::Host),
        _ => None,
    }
}

/// Arma los `LlamaContextParams` de generación. Lo usan DOS lados que deben
/// coincidir: la creación real del contexto y el probe del auto-fit. Si el
/// probe midiera con otros parámetros, el fit repartiría capas contra un
/// consumo de VRAM que no es el que la generación va a tener realmente.
pub(super) fn build_ctx_params(
    n_ctx: u32,
    placement: KvPlacement,
    kv_type: Option<KvCacheType>,
) -> LlamaContextParams {
    let n_batch_max = n_ctx.max(256);
    let mut params =
        LlamaContextParams::default().with_n_ctx(std::num::NonZeroU32::new(n_batch_max));
    if placement == KvPlacement::Host {
        let ubatch = ubatch_setting();
        params = params
            .with_offload_kqv(false)
            .with_n_ubatch(ubatch)
            // `n_batch` (batch LÓGICO) cubre el prompt entero (braze lo
            // decodifica en chunks, si no `GGML_ASSERT n_tokens_all <= n_batch`).
            .with_n_batch(n_batch_max.max(ubatch));
    }
    if let Some(t) = kv_type {
        params = params.with_type_k(t).with_type_v(t);
    }
    params
}

/// `GGML_LOG_LEVEL_WARN`. `ggml_log_level` es un alias de `c_uint` en los
/// bindings y `llama-cpp-2` no re-exporta el crate `-sys`, así que el nivel
/// viaja como literal: los `LOG_INF`/`LOG_TRC` del fitting caen a debug y no
/// ensucian la TUI (nuestra propia traza del resultado dice lo que importa).
pub(super) const FIT_LOG_LEVEL: u32 = 3;

/// Margen de memoria que el auto-fit deja libre por device.
/// Default = 1 GiB, el mismo de llama.cpp upstream (`fit_params_target`);
/// override con `BRAZE_LOCAL_VRAM_MARGIN_MB` para exprimir o ser más
/// conservador según la tarjeta.
pub(super) fn fit_margin_bytes() -> usize {
    const DEFAULT_MARGIN_MIB: usize = 1024;
    let mib = std::env::var("BRAZE_LOCAL_VRAM_MARGIN_MB")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MARGIN_MIB);
    mib * 1024 * 1024
}

/// ¿El binario tiene backend de GPU y hay uno disponible? Sin esto no se
/// puede interpretar el `-1` que devuelve el fit: en un build con CUDA y
/// tarjeta presente significa "todas las capas", y en un build CPU-only
/// significa cero.
pub(super) fn backend_supports_gpu() -> bool {
    shared_llama_backend().is_ok_and(|b| b.supports_gpu_offload())
}

/// `common_fit_params` **no es thread-safe** (muta el logger global de
/// llama.cpp mientras corre). braze-bench crea un `LocalBackend` por tarea,
/// así que serializamos los fits entre sí.
pub(super) static FIT_LOCK: Mutex<()> = Mutex::new(());

/// De dónde salió el `n_gpu_layers` con el que se cargó el modelo. Se traza
/// para que un sweep pueda distinguir "el auto-fit eligió 24" de "el usuario
/// pidió 24" sin releer el entorno.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GpuLayersSource {
    /// `BRAZE_LOCAL_GPU_LAYERS` explícito — el usuario manda.
    Explicit,
    /// El auto-fit de llama.cpp repartió capas contra la VRAM libre.
    AutoFit,
    /// Auto-fit apagado (`BRAZE_LOCAL_AUTOFIT=off`) → CPU puro.
    Disabled,
    /// El auto-fit falló; se degradó a CPU en vez de crashear.
    FitFailed,
}

/// Resuelve `n_gpu_layers` y devuelve los `LlamaModelParams` listos para
/// cargar. El auto-fit (idea #1 de `docs/inference-runtimes-audit-2026-07-25.md`)
/// delega en `common_fit_params` de libcommon — el MISMO algoritmo que usa
/// `llama-cli` con `--fit` (default upstream: `fit_params = true`): mide la
/// VRAM libre por device con un probe `no_alloc`, llena capas densas
/// back-to-front dejando el margen, y manda los tensores MoE sobrantes a
/// RAM vía `tensor_buft_overrides`.
///
/// Sustituye al `BRAZE_LOCAL_GPU_LAYERS` adivinado a mano: ese número, si se
/// pasaba de largo, no daba un error legible sino un OOM de CUDA que mata el
/// proceso a mitad de sweep.
///
/// Precedencia: env explícito > auto-fit > CPU. Cualquier fallo del fit
/// degrada a CPU puro con un warn (filosofía degrade-not-crash del proyecto).
pub(super) fn resolve_model_params(
    gguf: &Path,
    n_ctx: u32,
    gpu_layers_override: Option<u32>,
) -> (
    Pin<Box<LlamaModelParams>>,
    i32,
    GpuLayersSource,
    KvPlacement,
) {
    let forced = forced_kv_placement();
    // Precedencia: override del llamador > env > auto-fit > CPU. El
    // override es POR BACKEND y el env es del proceso entero, así que el
    // más específico manda — es lo que deja correr dos precios de ronda
    // (GPU y CPU) como dos brazos del MISMO sweep de braze-bench
    // (`+ablate:gpu-layers=N`, línea round-economics) en vez de dos
    // corridas separadas.
    if let Some(n) = gpu_layers_override.or_else(|| {
        std::env::var("BRAZE_LOCAL_GPU_LAYERS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
    }) {
        let layers = i32::try_from(n).unwrap_or(i32::MAX);
        let params = Box::pin(LlamaModelParams::default().with_n_gpu_layers(n));
        tracing::info!(
            gpu_layers = layers,
            source = if gpu_layers_override.is_some() {
                "caller"
            } else {
                "BRAZE_LOCAL_GPU_LAYERS"
            },
            "local: n_gpu_layers explícito — auto-fit omitido"
        );
        // Sin fit no hay medición, así que se usa el default rápido
        // (`Device`, el de llama.cpp) salvo override. La red de seguridad es
        // la escalera de `generate_blocking`: si el contexto no entra en
        // VRAM, cae a `Host` sola.
        return (
            params,
            layers,
            GpuLayersSource::Explicit,
            forced.unwrap_or(KvPlacement::Device),
        );
    }

    if std::env::var("BRAZE_LOCAL_AUTOFIT").as_deref() == Ok("off") {
        tracing::info!("local: auto-fit desactivado (BRAZE_LOCAL_AUTOFIT=off) — CPU puro");
        return (
            Box::pin(LlamaModelParams::default().with_n_gpu_layers(0u32)),
            0,
            GpuLayersSource::Disabled,
            KvPlacement::Device,
        );
    }

    let Some(path_str) = gguf.to_str() else {
        tracing::warn!(
            path = %gguf.display(),
            "local: ruta del GGUF no es UTF-8 — auto-fit omitido, CPU puro"
        );
        return (
            Box::pin(LlamaModelParams::default().with_n_gpu_layers(0u32)),
            0,
            GpuLayersSource::FitFailed,
            KvPlacement::Device,
        );
    };
    let Ok(c_path) = std::ffi::CString::new(path_str) else {
        tracing::warn!("local: ruta del GGUF con byte nulo — auto-fit omitido, CPU puro");
        return (
            Box::pin(LlamaModelParams::default().with_n_gpu_layers(0u32)),
            0,
            GpuLayersSource::FitFailed,
            KvPlacement::Device,
        );
    };

    let kv_type = std::env::var("BRAZE_LOCAL_KV_TYPE")
        .ok()
        .and_then(|kv| parse_kv_cache_type(&kv));

    // Dos intentos: con el KV pedido y, si falla, con f16. El KV cuantizado
    // requiere flash-attn, que gpt-oss/Harmony NO soporta — sin este
    // reintento un `BRAZE_LOCAL_KV_TYPE` no soportado haría fracasar el fit
    // y perderíamos la GPU entera por una palanca ortogonal.
    let attempts: &[Option<KvCacheType>] = if kv_type.is_some() {
        &[kv_type, None]
    } else {
        &[None]
    };

    // Orden de placements: el camino RÁPIDO primero. `Host` solo se prueba si
    // con el KV en VRAM no entra ninguna capa — o sea, se paga el throughput
    // del KV en host únicamente cuando la VRAM medida obliga, no por regla.
    let placements: &[KvPlacement] = match forced {
        Some(p) => std::slice::from_ref(match p {
            KvPlacement::Device => &KvPlacement::Device,
            KvPlacement::Host => &KvPlacement::Host,
        }),
        None => &[KvPlacement::Device, KvPlacement::Host],
    };

    let margin = fit_margin_bytes();
    // Si `Device` fitea pero sin capas, se guarda antes de probar `Host`: si
    // `Host` tampoco consigue offload, esto es CPU puro y da igual dónde viva
    // el KV, así que se devuelve el rápido.
    let mut device_sin_capas: Option<Pin<Box<LlamaModelParams>>> = None;

    for &placement in placements {
        for (attempt, kv) in attempts.iter().enumerate() {
            let mut params = Box::pin(LlamaModelParams::default());
            let mut cparams = build_ctx_params(n_ctx, placement, *kv);
            let mut margins = vec![margin; llama_cpp_2::max_devices().max(1)];
            let result = {
                let _guard = FIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
                params.as_mut().fit_params(
                    &c_path,
                    &mut cparams,
                    &mut margins,
                    n_ctx,
                    FIT_LOG_LEVEL,
                )
            };
            match result {
                Ok(fit) => {
                    // Sin GPU disponible el fit no toca `n_gpu_layers` y lo
                    // deja en su default `-1`. Ese `-1` significa "todas las
                    // que quepan", que sin device es CERO — tomado literal
                    // haría que se active el camino de VRAM en una corrida de
                    // CPU puro (bug cazado en vivo con `braze tune` en la
                    // máquina sin GPU, 2026-07-25).
                    let layers = if backend_supports_gpu() {
                        params.n_gpu_layers()
                    } else {
                        0
                    };
                    if layers == 0 {
                        // Sin capas en GPU el placement da igual para la VRAM
                        // (no hay nada allá) pero NO para la velocidad:
                        // `Host` achica el micro-batch y eso frena el prefill
                        // en CPU. Así que 0 capas SIEMPRE termina en el
                        // camino rápido. Sin esta guarda, una máquina sin GPU
                        // caía a `Host` y reintroducía la regresión de CPU
                        // que se arregló normalizando el `-1` — misma clase
                        // de bug por otro camino (cazado en vivo, 2026-07-25).
                        if placement == KvPlacement::Device
                            && placements.len() > 1
                            && backend_supports_gpu()
                        {
                            tracing::info!(
                                "local: con el KV en VRAM no entra ninguna capa — probando KV en host"
                            );
                            device_sin_capas = Some(params);
                            break; // siguiente placement
                        }
                        let params = device_sin_capas.take().unwrap_or(params);
                        tracing::info!(
                            "local: sin offload a GPU — KV en VRAM (camino rápido) por defecto"
                        );
                        return (params, 0, GpuLayersSource::AutoFit, KvPlacement::Device);
                    }
                    tracing::info!(
                        gpu_layers = layers,
                        fitted_n_ctx = fit.n_ctx,
                        margin_mib = margin / (1024 * 1024),
                        kv_quantized = kv.is_some(),
                        ?placement,
                        "local: auto-fit resolvió el offload a GPU contra la VRAM libre"
                    );
                    return (params, layers, GpuLayersSource::AutoFit, placement);
                }
                Err(e) if attempt + 1 < attempts.len() => {
                    tracing::warn!(
                        error = %e,
                        "local: auto-fit falló con KV cuantizado; reintentando con f16"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        ?placement,
                        "local: auto-fit falló — se carga en CPU puro (degradar, no crashear). \
                         Forzar capas con BRAZE_LOCAL_GPU_LAYERS si la GPU sí sirve."
                    );
                }
            }
        }
    }

    if let Some(params) = device_sin_capas {
        tracing::info!("local: ni con el KV en host entra offload — CPU puro");
        return (params, 0, GpuLayersSource::AutoFit, KvPlacement::Device);
    }

    (
        Box::pin(LlamaModelParams::default().with_n_gpu_layers(0u32)),
        0,
        GpuLayersSource::FitFailed,
        KvPlacement::Device,
    )
}

// El preámbulo de tools de las familias textuales (formato nativo de
// qwen2.5, reusado como convención instruida para Gemma) vive en
// `gemma::render_tools_preamble` — módulo puro, compartido y testeado.

/// Lo que el auto-fit resolvió para un GGUF, **sin cargarlo para
/// inferencia**. Es la salida de [`tune_model`], que alimenta `braze tune`
/// (idea #8 de `docs/inference-runtimes-audit-2026-07-25.md`).
#[derive(Debug, Clone)]
pub struct TuneReport {
    /// El GGUF que se midió, con la ruta ya canonicalizada.
    pub gguf: PathBuf,
    /// Contexto contra el que se fiteó — el reparto depende de él.
    pub n_ctx: u32,
    /// Capas a GPU (convención llama.cpp: 0 = CPU, negativo = todas).
    pub n_gpu_layers: i32,
    /// Cómo se llegó al número: auto-fit, env explícito, o degradación.
    pub source: &'static str,
    /// Margen de VRAM por device que se dejó libre.
    pub margin_mib: usize,
    /// Dónde quedó el KV cache: `"device"` (VRAM, camino rápido) u `"host"`
    /// (RAM, más lento pero libera VRAM para más capas). Desde 2026-07-25 lo
    /// decide el fit midiendo, así que sin este campo el reporte no
    /// describiría la configuración que realmente se va a correr.
    pub kv_placement: &'static str,
}

/// Corre el auto-fit sobre un GGUF y reporta el reparto, sin cargar los
/// pesos para inferencia (el probe usa `no_alloc`). Sirve para **fitear una
/// vez y fijar el número**: el sweep siguiente exporta
/// `BRAZE_LOCAL_GPU_LAYERS` y se ahorra el fit por tarea, además de quedar
/// reproducible en vez de re-adivinado.
///
/// # Errors
/// Devuelve error si el GGUF no existe o si el backend de llama.cpp no
/// pudo inicializarse.
pub fn tune_model(gguf: impl AsRef<Path>, n_ctx: u32) -> Result<TuneReport, ModelError> {
    let gguf = gguf.as_ref();
    if !gguf.exists() {
        return Err(ModelError::Request(format!(
            "el GGUF no existe: {}",
            gguf.display()
        )));
    }
    // El fit carga un modelo con `no_alloc`, así que el backend global tiene
    // que estar inicializado (y esto además rutea los logs de llama.cpp a
    // `tracing`, para que el probe no escupa a stderr).
    let _backend = shared_llama_backend()?;
    // `None`: `tune_model` existe justamente para MEDIR qué resuelve el
    // auto-fit (o qué fijó el env) y poder fijar ese número después —
    // pasarle un override acá haría que el reporte describiera el
    // override en vez de la máquina.
    let (_params, n_gpu_layers, source, placement) = resolve_model_params(gguf, n_ctx, None);
    Ok(TuneReport {
        gguf: gguf.canonicalize().unwrap_or_else(|_| gguf.to_path_buf()),
        n_ctx,
        n_gpu_layers,
        source: match source {
            GpuLayersSource::Explicit => "explicit",
            GpuLayersSource::AutoFit => "auto-fit",
            GpuLayersSource::Disabled => "disabled",
            GpuLayersSource::FitFailed => "fit-failed",
        },
        margin_mib: fit_margin_bytes() / (1024 * 1024),
        kv_placement: match placement {
            KvPlacement::Device => "device",
            KvPlacement::Host => "host",
        },
    })
}

impl TuneReport {
    /// Renderiza el reporte como TOML. Los comentarios mapean cada valor a
    /// su variable de entorno porque **esa es la vía de consumo**: braze no
    /// lee este archivo, se pega el `export` y el reparto queda fijado.
    #[must_use]
    pub fn to_toml(&self) -> String {
        format!(
            "# Generado por `braze tune` — reparto resuelto por el auto-fit.\n\
             # braze NO lee este archivo: es un registro reproducible del fit.\n\
             # Para fijarlo en un sweep, exportá las variables comentadas.\n\
             \n[local]\n\
             model = \"{}\"\n\
             n_ctx = {}\n\
             n_gpu_layers = {}      # BRAZE_LOCAL_GPU_LAYERS\n\
             vram_margin_mb = {}    # BRAZE_LOCAL_VRAM_MARGIN_MB\n\
             kv_placement = \"{}\"   # BRAZE_LOCAL_KV_OFFLOAD=gpu|host\n\
             source = \"{}\"\n",
            self.gguf.display(),
            self.n_ctx,
            self.n_gpu_layers,
            self.margin_mib,
            self.kv_placement,
            self.source,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kv_cache_type_parses_known_quant_types() {
        assert_eq!(parse_kv_cache_type("q8_0"), Some(KvCacheType::Q8_0));
        assert_eq!(parse_kv_cache_type("Q4_0"), Some(KvCacheType::Q4_0));
        assert_eq!(parse_kv_cache_type(" f16 "), Some(KvCacheType::F16));
        assert_eq!(parse_kv_cache_type("q5_1"), Some(KvCacheType::Q5_1));
    }

    #[test]
    fn kv_cache_type_unknown_falls_back_to_none() {
        assert_eq!(parse_kv_cache_type("q3_k"), None);
        assert_eq!(parse_kv_cache_type("nonsense"), None);
        assert_eq!(parse_kv_cache_type(""), None);
    }

    #[test]
    fn el_contexto_respeta_el_piso_de_256_tokens() {
        let chico = build_ctx_params(64, KvPlacement::Device, None);
        assert_eq!(chico.n_ctx().map(std::num::NonZeroU32::get), Some(256));
        let normal = build_ctx_params(4096, KvPlacement::Device, None);
        assert_eq!(normal.n_ctx().map(std::num::NonZeroU32::get), Some(4096));
    }

    #[test]
    fn el_placement_del_kv_decide_offload_y_micro_batch() {
        // `Device` es el camino rápido: KV en VRAM y los batches default de
        // llama.cpp. `Host` es el que renuncia a throughput para liberar
        // VRAM. Que el micro-batch viaje pegado al placement es deliberado:
        // los dos se introdujeron juntos (483f8e2) y medirlos por separado
        // no tendría sentido, porque el buffer de prompt-processing vive en
        // VRAM igual que el KV.
        let device = build_ctx_params(4096, KvPlacement::Device, None);
        assert!(device.offload_kqv(), "Device deja el KV en VRAM");
        let host = build_ctx_params(4096, KvPlacement::Host, None);
        assert!(!host.offload_kqv(), "Host saca el KV de la VRAM");
        assert!(
            host.n_ubatch() < device.n_ubatch(),
            "Host achica el micro-batch"
        );
        assert!(host.n_batch() >= host.n_ubatch());
    }

    #[test]
    fn la_escalera_de_contexto_degrada_en_orden_y_nunca_vuelve_a_subir() {
        // Sustituye al test del acoplamiento `kv_on_host(layers)`, que murió
        // cuando el placement pasó a resolverse por medición en vez de por
        // regla. Lo que importa ahora es el ORDEN de renuncias.
        let d_quant = context_ladder(KvPlacement::Device, Some(KvCacheType::Q8_0));
        assert_eq!(
            d_quant,
            vec![
                (KvPlacement::Device, Some(KvCacheType::Q8_0)),
                (KvPlacement::Device, None),
                (KvPlacement::Host, Some(KvCacheType::Q8_0)),
                (KvPlacement::Host, None),
            ],
            "primero se suelta el KV cuantizado, después la VRAM"
        );

        // Sin KV cuantizado pedido no hay escalón que renuncie a él.
        assert_eq!(
            context_ladder(KvPlacement::Device, None),
            vec![(KvPlacement::Device, None), (KvPlacement::Host, None)]
        );

        // Desde Host no se sube a Device: si el fit ya midió que en VRAM no
        // entra, reintentarlo sería volver a chocar contra la misma pared.
        for kv in [None, Some(KvCacheType::Q8_0)] {
            let ladder = context_ladder(KvPlacement::Host, kv);
            assert!(
                ladder.iter().all(|(p, _)| *p == KvPlacement::Host),
                "una escalera que arranca en Host no debe proponer Device"
            );
        }

        // En todos los casos el primer escalón es exactamente lo pedido, y
        // el último es el más conservador.
        for (p, kv) in [
            (KvPlacement::Device, None),
            (KvPlacement::Device, Some(KvCacheType::Q4_0)),
            (KvPlacement::Host, Some(KvCacheType::Q4_0)),
        ] {
            let ladder = context_ladder(p, kv);
            assert_eq!(ladder.first(), Some(&(p, kv)));
            assert_eq!(ladder.last(), Some(&(KvPlacement::Host, None)));
        }
    }
}
