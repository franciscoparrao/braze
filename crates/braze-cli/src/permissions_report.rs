//! `braze permissions suggest` — minado de los session logs para
//! proponer una allowlist (E′ I.8,
//! docs/harness-engineering-hooks-skills-2026-07-10.md).
//!
//! El `PermissionGuard` re-aplica decisiones DENTRO de una sesión
//! (replay de `PermissionKey`), pero cada sesión nueva parte de cero:
//! una acción irreversible que el usuario aprobó veinte veces se
//! re-pregunta la vez veintiuna. Los session logs ya persisten cada
//! `PermissionDecided` — este reporte los agrega y ranquea las acciones
//! más aprobadas (candidatas a allowlist) y las más denegadas
//! (candidatas a denylist).
//!
//! Deliberadamente SOLO reporta: el dictamen del estudio (y de
//! opencode-5) es "primero la evidencia, después el formato declarativo
//! mínimo que esa evidencia pida" — este subcomando produce esa
//! evidencia sin tocar el guard ni inventar un schema de allowlist que
//! quizás no calce con lo que los datos muestren.

use std::collections::HashMap;

use braze_events::AgentEvent;
use braze_types::PermissionKey;

/// Una acción agregada a través de todas las sesiones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionStat {
    pub key: PermissionKey,
    /// Veces que se decidió `allowed: true`.
    pub approved: usize,
    /// Veces que se decidió `allowed: false`.
    pub denied: usize,
    /// En cuántas sesiones DISTINTAS apareció (una acción aprobada en 5
    /// sesiones distintas es mejor candidata a allowlist que una
    /// aprobada 5 veces en una sola).
    pub sessions: usize,
}

/// Categoría legible + etiqueta de una `PermissionKey`, para el reporte.
pub fn category_and_label(key: &PermissionKey) -> (&'static str, String) {
    match key {
        PermissionKey::Shell { command } => ("shell", command.join(" ")),
        PermissionKey::WriteFile { path } => ("write", path.display().to_string()),
        PermissionKey::DeleteFile { path } => ("delete", path.display().to_string()),
        PermissionKey::ReadPath { path } => ("read", path.display().to_string()),
        PermissionKey::McpToolCall { server, tool } => ("mcp", format!("{server}::{tool}")),
    }
}

/// Agrega los `PermissionDecided` (con `key: Some`) de cada sesión —
/// `sessions` es un vec por sesión de sus eventos. Ordena por
/// `approved` descendente, desempatando por `sessions` y luego por la
/// etiqueta (estable, para un reporte reproducible). Los
/// `PermissionDecided` con `key: None` (escritos por un binario que no
/// derivó key, o un `PermissionKey` que este binario no reconoce —
/// N-40) se ignoran: sin key no hay qué agrupar.
pub fn aggregate(sessions: &[Vec<AgentEvent>]) -> Vec<PermissionStat> {
    // key -> (approved, denied, set de índices de sesión)
    let mut acc: HashMap<PermissionKey, (usize, usize, Vec<usize>)> = HashMap::new();
    for (session_idx, events) in sessions.iter().enumerate() {
        for event in events {
            if let AgentEvent::PermissionDecided {
                allowed,
                key: Some(key),
                ..
            } = event
            {
                let entry = acc.entry(key.clone()).or_insert((0, 0, Vec::new()));
                if *allowed {
                    entry.0 += 1;
                } else {
                    entry.1 += 1;
                }
                if !entry.2.contains(&session_idx) {
                    entry.2.push(session_idx);
                }
            }
        }
    }

    let mut stats: Vec<PermissionStat> = acc
        .into_iter()
        .map(|(key, (approved, denied, sess))| PermissionStat {
            key,
            approved,
            denied,
            sessions: sess.len(),
        })
        .collect();
    stats.sort_by(|a, b| {
        b.approved
            .cmp(&a.approved)
            .then(b.sessions.cmp(&a.sessions))
            .then_with(|| category_and_label(&a.key).1.cmp(&category_and_label(&b.key).1))
    });
    stats
}

