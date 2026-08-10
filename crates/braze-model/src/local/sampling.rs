//! Sampling del LocalBackend: `LocalSampling` (default greedy,
//! deliberado — todo lo medido salió con greedy) y la construcción de
//! samplers libres/constrained (stencil GBNF). L-4: extraído VERBATIM
//! de `local.rs`.

use super::*;

/// Cómo samplea el `LocalBackend`.
///
/// **El default es greedy**, que es lo único que este backend hizo desde su
/// Fase 1: `CompletionRequest` no lleva temperatura y `local.rs` nunca la
/// consultó. Mantenerlo así es deliberado — todo lo medido del LocalBackend
/// (paridad, stencil, pass^k, el 57/57 de gpt-oss) salió con greedy, y
/// cambiar el default de entrada volvería incomparables esos números. DRY y
/// min-p entran como **palanca opt-in que se gana su default por bench**,
/// misma doctrina que KV-quant y el stencil.
///
/// Hueco conocido que esto NO tapa: `braze-bench --temperature` sigue sin
/// llegar al LocalBackend (`build_local` ni recibe el `sampling`), así que la
/// garantía N-34 de "un régimen de sampling por sweep" no se cumple para los
/// brazos locales. Documentado como abierto, decisión del autor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LocalSampling {
    /// `0.0` = greedy (default). Cualquier valor > 0 activa muestreo.
    pub temperature: f32,
    /// min-p: descarta lo que esté por debajo de esta fracción del token más
    /// probable. `0.0` = apagado. Ataca la degeneración de modelos chicos
    /// mejor que top-p porque el umbral se adapta a lo confiado que esté el
    /// modelo en cada paso.
    pub min_p: f32,
    /// top-k, `0` = apagado.
    pub top_k: i32,
    /// top-p (nucleus), `0.0` = apagado.
    pub top_p: f32,
    /// Penalización de repetición, `1.0` = apagada.
    pub repeat_penalty: f32,
    /// Cuántos tokens atrás mira la penalización. `-1` = todo el contexto.
    pub repeat_last_n: i32,
    /// DRY (anti-repetición por n-gramas). `0.0` = apagado.
    pub dry_multiplier: f32,
    pub dry_base: f32,
    pub dry_allowed_length: i32,
    pub dry_penalty_last_n: i32,
    /// Semilla del muestreo. El default es `LLAMA_DEFAULT_SEED`
    /// (`0xFFFFFFFF`), que en llama.cpp significa **semilla aleatoria por
    /// generación** — no un seed fijo. Importa: con un seed fijo, las
    /// repeticiones de una misma tarea en un sweep producirían salidas
    /// idénticas y `--repetitions` no mediría varianza ninguna. Fijarlo solo
    /// para reproducir una corrida puntual. Irrelevante con greedy.
    pub seed: u32,
}

/// `LLAMA_DEFAULT_SEED` de llama.cpp: "usá una semilla aleatoria".
pub(super) const RANDOM_SEED: u32 = 0xFFFF_FFFF;

impl Default for LocalSampling {
    fn default() -> Self {
        Self {
            temperature: 0.0,
            min_p: 0.0,
            top_k: 0,
            top_p: 0.0,
            repeat_penalty: 1.0,
            repeat_last_n: 64,
            dry_multiplier: 0.0,
            // Defaults de llama.cpp para cuando se activa DRY.
            dry_base: 1.75,
            dry_allowed_length: 2,
            dry_penalty_last_n: -1,
            seed: RANDOM_SEED,
        }
    }
}

