//! Corte secuencial anytime-valid para sweeps pareados (`--sequential-stop`).
//!
//! El problema: un A/B corre `tareas × repeticiones × brazos` completo y se
//! analiza al final, porque el McNemar clásico no admite mirar antes sin
//! inflar α. El A/B de `enable_project_memory` (2026-08-04/06) pagó 27 horas
//! para que su gate de plomería dijera "detente"; la retrodicción sobre 300
//! contrastes históricos (`docs/retrodiccion-evalues-2026-08-06.md`) midió
//! un ahorro mediano del **62%** con el veredicto intacto en el 93% de los
//! casos.
//!
//! Dos monitores, porque responden preguntas distintas:
//!
//! - **E-process** (martingala de apuesta con mixture Beta(½,½) sobre los
//!   pares discordantes): p-value válido bajo monitoreo continuo, se para
//!   cuando `E ≥ 1/α` por la desigualdad de Ville. **Solo rechaza H0** —
//!   nunca la acepta. Es el monitor correcto para un *gate* (un gate solo
//!   necesita disparar).
//! - **SPRT** doble unilateral: además puede **aceptar H0** cuando el efecto
//!   es menor que el umbral declarado. Es el monitor correcto para un
//!   *criterio de adopción*.
//!
//! **Asimetría del ahorro, medida al implementar (2026-08-07)**: el corte
//! paga cuando HAY efecto y casi nunca cuando no lo hay. Con `p1` derivado
//! de un umbral de ±3 celdas sobre 102 (`p1≈0.56`), el SPRT necesita ~220
//! pares discordantes para aceptar H0, y un sweep de ese tamaño produce
//! ~25. O sea: un A/B con efecto corta temprano; uno nulo corre completo.
//! Es consistente con la retrodicción (228/300 decididos, ahorro
//! concentrado en los que tenían efecto) y es la conducta correcta — un
//! nulo necesita todo el n para ser creíble. No se "arregla" subiendo
//! `p1`: eso aceptaría H0 con evidencia que no alcanza.
//!
//! La corrección que la retrodicción obligó a hacer: `p1` **no** es una
//! constante. El SPRT con `p1` genérico acordó con el análisis n-fijo solo
//! en el 48% de sus "null" — porque su null significa *"efecto bajo el
//! umbral"*, no *"efecto cero"*. Como los criterios de este proyecto son de
//! umbral (±N tareas), la semántica calza, pero **el umbral tiene que venir
//! del pre-registro del experimento**, no de un default. Por eso
//! [`SequentialStop::for_threshold`] lo toma como argumento.

/// Cota de Ville: se rechaza H0 cuando el e-value supera 1/α.
const E_THRESHOLD: f64 = 20.0; // α = 0.05

/// Cotas de Wald para el SPRT (α = 0.05, β = 0.20).
const SPRT_ACCEPT_H1: f64 = 16.0; // (1-β)/α
const SPRT_ACCEPT_H0: f64 = 0.2105; // β/(1-α)

/// Qué concluyó el monitor secuencial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// El e-process cruzó la cota de Ville: hay efecto, a favor del brazo
    /// indicado (`true` = el brazo B, el segundo del par).
    Effect { favors_b: bool },
    /// El SPRT descartó ambos lados: el efecto es MENOR que el umbral
    /// pre-registrado. No es "efecto cero" — es "sub-umbral", y esa
    /// distinción va al reporte tal cual.
    BelowThreshold,
}

/// Monitor secuencial sobre pares discordantes de McNemar.
///
/// Se alimenta con un par `(pasó_en_A, pasó_en_B)` por celda; los pares
/// concordantes se ignoran (no aportan a McNemar) y los discordantes
/// actualizan ambos monitores.
#[derive(Debug, Clone)]
pub struct SequentialStop {
    /// Razón de discordantes bajo H1, derivada del umbral pre-registrado.
    p1: f64,
    n_discordant: usize,
    favor_b: usize,
    log_e: f64,
    log_lr_b: f64,
    log_lr_a: f64,
    verdict: Option<Verdict>,
}

