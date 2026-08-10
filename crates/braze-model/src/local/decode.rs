//! El loop de decode: prefill en chunks de n_batch + generación token
//! a token, con cancelación por consumidor caído (`tx.is_closed()`)
//! en los dos loops, guarda de turno vacío, stencil y marcadores
//! Harmony. Corre bloqueante en `spawn_blocking`. L-4: extraído
//! VERBATIM de `local.rs`.

use super::*;

/// Traduce un [`HarmonyEvent`] del parser a su `CompletionEvent` y lo
/// empuja. Devuelve `false` si el consumidor abandonó (cancelación).
pub(super) fn emit_harmony_event(
    event: HarmonyEvent,
    tx: &tokio::sync::mpsc::Sender<Result<CompletionEvent, ModelError>>,
) -> bool {
    match event {
        HarmonyEvent::Visible(text) => tx
            .blocking_send(Ok(CompletionEvent::TextDelta(text)))
            .is_ok(),
        HarmonyEvent::ToolCall { name, raw_args } => {
            let (arguments, outcome) = parse_arguments_with_repair(&raw_args);
            if !matches!(outcome, ArgumentsOutcome::Parsed) {
                tracing::warn!(
                    tool = %name,
                    ?outcome,
                    "harmony: argumentos de tool call reparados/colapsados"
                );
            }
            let id = format!(
                "local-tool-call-{}-{}",
                crate::synth_id::process_nonce(),
                TOOL_CALL_COUNTER.fetch_add(1, Ordering::Relaxed)
            );
            tx.blocking_send(Ok(CompletionEvent::ToolCallRequested {
                id,
                name,
                arguments,
            }))
            .is_ok()
        }
    }
}

/// Los knobs numéricos del turno de generación. Agrupados por la misma
/// razón que [`FamilyRuntime`]: `generate_blocking` acumulaba parámetros
/// sueltos. `gpu_layers` viaja acá (y no se relee del entorno) para que el
/// contexto se arme contra el MISMO reparto de capas que midió el auto-fit
/// al cargar el modelo.
#[derive(Debug, Clone, Copy)]
pub(super) struct GenParams {
    pub(super) n_ctx: u32,
    pub(super) max_tokens: u32,
    pub(super) gpu_layers: i32,
    pub(super) placement: KvPlacement,
    pub(super) sampling: LocalSampling,
}