impl LocalSampling {
    /// Lee las palancas del entorno. Todas opt-in: sin ninguna, greedy.
    #[must_use]
    pub fn from_env() -> Self {
        fn f32_var(k: &str, default: f32) -> f32 {
            std::env::var(k)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(default)
        }
        fn i32_var(k: &str, default: i32) -> i32 {
            std::env::var(k)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(default)
        }
        let d = Self::default();
        Self {
            temperature: f32_var("BRAZE_LOCAL_TEMP", d.temperature),
            min_p: f32_var("BRAZE_LOCAL_MIN_P", d.min_p),
            top_k: i32_var("BRAZE_LOCAL_TOP_K", d.top_k),
            top_p: f32_var("BRAZE_LOCAL_TOP_P", d.top_p),
            repeat_penalty: f32_var("BRAZE_LOCAL_REPEAT_PENALTY", d.repeat_penalty),
            repeat_last_n: i32_var("BRAZE_LOCAL_REPEAT_LAST_N", d.repeat_last_n),
            dry_multiplier: f32_var("BRAZE_LOCAL_DRY", d.dry_multiplier),
            dry_base: f32_var("BRAZE_LOCAL_DRY_BASE", d.dry_base),
            dry_allowed_length: i32_var("BRAZE_LOCAL_DRY_ALLOWED", d.dry_allowed_length),
            dry_penalty_last_n: i32_var("BRAZE_LOCAL_DRY_LAST_N", d.dry_penalty_last_n),
            // Sin la variable, semilla aleatoria por generación.
            seed: std::env::var("BRAZE_LOCAL_SEED")
                .ok()
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(RANDOM_SEED),
        }
    }

    /// Aplica el régimen de sampling que fija un sweep **encima** de la
    /// base del entorno.
    ///
    /// La fusión (en vez de reemplazo) es deliberada: el bench controla
    /// temperatura/seed/top-p/top-k/repeat-penalty, pero no conoce min-p ni
    /// DRY. Si sobrescribiera todo, esas dos dejarían de ser ablacionables
    /// dentro de un sweep — que es justo como se corrió su primer A/B. Así,
    /// el sweep manda en lo suyo y el entorno sigue gobernando el resto.
    ///
    /// Cierra el hueco de **N-34** para el LocalBackend: hasta el
    /// 2026-07-26 `braze-bench --temperature` no llegaba acá y todo brazo
    /// local corría greedy, así que la garantía de "un solo régimen de
    /// sampling por sweep" no se cumplía.
    #[must_use]
    pub fn with_sweep(
        mut self,
        temperature: f32,
        seed: Option<u64>,
        top_p: Option<f32>,
        top_k: Option<u32>,
        repeat_penalty: Option<f32>,
    ) -> Self {
        self.temperature = temperature;
        if let Some(seed) = seed {
            self.seed = u32::try_from(seed & u64::from(u32::MAX)).unwrap_or(RANDOM_SEED);
        }
        if let Some(p) = top_p {
            self.top_p = p;
        }
        if let Some(k) = top_k {
            self.top_k = i32::try_from(k).unwrap_or(i32::MAX);
        }
        if let Some(r) = repeat_penalty {
            self.repeat_penalty = r;
        }
        self
    }

    /// ¿Es el camino histórico (greedy puro, sin filtros)?
    #[must_use]
    pub fn is_greedy(&self) -> bool {
        self.temperature <= 0.0
            && self.min_p <= 0.0
            && self.top_k <= 0
            && self.top_p <= 0.0
            && (self.repeat_penalty - 1.0).abs() < f32::EPSILON
            && !self.dry_enabled()
    }

    /// DRY lleva **estado** (la historia de n-gramas). Importa porque el
    /// stencil reconstruye el sampler cada vez que suelta el constraint: con
    /// greedy da igual (sin estado), con DRY habría que re-sembrarlo o
    /// perdería su historia en cada tool call.
    #[must_use]
    pub fn dry_enabled(&self) -> bool {
        self.dry_multiplier > 0.0
    }
}