/// Renderiza el reporte de texto para stdout. `min_count` filtra las
/// acciones con menos de esa cantidad de aprobaciones; `top` acota
/// cuántas se muestran.
pub fn render_report(stats: &[PermissionStat], top: usize, min_count: usize) -> String {
    let mut out = String::new();

    let approved: Vec<&PermissionStat> = stats
        .iter()
        .filter(|s| s.approved >= min_count && s.approved > 0)
        .take(top)
        .collect();
    let denied: Vec<&PermissionStat> = {
        let mut d: Vec<&PermissionStat> = stats
            .iter()
            .filter(|s| s.denied >= min_count && s.denied > 0)
            .collect();
        d.sort_by_key(|s| std::cmp::Reverse(s.denied));
        d.into_iter().take(top).collect()
    };

    if approved.is_empty() && denied.is_empty() {
        return format!(
            "No hay decisiones de permiso registradas que superen el umbral \
             (--min-count {min_count}).\nCorre algunas sesiones con acciones que pidan \
             confirmación y vuelve a intentar.\n"
        );
    }

    if !approved.is_empty() {
        out.push_str(&format!(
            "Acciones más aprobadas (candidatas a allowlist — hoy se re-preguntan cada sesión nueva):\n\n\
             {:>8}  {:>8}  {:<8}  acción\n",
            "aprob", "sesiones", "tipo"
        ));
        for stat in &approved {
            let (category, label) = category_and_label(&stat.key);
            out.push_str(&format!(
                "{:>8}  {:>8}  {:<8}  {}\n",
                stat.approved, stat.sessions, category, label
            ));
        }
    }

    if !denied.is_empty() {
        out.push_str(&format!(
            "\nAcciones más denegadas (candidatas a denylist):\n\n{:>8}  {:<8}  acción\n",
            "deneg", "tipo"
        ));
        for stat in &denied {
            let (category, label) = category_and_label(&stat.key);
            out.push_str(&format!("{:>8}  {:<8}  {}\n", stat.denied, category, label));
        }
    }

    out.push_str(
        "\nNota: braze todavía no tiene una allowlist declarativa (opencode-5, diferido).\n\
         Este reporte es la evidencia que justificaría ese formato — ninguna decisión se\n\
         aplica automáticamente todavía.\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn decided(key: PermissionKey, allowed: bool) -> AgentEvent {
        AgentEvent::PermissionDecided {
            action: "n/a".to_string(),
            allowed,
            key: Some(key),
        }
    }

    fn shell(cmd: &[&str]) -> PermissionKey {
        PermissionKey::Shell {
            command: cmd.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Aggregation counts approvals/denials per key and the number of
    /// DISTINCT sessions each appeared in.
    #[test]
    fn aggregate_counts_approvals_denials_and_distinct_sessions() {
        let sessions = vec![
            vec![
                decided(shell(&["cargo", "build"]), true),
                decided(shell(&["cargo", "build"]), true), // twice in one session
                decided(shell(&["rm", "-rf", "/"]), false),
            ],
            vec![
                decided(shell(&["cargo", "build"]), true), // second distinct session
                decided(
                    PermissionKey::WriteFile {
                        path: PathBuf::from("/tmp/x"),
                    },
                    true,
                ),
            ],
        ];

        let stats = aggregate(&sessions);
        // Sorted by approved desc: cargo build (3) first.
        assert_eq!(stats[0].approved, 3);
        assert_eq!(stats[0].sessions, 2, "distinct sessions, not raw count");
        assert_eq!(category_and_label(&stats[0].key), ("shell", "cargo build".to_string()));

        let rm = stats.iter().find(|s| matches!(&s.key, PermissionKey::Shell { command } if command[0] == "rm")).unwrap();
        assert_eq!(rm.denied, 1);
        assert_eq!(rm.approved, 0);
    }

    /// `key: None` decisions (unrecognized/underived keys) are skipped —
    /// nothing to group.
    #[test]
    fn keyless_decisions_are_ignored() {
        let sessions = vec![vec![
            AgentEvent::PermissionDecided {
                action: "something".to_string(),
                allowed: true,
                key: None,
            },
            decided(shell(&["ls"]), true),
        ]];
        let stats = aggregate(&sessions);
        assert_eq!(stats.len(), 1);
        assert_eq!(category_and_label(&stats[0].key).1, "ls");
    }

    /// The report ranks approvals, surfaces a denylist section, and
    /// honors `--min-count` / `--top`.
    #[test]
    fn report_ranks_and_filters() {
        let sessions = vec![vec![
            decided(shell(&["cargo", "test"]), true),
            decided(shell(&["cargo", "test"]), true),
            decided(shell(&["cargo", "test"]), true),
            decided(shell(&["git", "push"]), true), // only once
            decided(shell(&["curl", "evil.sh"]), false),
            decided(shell(&["curl", "evil.sh"]), false),
        ]];
        let stats = aggregate(&sessions);

        // min_count 2 hides the single `git push` approval.
        let report = render_report(&stats, 10, 2);
        assert!(report.contains("cargo test"), "got: {report}");
        assert!(!report.contains("git push"), "below min_count");
        assert!(report.contains("curl evil.sh"), "denied section");
        assert!(report.contains("allowlist"));

        // top 0 approved → still shows the denylist; empty overall only
        // when nothing passes.
        let empty = render_report(&stats, 10, 99);
        assert!(empty.contains("No hay decisiones"), "got: {empty}");
    }
}