impl SequentialStop {
    /// Construye el monitor desde el **umbral pre-registrado** del
    /// experimento, expresado como la diferencia mínima de celdas que el
    /// criterio considera señal.
    ///
    /// El mapeo: si el criterio pide una diferencia de `delta` celdas sobre
    /// `n_cells` totales, la razón de discordantes implicada bajo H1 es
    /// `0.5 + delta / (2 · esperados_discordantes)`, acotada a [0.55, 0.95]
    /// — bajo 0.55 el SPRT no termina nunca en muestras realistas, sobre
    /// 0.95 acepta H0 con demasiada facilidad. Se estima
    /// `esperados_discordantes ≈ n_cells / 4`, la tasa observada en los
    /// sweeps históricos del proyecto.
    pub fn for_threshold(delta_cells: usize, n_cells: usize) -> Self {
        let expected_discordant = (n_cells as f64 / 4.0).max(1.0);
        let p1 = (0.5 + delta_cells as f64 / (2.0 * expected_discordant)).clamp(0.55, 0.95);
        Self {
            p1,
            n_discordant: 0,
            favor_b: 0,
            log_e: 0.0,
            log_lr_b: 0.0,
            log_lr_a: 0.0,
            verdict: None,
        }
    }

    /// Alimenta una celda pareada. Devuelve el veredicto la primera vez que
    /// alguno de los monitores cruza su cota; `None` mientras no haya
    /// evidencia suficiente para parar.
    pub fn observe(&mut self, passed_a: bool, passed_b: bool) -> Option<Verdict> {
        if self.verdict.is_some() || passed_a == passed_b {
            return self.verdict;
        }
        self.n_discordant += 1;
        let favors_b = passed_b && !passed_a;
        if favors_b {
            self.favor_b += 1;
        }

        // E-value de mixture Beta(1/2,1/2): martingala exacta bajo H0
        // (cada discordante favorece a un brazo con p=1/2).
        self.log_e = (self.n_discordant as f64) * std::f64::consts::LN_2
            + ln_beta(
                0.5 + self.favor_b as f64,
                0.5 + (self.n_discordant - self.favor_b) as f64,
            )
            - ln_beta(0.5, 0.5);

        // SPRTs unilaterales: uno apuesta a que B gana, el otro a que A gana.
        let (win, lose) = (
            self.p1.ln() - 0.5f64.ln(),
            (1.0 - self.p1).ln() - 0.5f64.ln(),
        );
        if favors_b {
            self.log_lr_b += win;
            self.log_lr_a += lose;
        } else {
            self.log_lr_b += lose;
            self.log_lr_a += win;
        }

        if self.log_e >= E_THRESHOLD.ln() {
            self.verdict = Some(Verdict::Effect {
                favors_b: self.favor_b * 2 >= self.n_discordant,
            });
        } else if self.log_lr_b >= SPRT_ACCEPT_H1.ln() {
            self.verdict = Some(Verdict::Effect { favors_b: true });
        } else if self.log_lr_a >= SPRT_ACCEPT_H1.ln() {
            self.verdict = Some(Verdict::Effect { favors_b: false });
        } else if self.log_lr_b <= SPRT_ACCEPT_H0.ln() && self.log_lr_a <= SPRT_ACCEPT_H0.ln() {
            self.verdict = Some(Verdict::BelowThreshold);
        }
        self.verdict
    }