/// Genera de forma bloqueante y empuja eventos por `tx`. Corre en un hilo
/// de `spawn_blocking`. Convención de error: cualquier fallo se manda
/// como `Err` por el canal (el stream lo propaga como `StreamError`).
///
/// Con `harmony: Some(_)` la salida se interpreta como mensajes Harmony:
/// los tokens especiales se matchean por id (nunca se renderizan), el
/// canal `final` fluye como `TextDelta`, `analysis` se traza y suprime, y
/// `<|call|>`/`<|return|>` cierran el turno con su `stop_reason` honesto
/// (`tool_use`/`stop`; presupuesto agotado = `length`).
pub(super) fn generate_blocking(
    backend: &LlamaBackend,
    model: &LlamaModel,
    prompt: &str,
    gen_params: GenParams,
    family: &FamilyRuntime,
    tx: &tokio::sync::mpsc::Sender<Result<CompletionEvent, ModelError>>,
) {
    let GenParams {
        n_ctx,
        max_tokens,
        gpu_layers,
        placement,
        sampling,
    } = gen_params;
    let (harmony, tools) = match family {
        FamilyRuntime::Harmony { ids, tools } => (Some(ids), tools.as_slice()),
        FamilyRuntime::ChatMl { tools } => (None, tools.as_slice()),
    };
    macro_rules! bail {
        ($($arg:tt)*) => {{
            let _ = tx.blocking_send(Err(ModelError::StreamError(format!($($arg)*))));
            return;
        }};
    }

    // KV cache cuantizado (`BRAZE_LOCAL_KV_TYPE=q8_0|q4_0|q5_0|q5_1|q4_1`, idea
    // #2 de `docs/inference-runtimes-audit-2026-07-25.md`): baja el footprint
    // del KV (RAM host, o VRAM con `BRAZE_LOCAL_KV_OFFLOAD=gpu` → más capas).
    // Default `f16` — palanca opt-in que gana su default por bench. **Verificado
    // en vivo 2026-07-25**: el KV cuantizado requiere flash-attn, que
    // gpt-oss/Harmony NO soportan (attention sinks) → `new_context` devuelve
    // null; qwen2.5:3b sí funciona. Por eso degradamos con gracia a f16 abajo si
    // falla, en vez de crashear (filosofía degrade-not-crash del proyecto).
    let requested_kv = std::env::var("BRAZE_LOCAL_KV_TYPE").ok().and_then(|kv| {
        parse_kv_cache_type(&kv).or_else(|| {
            tracing::warn!(kv_type = %kv, "BRAZE_LOCAL_KV_TYPE desconocido; se ignora (f16)");
            None
        })
    });
    // `gpu_layers` y `placement` vienen resueltos de la carga (auto-fit o env
    // explícito), NO del entorno: el contexto tiene que armarse contra el
    // mismo reparto de capas y la misma ubicación de KV que se midieron al
    // cargar el modelo.
    if placement == KvPlacement::Host {
        tracing::info!(
            gpu_layers,
            ubatch = ubatch_setting(),
            "local: KV en host + micro-batch chico para mantener la VRAM plana"
        );
    }
    if requested_kv.is_some() {
        tracing::info!("local: KV cache cuantizado solicitado");
    }

    let ladder = context_ladder(placement, requested_kv);

    let mut ctx = 'ctx: {
        let mut last_err = String::from("sin intentos");
        for (i, (p, kv)) in ladder.iter().enumerate() {
            match model.new_context(backend, build_ctx_params(n_ctx, *p, *kv)) {
                Ok(c) => {
                    if i > 0 {
                        tracing::warn!(
                            placement = ?p,
                            kv_quantized = kv.is_some(),
                            "local: contexto creado tras degradar (el escalón previo no entró)"
                        );
                    }
                    break 'ctx c;
                }
                Err(e) => {
                    last_err = e.to_string();
                    tracing::debug!(placement = ?p, kv_quantized = kv.is_some(), error = %last_err,
                        "local: escalón de contexto descartado");
                }
            }
        }
        bail!("local: no se pudo crear el contexto en ningún escalón: {last_err}")
    };
    let n_ctx = std::num::NonZeroU32::new(n_ctx.max(256));

    // Harmony no lleva BOS: la conversación arranca directo en
    // `<|start|>system` (los GGUF de gpt-oss no definen add_bos).
    let add_bos = if harmony.is_some() {
        AddBos::Never
    } else {
        AddBos::Always
    };
    let tokens = match model.str_to_token(prompt, add_bos) {
        Ok(t) => t,
        Err(e) => bail!("local: tokenización falló: {e}"),
    };
    let input_tokens = tokens.len() as u32;

    // Guard explícito: un prompt que no cabe en el contexto debe ser un
    // error legible del backend, no un assert C++ que mata el proceso.
    let ctx_limit = n_ctx.map_or(256, std::num::NonZeroU32::get) as usize;
    if tokens.len() >= ctx_limit {
        bail!(
            "local: el prompt ({} tokens) no cabe en n_ctx ({ctx_limit}) — \
             la compactación del engine debería haber actuado antes",
            tokens.len()
        );
    }

    // El KV cache guarda prompt Y generación en el mismo `n_ctx`, así que el
    // presupuesto de tokens nuevos no puede ser el `max_tokens` pedido a
    // secas: hay que recortarlo a lo que sobra. Sin esto, un prompt de
    // `ctx_limit - 1` dejaba lugar para UN token y la generación moría de
    // `NoKvCacheSlot` (visto en vivo con el refactor de roam, 2026-07-26).
    // La guarda de arriba solo verificaba que el prompt entrara.
    let room = ctx_limit.saturating_sub(tokens.len());
    let budget = u32::try_from(room)
        .unwrap_or(u32::MAX)
        .min(max_tokens)
        .max(1);
    if budget < max_tokens {
        tracing::warn!(
            prompt_tokens = tokens.len(),
            ctx_limit,
            max_tokens,
            budget,
            "local: presupuesto de generación recortado por el contexto disponible"
        );
    }

    // El prompt se decodifica en chunks de n_batch: llama.cpp aborta el
    // proceso entero (GGML_ASSERT n_tokens_all <= n_batch) si un decode
    // excede el batch. Latente desde Fase 1 — los smokes usan prompts
    // cortos; lo expuso una tarea multi-ronda del sweep A/B del stencil
    // cuyo prompt de ronda superó los 2048 tokens (2026-07-21).
    const N_BATCH: usize = 2048; // default de llama_context_default_params
    let mut batch = LlamaBatch::new(N_BATCH, 1);
    let total = tokens.len();
    let mut fed = 0usize;
    while fed < total {
        // Misma cancelación por consumidor caído que el loop de
        // generación, y acá importa MÁS: el prefill de un prompt largo en
        // CPU puede tardar decenas de segundos por sí solo — verificado
        // en vivo con el deadline por ronda del engine (2026-08-09): el
        // corte llegó a los 2 s y el prefill siguió quemando CPU 44 s
        // más antes de que el loop de generación notara el canal cerrado.
        if tx.is_closed() {
            tracing::debug!(
                fed,
                total,
                "local: el consumidor abandonó — prefill cortado"
            );
            return;
        }
        batch.clear();
        let end = (fed + N_BATCH).min(total);
        for (i, tok) in tokens[fed..end].iter().enumerate() {
            let pos = fed + i;
            // Solo el último token del prompt pide logits.
            if let Err(e) = batch.add(*tok, pos as i32, &[0], pos + 1 == total) {
                bail!("local: batch.add falló: {e}");
            }
        }
        if let Err(e) = ctx.decode(&mut batch) {
            bail!("local: decode del prompt falló: {e}");
        }
        fed = end;
    }

    let mut sampler = free_sampler(model, &sampling);
    // Tokens ya generados, solo para re-sembrar DRY cuando el stencil
    // reconstruye el sampler (ver `rebuild_free_sampler`). Con greedy queda
    // vacío: no vale la pena acumular lo que nadie va a leer.
    let mut generated: Vec<LlamaToken> = Vec::new();
    // Tokens EOG prohibidos en la posición 0 (ver la guarda de turno vacío
    // en el loop). Se acumulan porque un vocabulario puede tener varios
    // (`<eos>`, `<end_of_turn>`…) y banear uno puede destapar el siguiente.
    let mut eog_bans: Vec<LlamaLogitBias> = Vec::new();
    const MAX_EOG_BANS: usize = 4;
    let track_generated = sampling.dry_enabled();
    // Posición del próximo token en el KV cache: el total del prompt
    // (no `batch.n_tokens()`, que tras el decode en chunks es solo el
    // tamaño del último chunk).
    let mut n_cur = total as i32;
    let mut output_tokens = 0u32;
    // `budget` ya se calculó arriba, recortado al contexto disponible.
    // Decoder UTF-8 persistente: un carácter multi-byte puede repartirse
    // entre tokens, y un decoder fresco por token lo rompería.
    let mut decoder = encoding_rs::UTF_8.new_decoder();

    let mut parser = HarmonyParser::new();
    // Presupuesto agotado sin cierre = "length" (mismo diagnóstico que
    // los wires: una tool call cortada por max_tokens no debe parecer un
    // stop limpio).
    let mut stop_reason = "length";

    // Stencil (Fase 3): constrained decoding GBNF con laziness manual —
    // el sampler se swapea a gramática+greedy exactamente cuando empieza
    // una tool call y vuelve a libre cuando el envelope se completa.
    // Kill-switch `BRAZE_LOCAL_GRAMMAR=off` (el brazo de ablación del
    // A/B; misma convención que BRAZE_CIRCUIT_BREAKER).
    let grammar_enabled = !matches!(
        std::env::var("BRAZE_LOCAL_GRAMMAR").as_deref(),
        Ok("off") | Ok("0")
    );
    // Precomputada por turno: el envelope qwen depende del inventario de
    // tools. Caveat compartido con la escalera de rescate: un
    // `<tool_call>` literal citado en texto libre (p.ej. dentro de un
    // fence) también gatilla — mismo trade-off, y el kill-switch cubre.
    let qwen_grammar = if harmony.is_none() && grammar_enabled {
        qwen_call_grammar(tools)
    } else {
        None
    };
    let mut constrained = false;
    let mut args_cursor = JsonCursor::new();
    let mut tail = String::new();

    // `n_cur` es la posición en el KV-cache, no un mero contador de
    // iteraciones (arranca en `batch.n_tokens()` y sólo avanza en tokens
    // que continúan la generación) — de ahí el allow.
    #[allow(clippy::explicit_counter_loop)]
    for _ in 0..budget {
        // Cancelación por consumidor caído, chequeada POR TOKEN. Los
        // `blocking_send(...).is_err()` de abajo no bastan: hay tramos
        // largos que no intentan ningún send — el canal analysis de
        // Harmony suprimido puede tragarse miles de tokens de
        // razonamiento sin emitir evento, y los fragmentos UTF-8
        // parciales tampoco emiten. Sin este chequeo, un consumidor que
        // dropea el stream (p.ej. el deadline por ronda del engine,
        // `Engine::with_max_round_wall_clock`) dejaría la generación
        // quemando CPU/GPU en background hasta agotar `budget` —
        // contaminando la celda siguiente de un sweep con un decode
        // concurrente fantasma.
        if tx.is_closed() {
            tracing::debug!(
                output_tokens,
                "local: el consumidor abandonó — generación cortada"
            );
            return;
        }
        // OJO: `sample()` ya hace el accept internamente
        // (`llama_sampler_sample` → `llama_sampler_accept`). Un accept
        // explícito acá sería double-accept: inofensivo con greedy
        // (stateless), fatal con gramática (avanza el stack GBNF dos
        // veces → GGML_ASSERT(!stacks.empty()) — depurado en vivo,
        // 2026-07-21).
        let token = sampler.sample(&ctx, batch.n_tokens() - 1);
        if track_generated {
            generated.push(token);
        }
        let banned_eog = |t: LlamaToken| eog_bans.iter().any(|b| b.token() == t);
        let marker = harmony.and_then(|ids| ids.marker_of(token));
        if let Some(m @ (HarmonyMarker::Call | HarmonyMarker::Return)) = marker {
            // Cierre del turno harmony (ambos son además EOG en el GGUF
            // de gpt-oss; el match por id decide el stop_reason honesto).
            output_tokens += 1;
            stop_reason = "stop";
            if let Some(event) = parser.feed_marker(m) {
                let is_call = matches!(event, HarmonyEvent::ToolCall { .. });
                if !emit_harmony_event(event, tx) {
                    return; // el consumidor abandonó (cancelación)
                }
                if is_call {
                    stop_reason = "tool_use";
                }
            }
            break;
        }
        // GUARDA DE TURNO VACÍO. Un EOG como PRIMER token deja la ronda en 0
        // tokens y el engine la ve como "el modelo no dijo nada"
        // (`ModelBackendError`) — no como un fin de turno legítimo. Medido en
        // gemma4:e4b el 2026-07-26: pasa en ~9% de las rondas y **no es
        // determinista**, porque `<eos>` empata con el token real dentro de
        // 0.05 de logit (`"<eos>"=23.225 "<"=23.175 "The"=23.109`) y el
        // no-determinismo de punto flotante en GPU decide el desempate. Con
        // temperatura empeora (21%): aplana una distribución ya plana.
        //
        // Un empate así no es el modelo decidiendo terminar; es el modelo
        // indeciso. Prohibir el EOG y re-muestrear devuelve el mejor token
        // real, que es justo lo que el turno necesitaba. Solo aplica en la
        // posición 0: a partir del primer token, un EOG es un fin de turno
        // legítimo y se respeta.
        if marker.is_none() && output_tokens == 0 && model.is_eog_token(token) && !banned_eog(token)
        {
            eog_bans.push(LlamaLogitBias::new(token, f32::NEG_INFINITY));
            if eog_bans.len() <= MAX_EOG_BANS {
                tracing::warn!(
                    eog_token = token.0,
                    intento = eog_bans.len(),
                    "local: EOG como primer token de la ronda — prohibido y re-muestreando"
                );
                sampler = LlamaSampler::chain_simple([
                    LlamaSampler::logit_bias(model.n_vocab(), &eog_bans),
                    free_sampler(model, &sampling),
                ]);
                continue;
            }
            tracing::warn!(
                "local: la ronda sigue eligiendo EOG tras {MAX_EOG_BANS} intentos — se cierra vacía"
            );
        }
        if marker.is_none() && model.is_eog_token(token) {
            // Diagnóstico del turno vacío: si el PRIMER token muestreado ya
            // es EOG, la ronda entera se va con 0 tokens y el engine la ve
            // como "el modelo no dijo nada" (ModelBackendError). Pasa ~9% de
            // las veces con gemma4:e4b y no es determinista, lo que apunta a
            // un empate casi exacto entre EOG y el token real: el
            // no-determinismo de punto flotante en GPU decide. Loguear los
            // candidatos de arriba es lo único que distingue "la plantilla
            // deja EOG arriba" de "el modelo realmente no tiene nada que
            // decir".
            if output_tokens == 0 {
                let mut top: Vec<_> = ctx.candidates_ith(batch.n_tokens() - 1).collect();
                top.sort_by(|a, b| b.logit().total_cmp(&a.logit()));
                let top: Vec<String> = top
                    .iter()
                    .take(5)
                    .map(|c| {
                        // `special = true`: acá SÍ queremos ver los
                        // marcadores de plantilla — son los sospechosos.
                        let mut dec = encoding_rs::UTF_8.new_decoder();
                        let piece = model
                            .token_to_piece(c.id(), &mut dec, true, None)
                            .unwrap_or_else(|_| format!("<id {}>", c.id().0));
                        format!("{piece:?}={:.3}", c.logit())
                    })
                    .collect();
                tracing::warn!(
                    eog_token = token.0,
                    candidatos = %top.join(" "),
                    "local: la ronda terminó con 0 tokens — EOG salió como PRIMER token"
                );
            }
            stop_reason = "stop";
            break;
        }
        output_tokens += 1;
        if let Some(m) = marker {
            // Marcador estructural intra-turno (<|channel|>, <|message|>,
            // <|end|>…): nunca se renderiza; puede cerrar una tool call
            // off-spec (lenidad de <|end|>) y la generación continúa —
            // eso habilita turnos multi-call.
            if let Some(event) = parser.feed_marker(m) {
                if matches!(event, HarmonyEvent::ToolCall { .. }) {
                    stop_reason = "tool_use";
                }
                if !emit_harmony_event(event, tx) {
                    return;
                }
            }
            if grammar_enabled {
                match m {
                    // El header fijó destinatario: lo que viene son los
                    // args — estencilarlos con la gramática derivada del
                    // schema de ESA tool (fallback: objeto JSON genérico).
                    HarmonyMarker::Message if parser.tool_call_in_progress() && !constrained => {
                        let tool = parser.pending_tool_name().unwrap_or_default();
                        let grammar = harmony_args_grammar(tool, tools);
                        if let Some(s) = constrained_sampler(model, &grammar, &sampling) {
                            sampler = s;
                            constrained = true;
                            args_cursor = JsonCursor::new();
                            tracing::info!(tool, "stencil: constraint de args harmony activado");
                        }
                    }
                    // Cierre de mensaje con el constraint aún puesto
                    // (cierre off-spec): liberar antes de seguir.
                    HarmonyMarker::End | HarmonyMarker::Start if constrained => {
                        sampler = rebuild_free_sampler(model, &sampling, &generated);
                        constrained = false;
                    }
                    _ => {}
                }
            }
        } else {
            // `special = false`: no renderizar tokens especiales de
            // plantilla (p.ej. `<|im_end|>`) como texto — no deben
            // filtrarse a la salida.
            match model.token_to_piece(token, &mut decoder, false, None) {
                Ok(piece) => {
                    if piece.is_empty() {
                        // token especial no-EOG o fragmento UTF-8
                        // pendiente: sigue generando, no emite nada.
                    } else if harmony.is_some() {
                        if let Some(event) = parser.feed_text(&piece)
                            && !emit_harmony_event(event, tx)
                        {
                            return;
                        }
                        // Los args estencilados avanzan el cursor; al
                        // cerrar el objeto raíz se libera el sampler y
                        // el modelo emite su <|call|> libremente.
                        if constrained {
                            args_cursor.feed(&piece);
                            if args_cursor.complete() {
                                sampler = rebuild_free_sampler(model, &sampling, &generated);
                                constrained = false;
                                tracing::info!(
                                    "stencil: args JSON completos — constraint liberado"
                                );
                            }
                        }
                    } else {
                        if tx
                            .blocking_send(Ok(CompletionEvent::TextDelta(piece.clone())))
                            .is_err()
                        {
                            return; // el consumidor abandonó (cancelación)
                        }
                        if qwen_grammar.is_some() {
                            tail.push_str(&piece);
                            let excess = tail.len().saturating_sub(64);
                            if excess > 0 {
                                let cut = (excess..tail.len())
                                    .find(|i| tail.is_char_boundary(*i))
                                    .unwrap_or(0);
                                tail.drain(..cut);
                            }
                            if !constrained && tail.ends_with("<tool_call>") {
                                if let Some(s) = constrained_sampler(
                                    model,
                                    qwen_grammar.as_deref().unwrap_or_default(),
                                    &sampling,
                                ) {
                                    sampler = s;
                                    constrained = true;
                                    tracing::info!("stencil: envelope qwen activado");
                                }
                            } else if constrained && tail.ends_with("</tool_call>") {
                                sampler = rebuild_free_sampler(model, &sampling, &generated);
                                constrained = false;
                                tracing::info!("stencil: envelope cerrado — constraint liberado");
                            }
                        }
                    }
                }
                Err(e) => {
                    // Un token de control no renderizable (p.ej. un
                    // `<|im_start|>` espurio: el modelo intentando abrir
                    // otro turno) terminaba el stream con error duro — 3
                    // fallos del brazo OFF del sweep A/B del stencil
                    // (2026-07-21). Es fin-de-turno de facto, no un error
                    // del backend: los stacks de chat suelen listar
                    // `<|im_start|>` como stop string. Cerrar limpio.
                    tracing::debug!(error = %e, "local: token no renderizable — fin de turno");
                    stop_reason = "stop";
                    break;
                }
            }
        }
        batch.clear();
        if let Err(e) = batch.add(token, n_cur, &[0], true) {
            bail!("local: batch.add (gen) falló: {e}");
        }
        n_cur += 1;
        if let Err(e) = ctx.decode(&mut batch) {
            // Quedarse sin KV cache no es un fallo del backend: es el
            // contexto lleno. Cerrar la ronda como `length` deja que el
            // engine vea un turno truncado y compacte, que es su trabajo.
            // Antes esto hacía `bail!` y mataba el turno entero — encontrado
            // corriendo el refactor de `Trajectory` sobre roam (2026-07-26),
            // donde el prompt real ronda el `n_ctx` y `default.toml` nunca
            // llega a acercarse.
            if matches!(e, llama_cpp_2::DecodeError::NoKvCacheSlot) {
                tracing::warn!(
                    n_cur,
                    output_tokens,
                    "local: KV cache lleno a mitad de generación — ronda cerrada como `length`"
                );
                stop_reason = "length";
                break;
            }
            bail!("local: decode (gen) falló: {e}");
        }
    }

    if stop_reason == "length" && parser.tool_call_in_progress() {
        tracing::warn!(
            "harmony: presupuesto de tokens agotado a mitad de una tool call — \
             la call se descarta (subir max_tokens)"
        );
    }

    let _ = tx.blocking_send(Ok(CompletionEvent::Usage {
        input_tokens,
        output_tokens,
        stop_reason: Some(stop_reason.to_string()),
        cache_read_tokens: None,
        cache_write_tokens: None,
        escalation_trigger: None,
    }));
    let _ = tx.blocking_send(Ok(CompletionEvent::Done));
}