/// Arma la cadena de sampling libre (sin gramática) según la configuración.
///
/// Orden de la cadena, el canónico de llama.cpp: penalizaciones primero
/// (DRY), después los filtros de candidatos (top-k, min-p), después la
/// temperatura, y al final la extracción del token. Invertirlo cambiaría
/// qué distribución ve cada etapa.
pub(super) fn free_sampler(model: &LlamaModel, s: &LocalSampling) -> LlamaSampler {
    if s.is_greedy() {
        return LlamaSampler::greedy();
    }
    let mut chain = Vec::new();
    if s.dry_enabled() {
        // seq_breakers default de llama.cpp: cortan el n-grama en límites
        // naturales para no penalizar estructura legítima (JSON, listas).
        chain.push(LlamaSampler::dry(
            model,
            s.dry_multiplier,
            s.dry_base,
            s.dry_allowed_length,
            s.dry_penalty_last_n,
            ["\n", ":", "\"", "*"],
        ));
    }
    if (s.repeat_penalty - 1.0).abs() >= f32::EPSILON {
        chain.push(LlamaSampler::penalties(
            s.repeat_last_n,
            s.repeat_penalty,
            0.0,
            0.0,
        ));
    }
    if s.top_k > 0 {
        chain.push(LlamaSampler::top_k(s.top_k));
    }
    if s.top_p > 0.0 {
        chain.push(LlamaSampler::top_p(s.top_p, 1));
    }
    if s.min_p > 0.0 {
        chain.push(LlamaSampler::min_p(s.min_p, 1));
    }
    if s.temperature > 0.0 {
        chain.push(LlamaSampler::temp(s.temperature));
        chain.push(LlamaSampler::dist(s.seed));
    } else {
        // Filtros sin temperatura: los filtros acotan el conjunto y greedy
        // elige el más probable de lo que quedó.
        chain.push(LlamaSampler::greedy());
    }
    LlamaSampler::chain_simple(chain)
}

/// Reconstruye la cadena libre **conservando el estado** que el stencil
/// destruiría.
///
/// El stencil swapea el sampler cada vez que abre y cierra una tool call.
/// Con greedy eso es inocuo (no tiene estado), pero DRY lleva la historia de
/// n-gramas generados: un sampler nuevo la perdería en cada tool call y DRY
/// quedaría medio apagado justo en las generaciones largas, que son las que
/// degeneran. Re-alimentar los tokens ya emitidos lo deja donde estaba.
///
/// Sin DRY se salta el trabajo: `accept_many` sobre cientos de tokens no es
/// gratis y no compra nada para samplers sin estado.
pub(super) fn rebuild_free_sampler(
    model: &LlamaModel,
    s: &LocalSampling,
    generated: &[LlamaToken],
) -> LlamaSampler {
    let mut sampler = free_sampler(model, s);
    if s.dry_enabled() {
        sampler.accept_many(generated);
    }
    sampler
}