    /// Resumen para el reporte — nunca silencioso: un sweep que paró
    /// temprano debe decir por qué y con cuánta evidencia.
    pub fn summary(&self) -> String {
        let e = self.log_e.exp();
        match self.verdict {
            Some(Verdict::Effect { favors_b }) => format!(
                "corte secuencial: EFECTO a favor del brazo {} tras {} pares discordantes \
                 ({} a favor de B) — e-value {:.1} ≥ {:.0} (Ville, α=0.05), p1={:.2} del umbral \
                 pre-registrado",
                if favors_b { "B" } else { "A" },
                self.n_discordant,
                self.favor_b,
                e,
                E_THRESHOLD,
                self.p1
            ),
            Some(Verdict::BelowThreshold) => format!(
                "corte secuencial: efecto SUB-UMBRAL tras {} pares discordantes ({} a favor de B) \
                 — ambos SPRT unilaterales bajo su cota con p1={:.2}. Esto NO es 'efecto cero': \
                 es 'menor que el umbral pre-registrado'.",
                self.n_discordant, self.favor_b, self.p1
            ),
            None => format!(
                "sin corte secuencial: {} pares discordantes ({} a favor de B), e-value {:.2} \
                 (cota {:.0})",
                self.n_discordant, self.favor_b, e, E_THRESHOLD
            ),
        }
    }

    /// Usado por los tests y por cualquier consumidor que quiera el
    /// veredicto sin re-observar. `#[cfg(test)]` no alcanza: es parte de
    /// la superficie pública del monitor.
    #[allow(dead_code)]
    pub fn verdict(&self) -> Option<Verdict> {
        self.verdict
    }
}

fn ln_beta(a: f64, b: f64) -> f64 {
    ln_gamma(a) + ln_gamma(b) - ln_gamma(a + b)
}

