//! Orden de corrida intercalado (`--interleave <seed>`, 2026-11-17).
//!
//! Por qué existe: el Estudio 2 del Paper 2 corrió sus tres brazos en
//! secuencia (baseline entero, después empty, después seeded) y el brazo
//! baseline salió 1,7× más lento por ronda en el mismo nodo. Con brazos en
//! secuencia, el brazo y el momento de la corrida quedan confundidos: una
//! deriva del nodo se lee como efecto del tratamiento. Intercalar reparte
//! esa deriva entre los brazos en vez de separarlos.
//!
//! Diseño: bloques (repetición, tarea) en orden repetición-mayor —cada
//! repetición recorre la suite entera antes de la siguiente— y, dentro de
//! cada bloque, los brazos en una permutación aleatoria propia. El
//! generador es SplitMix64 sembrado con la semilla del flag: el orden es
//! reproducible desde la metadata sin depender de un crate externo.

/// Una unidad del plan: (índice de brazo, índice de tarea, repetición).
pub type Unit = (usize, usize, u32);

/// SplitMix64: pequeño, determinista y con buena dispersión para barajar.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Entero uniforme en `0..=bound` (rechazo para evitar sesgo de módulo).
    fn below_or_eq(&mut self, bound: u64) -> u64 {
        let range = bound + 1;
        let zone = u64::MAX - (u64::MAX % range);
        loop {
            let v = self.next();
            if v < zone {
                return v % range;
            }
        }
    }
}

/// Plan intercalado: repetición-mayor, tarea, y dentro de cada bloque una
/// permutación Fisher-Yates de los brazos.
pub fn schedule(n_arms: usize, n_tasks: usize, repetitions: u32, seed: u64) -> Vec<Unit> {
    let mut rng = SplitMix64(seed);
    let mut plan = Vec::with_capacity(n_arms * n_tasks * repetitions as usize);
    for repetition in 0..repetitions {
        for task in 0..n_tasks {
            let mut arms: Vec<usize> = (0..n_arms).collect();
            for i in (1..arms.len()).rev() {
                let j = rng.below_or_eq(i as u64) as usize;
                arms.swap(i, j);
            }
            plan.extend(arms.into_iter().map(|arm| (arm, task, repetition)));
        }
    }
    plan
}

/// Plan por brazo (el orden histórico del bench): brazo, tarea, repetición.
pub fn arm_major(n_arms: usize, n_tasks: usize, repetitions: u32) -> Vec<Unit> {
    let mut plan = Vec::with_capacity(n_arms * n_tasks * repetitions as usize);
    for arm in 0..n_arms {
        for task in 0..n_tasks {
            for repetition in 0..repetitions {
                plan.push((arm, task, repetition));
            }
        }
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_block_is_a_permutation_of_the_arms() {
        let plan = schedule(3, 34, 3, 7);
        assert_eq!(plan.len(), 3 * 34 * 3);
        for block in plan.chunks(3) {
            let (_, task, rep) = block[0];
            assert!(block.iter().all(|&(_, t, r)| t == task && r == rep));
            let arms: HashSet<usize> = block.iter().map(|&(a, _, _)| a).collect();
            assert_eq!(arms, HashSet::from([0, 1, 2]));
        }
    }

    #[test]
    fn blocks_run_repetition_major() {
        let plan = schedule(2, 3, 2, 1);
        let blocks: Vec<(usize, u32)> = plan.chunks(2).map(|b| (b[0].1, b[0].2)).collect();
        assert_eq!(blocks, vec![(0, 0), (1, 0), (2, 0), (0, 1), (1, 1), (2, 1)]);
    }

    #[test]
    fn the_same_seed_gives_the_same_plan_and_another_seed_does_not() {
        assert_eq!(schedule(3, 34, 3, 42), schedule(3, 34, 3, 42));
        assert_ne!(schedule(3, 34, 3, 42), schedule(3, 34, 3, 43));
    }

    #[test]
    fn no_arm_systematically_runs_first() {
        // Con 3 brazos y 1.020 bloques, cada brazo debería ir primero en
        // ~1/3 de los bloques; el margen de ±5 pp es holgado (≈5 DE).
        let plan = schedule(3, 340, 3, 2026);
        let blocks = plan.len() / 3;
        for arm in 0..3 {
            let first = plan.chunks(3).filter(|b| b[0].0 == arm).count();
            let share = first as f64 / blocks as f64;
            assert!((share - 1.0 / 3.0).abs() < 0.05, "brazo {arm}: {share:.3}");
        }
    }

    #[test]
    fn arm_major_keeps_the_historical_order() {
        assert_eq!(
            arm_major(2, 2, 2),
            vec![
                (0, 0, 0),
                (0, 0, 1),
                (0, 1, 0),
                (0, 1, 1),
                (1, 0, 0),
                (1, 0, 1),
                (1, 1, 0),
                (1, 1, 1)
            ]
        );
    }

    #[test]
    fn a_single_arm_plan_is_just_the_blocks_in_order() {
        let plan = schedule(1, 2, 2, 9);
        assert_eq!(plan, vec![(0, 0, 0), (0, 1, 0), (0, 0, 1), (0, 1, 1)]);
    }
}