/// Construye el sampler estencilado: gramática GBNF + la cadena libre
/// encadenadas (la gramática enmascara logits; la cadena elige entre lo
/// permitido). Una gramática inválida es bug nuestro, no del modelo — se
/// loguea y se sigue sin constraint antes que brickear la generación.
pub(super) fn constrained_sampler(
    model: &LlamaModel,
    grammar: &str,
    s: &LocalSampling,
) -> Option<LlamaSampler> {
    match LlamaSampler::grammar(model, grammar, "root") {
        Ok(g) => Some(LlamaSampler::chain_simple([g, free_sampler(model, s)])),
        Err(e) => {
            tracing::warn!(error = %e, "stencil: gramática inválida — generación sin constraint");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn el_default_de_sampling_es_greedy() {
        // Lo que protege este test: TODO lo medido del LocalBackend
        // (paridad, stencil, pass^k, el 57/57 de gpt-oss) salió con greedy.
        // Si alguien cambia el default, esos números dejan de significar lo
        // que dicen los docs — que se rompa el test antes que la
        // comparabilidad.
        let d = LocalSampling::default();
        assert!(d.is_greedy());
        assert!(!d.dry_enabled());
        assert_eq!(d.temperature, 0.0);
        assert_eq!(d.min_p, 0.0);
        assert_eq!(d.top_k, 0);
        // Semilla aleatoria por generación: con un seed fijo las
        // repeticiones de un sweep saldrían calcadas y `--repetitions` no
        // mediría varianza. Irrelevante mientras el default sea greedy,
        // pero es lo que hace utilizable el brazo estocástico de un A/B.
        assert_eq!(d.seed, RANDOM_SEED);
    }

    #[test]
    fn cualquier_palanca_de_sampling_saca_del_camino_greedy() {
        // `is_greedy` decide qué cadena se arma; si una palanca no lo
        // sacara del camino greedy, quedaría configurada pero inerte.
        let base = LocalSampling::default();
        for tweak in [
            LocalSampling {
                temperature: 0.7,
                ..base
            },
            LocalSampling {
                min_p: 0.05,
                ..base
            },
            LocalSampling { top_k: 40, ..base },
            LocalSampling {
                dry_multiplier: 0.8,
                ..base
            },
        ] {
            assert!(!tweak.is_greedy(), "{tweak:?} debería salir de greedy");
        }
    }

    #[test]
    fn el_sweep_manda_en_lo_suyo_y_el_entorno_gobierna_el_resto() {
        // N-34 para el LocalBackend: el sweep fija temperatura/seed/top-p/
        // top-k/repeat-penalty. Pero NO conoce min-p ni DRY, así que si
        // sobrescribiera todo, esas dos dejarían de ser ablacionables
        // dentro de un sweep — que es exactamente como se corrió su primer
        // A/B. Por eso fusiona en vez de reemplazar.
        let base = LocalSampling {
            min_p: 0.05,
            dry_multiplier: 0.8,
            ..LocalSampling::default()
        };
        let merged = base.with_sweep(0.7, Some(42), Some(0.9), Some(40), Some(1.1));

        // Lo que el sweep fija, manda.
        assert_eq!(merged.temperature, 0.7);
        assert_eq!(merged.seed, 42);
        assert_eq!(merged.top_p, 0.9);
        assert_eq!(merged.top_k, 40);
        assert_eq!(merged.repeat_penalty, 1.1);
        // Lo que el sweep no conoce, sobrevive.
        assert_eq!(merged.min_p, 0.05, "min-p no debe perderse");
        assert!(merged.dry_enabled(), "DRY no debe perderse");
    }

    #[test]
    fn un_knob_ausente_en_el_sweep_no_pisa_el_del_entorno() {
        // `None` significa "el sweep no lo fijó", no "apagalo".
        let base = LocalSampling {
            top_k: 20,
            top_p: 0.8,
            repeat_penalty: 1.05,
            ..LocalSampling::default()
        };
        let merged = base.with_sweep(0.2, None, None, None, None);
        assert_eq!(merged.temperature, 0.2, "la temperatura siempre se aplica");
        assert_eq!(merged.top_k, 20);
        assert_eq!(merged.top_p, 0.8);
        assert_eq!(merged.repeat_penalty, 1.05);
        assert_eq!(
            merged.seed, RANDOM_SEED,
            "sin seed del sweep, sigue siendo aleatoria por generación"
        );
    }

    #[test]
    fn solo_dry_marca_el_sampling_como_con_estado() {
        // `dry_enabled` gobierna dos cosas caras: acumular los tokens
        // generados y re-alimentarlos al reconstruir el sampler. Que se
        // active de más cuesta CPU en cada tool call; de menos, DRY pierde
        // su historia y queda medio apagado.
        let base = LocalSampling::default();
        assert!(
            !LocalSampling {
                temperature: 0.7,
                min_p: 0.05,
                top_k: 40,
                ..base
            }
            .dry_enabled()
        );
        assert!(
            LocalSampling {
                dry_multiplier: 0.8,
                ..base
            }
            .dry_enabled()
        );
    }
}