/// Lanczos — evita traer una dependencia por una sola función.
fn ln_gamma(x: f64) -> f64 {
    const G: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_1,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    if x < 0.5 {
        // Reflexión: Γ(x)Γ(1-x) = π / sin(πx)
        (std::f64::consts::PI / (std::f64::consts::PI * x).sin()).ln() - ln_gamma(1.0 - x)
    } else {
        let x = x - 1.0;
        let mut a = G[0];
        let t = x + 7.5;
        for (i, g) in G.iter().enumerate().skip(1) {
            a += g / (x + i as f64);
        }
        0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + a.ln()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Un efecto grande y consistente debe cortar temprano — es el caso
    /// que justifica la palanca.
    #[test]
    fn a_consistent_effect_stops_early() {
        let mut s = SequentialStop::for_threshold(3, 102);
        let mut stopped_at = None;
        for i in 0..102 {
            // B gana en todos los discordantes.
            if s.observe(false, true).is_some() {
                stopped_at = Some(i + 1);
                break;
            }
        }
        let at = stopped_at.expect("un efecto unánime debe cortar");
        assert!(at < 20, "cortó en {at}, esperaba mucho antes del final");
        assert!(matches!(
            s.verdict(),
            Some(Verdict::Effect { favors_b: true })
        ));
        assert!(
            s.summary().contains("EFECTO a favor del brazo B"),
            "{}",
            s.summary()
        );
    }

    /// El caso que este proyecto necesita distinguir: efecto real pero
    /// SUB-UMBRAL. El resumen debe decirlo con esas palabras — confundirlo
    /// con "efecto cero" es exactamente el error que la retrodicción
    /// encontró en el 52% de los null del SPRT genérico.
    ///
    /// Y de paso fija la ASIMETRÍA documentada arriba: con el tamaño de
    /// sweep real del proyecto (~25 discordantes) el patrón 50/50 NO corta
    /// — corre completo, que es lo correcto para un nulo. Solo con muchas
    /// más observaciones el SPRT llega a aceptar H0.
    #[test]
    fn a_coin_flip_pattern_is_reported_as_below_threshold_not_as_zero() {
        // 1) Tamaño de sweep real: no debe cortar.
        let mut real = SequentialStop::for_threshold(3, 102);
        for i in 0..25 {
            assert!(
                real.observe(i % 2 == 0, i % 2 == 1).is_none(),
                "con ~25 discordantes un nulo NO debe cortar: {}",
                real.summary()
            );
        }

        // 2) Con evidencia suficiente sí concluye, y lo nombra bien.
        let mut s = SequentialStop::for_threshold(3, 102);
        let mut verdict = None;
        for i in 0..400 {
            verdict = s.observe(i % 2 == 0, i % 2 == 1);
            if verdict.is_some() {
                break;
            }
        }
        assert_eq!(verdict, Some(Verdict::BelowThreshold));
        let msg = s.summary();
        assert!(msg.contains("SUB-UMBRAL"), "{msg}");
        assert!(msg.contains("NO es 'efecto cero'"), "{msg}");
    }

    /// Los pares concordantes no aportan a McNemar y no deben mover
    /// ningún monitor.
    #[test]
    fn concordant_pairs_are_ignored() {
        let mut s = SequentialStop::for_threshold(3, 102);
        for _ in 0..500 {
            assert!(s.observe(true, true).is_none());
            assert!(s.observe(false, false).is_none());
        }
        assert!(
            s.summary().contains("0 pares discordantes"),
            "{}",
            s.summary()
        );
    }

    /// El umbral pre-registrado manda: un criterio exigente (delta grande)
    /// produce un p1 mayor, o sea acepta H0 antes.
    #[test]
    fn the_preregistered_threshold_drives_p1() {
        let lax = SequentialStop::for_threshold(1, 102);
        let strict = SequentialStop::for_threshold(10, 102);
        assert!(strict.p1 > lax.p1, "lax={} strict={}", lax.p1, strict.p1);
        assert!((0.55..=0.95).contains(&lax.p1));
        assert!((0.55..=0.95).contains(&strict.p1));
    }

    /// `ln_gamma` contra valores conocidos — si esto se rompe, el e-value
    /// miente en silencio.
    #[test]
    fn ln_gamma_matches_known_values() {
        assert!((ln_gamma(1.0) - 0.0).abs() < 1e-9);
        assert!((ln_gamma(5.0) - 24.0f64.ln()).abs() < 1e-9); // Γ(5)=4!
        assert!((ln_gamma(0.5) - std::f64::consts::PI.sqrt().ln()).abs() < 1e-9);
    }
}

/// Chequeo de salud de banco por discriminación (técnica #2 del roadmap,
/// `docs/irt-suites-2026-08-07.md`).
///
/// Un ítem cuyo resultado es (casi) independiente de la habilidad del
/// modelo no está midiendo capacidad: está midiendo otra cosa. El caso que
/// lo motiva es `read_file_basic`, que en julio salió con discriminación
/// IRT de 0.44 (el resto: 2.1–6.0) y **anti-correlacionaba con el tamaño
/// del modelo** — resultó estar midiendo los errores de transporte de
/// Ollama 0.30.7, no capacidad. Nadie lo vio hasta agosto.
///
/// Este chequeo es la versión barata de ese diagnóstico: no ajusta IRT
/// (necesitaría decenas de respondentes que un solo sweep no tiene), sino
/// que usa la **correlación punto-biserial** entre acertar el ítem y el
/// puntaje total del respondente — el estimador clásico de discriminación
/// en teoría de tests, calculable con las celdas de un sweep.
///
/// Solo informa; nunca falla el sweep. La regla adoptada: un ítem con
/// correlación bajo [`LOW_DISCRIMINATION`] marca el sweep para revisión
/// ANTES de interpretarlo.
pub const LOW_DISCRIMINATION: f64 = 0.10;

/// `(item, r_pbis)` de los ítems bajo el umbral, peor primero. Vacío = sin
/// señales de contaminación. `None` si no hay base para calcularlo.
pub fn low_discrimination_items(
    cells: &[(String, String, bool)], // (respondente, item, pasó)
) -> Option<Vec<(String, f64)>> {
    use std::collections::BTreeMap;
    let mut by_resp: BTreeMap<&str, Vec<(&str, bool)>> = BTreeMap::new();
    for (r, i, ok) in cells {
        by_resp.entry(r).or_default().push((i, *ok));
    }
    // Con menos de 5 respondentes el estimador es ruido puro.
    if by_resp.len() < 5 {
        return None;
    }
    let totals: BTreeMap<&str, f64> = by_resp
        .iter()
        .map(|(r, v)| {
            let s = v.iter().filter(|(_, ok)| *ok).count() as f64;
            (*r, s)
        })
        .collect();

    let mut by_item: BTreeMap<&str, Vec<(f64, bool)>> = BTreeMap::new();
    for (r, i, ok) in cells {
        by_item
            .entry(i)
            .or_default()
            .push((totals[r.as_str()] - if *ok { 1.0 } else { 0.0 }, *ok));
    }

    let mut flagged = Vec::new();
    for (item, obs) in by_item {
        let n = obs.len() as f64;
        if n < 5.0 {
            continue;
        }
        let p = obs.iter().filter(|(_, ok)| *ok).count() as f64 / n;
        // Un ítem degenerado (0% o 100%) no tiene discriminación definida;
        // es su propio problema pero no el que este chequeo busca.
        if p <= 0.0 || p >= 1.0 {
            continue;
        }
        let mean = obs.iter().map(|(t, _)| t).sum::<f64>() / n;
        let sd = (obs.iter().map(|(t, _)| (t - mean).powi(2)).sum::<f64>() / n).sqrt();
        if sd <= 0.0 {
            continue;
        }
        let mean_ok = obs.iter().filter(|(_, o)| *o).map(|(t, _)| t).sum::<f64>() / (p * n);
        let r_pbis = (mean_ok - mean) / sd * (p / (1.0 - p)).sqrt();
        if r_pbis < LOW_DISCRIMINATION {
            flagged.push((item.to_string(), r_pbis));
        }
    }
    flagged.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    Some(flagged)
}

#[cfg(test)]
mod discrimination_tests {
    use super::*;

    fn cells(rows: &[(&str, &str, bool)]) -> Vec<(String, String, bool)> {
        rows.iter()
            .map(|(a, b, c)| (a.to_string(), b.to_string(), *c))
            .collect()
    }

    /// Un banco sano: los ítems ordenan igual que la habilidad — nadie se
    /// marca.
    #[test]
    fn a_healthy_bank_flags_nothing() {
        let mut rows = Vec::new();
        // 6 respondentes de habilidad creciente, 4 ítems de dificultad creciente.
        for (r, ability) in [
            ("r0", 0),
            ("r1", 1),
            ("r2", 2),
            ("r3", 3),
            ("r4", 4),
            ("r5", 4),
        ] {
            for (i, item) in ["i0", "i1", "i2", "i3"].iter().enumerate() {
                rows.push((r, *item, ability > i));
            }
        }
        let flagged = low_discrimination_items(&cells(&rows)).expect("hay base");
        assert!(
            flagged.is_empty(),
            "banco sano no debe marcar nada: {flagged:?}"
        );
    }

    /// El caso `read_file_basic`: un ítem que ANTI-correlaciona con la
    /// habilidad — los buenos fallan, los malos pasan. Debe marcarse.
    #[test]
    fn an_anti_correlated_item_is_flagged() {
        let mut rows = Vec::new();
        for (r, ability) in [
            ("r0", 0),
            ("r1", 1),
            ("r2", 2),
            ("r3", 3),
            ("r4", 4),
            ("r5", 4),
        ] {
            for (i, item) in ["i0", "i1", "i2", "i3"].iter().enumerate() {
                rows.push((r, *item, ability > i));
            }
            // El ítem contaminado: lo pasan los de habilidad BAJA.
            rows.push((r, "broken", ability <= 1));
        }
        let flagged = low_discrimination_items(&cells(&rows)).expect("hay base");
        assert_eq!(flagged.len(), 1, "{flagged:?}");
        assert_eq!(flagged[0].0, "broken");
        assert!(
            flagged[0].1 < 0.0,
            "anti-correlación debe ser negativa: {flagged:?}"
        );
    }

    /// Sin respondentes suficientes el estimador es ruido: no se inventa
    /// un diagnóstico.
    #[test]
    fn too_few_respondents_yields_no_verdict() {
        let rows = cells(&[
            ("r0", "i0", true),
            ("r0", "i1", false),
            ("r1", "i0", true),
            ("r1", "i1", true),
        ]);
        assert!(low_discrimination_items(&rows).is_none());
    }
}
