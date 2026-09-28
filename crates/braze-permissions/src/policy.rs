//! Policy engine declarativo (Enclave M3): reglas en TOML sobre el choke
//! point [`crate::PermissionGuard::check`], por encima del
//! [`DefaultClassifier`](crate::DefaultClassifier).
//!
//! El clasificador base decide "reversible / irreversible" con reglas
//! escritas en código. Un cliente regulado necesita expresar su política
//! sin recompilar y con un veredicto que el base no tiene: **deny** — no
//! preguntar, no ejecutar (p.ej. `curl`/`ssh` en un build soberano, leer
//! `.env`, escribir fuera de `reports/`). Formato:
//!
//! ```toml
//! version = 1
//! default = "inherit"          # sin regla que matchee: inherit | allow | confirm | deny
//!
//! [[rule]]
//! id = "no-network-tools"
//! action = "shell"             # shell | write | delete | read | mcp | any
//! match = ["curl", "wget", "ssh", "scp", "nc", "socat", "git push*"]
//! verdict = "deny"
//! reason = "sin egress: herramientas de red prohibidas"
//!
//! [[rule]]
//! id = "reports-writable"
//! action = "write"
//! match = ["reports/**"]
//! verdict = "allow"
//! ```
//!
//! Semántica: **primera regla que matchea gana**, en orden del archivo.
//! `match` para `shell` compara el basename del programa (glob), o —si el
//! patrón tiene espacios— el comando completo unido por espacios (glob).
//! Para `write`/`delete`/`read`, glob de ruta: absoluto si empieza con `/`,
//! si no relativo al workdir (una ruta fuera del workdir no matchea un
//! patrón relativo). `mcp`: glob sobre `servidor/tool`. `any` sin `match`
//! matchea todo (regla catch-all); con `match`, glob sobre la descripción
//! de la acción. Globs: `*` (sin `/`), `**` (con `/`), `?`.
//!
//! La política se carga de donde el modelo NO puede escribir
//! (`<session_dir>/policy.toml`, nunca del workdir): una política que el
//! agente pudiera editar sería auto-escalación. Una política inválida es
//! error de arranque, no un warning.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::action::ActionDescriptor;
use crate::allowlist::normalize_lexically;
use crate::classifier::{ActionClassifier, Decision, Reversibility, Verdict};

/// Sobre qué acciones aplica una regla.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PolicyAction {
    Any,
    Shell,
    Write,
    Delete,
    Read,
    Mcp,
}

/// Qué hacer cuando ninguna regla matchea.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Fallback {
    /// Lo que diga el clasificador base (`DefaultClassifier`).
    #[default]
    Inherit,
    Allow,
    Confirm,
    Deny,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub id: String,
    pub action: PolicyAction,
    #[serde(default, rename = "match")]
    pub patterns: Vec<String>,
    pub verdict: Verdict,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub default: Fallback,
    #[serde(default, rename = "rule")]
    pub rules: Vec<Rule>,
}

fn default_version() -> u32 {
    1
}

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("policy: no se pudo leer {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("policy: TOML inválido: {0}")]
    Parse(String),
    #[error("policy: inválida: {0}")]
    Invalid(String),
}

impl Policy {
    /// Política vacía: `default = inherit`, sin reglas (equivale a no tener
    /// política).
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn from_toml(text: &str) -> Result<Self, PolicyError> {
        let policy: Policy = toml::from_str(text).map_err(|e| PolicyError::Parse(e.to_string()))?;
        policy.validate()?;
        Ok(policy)
    }

    pub fn load(path: &Path) -> Result<Self, PolicyError> {
        let text = std::fs::read_to_string(path).map_err(|source| PolicyError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_toml(&text)
    }

    /// Versión soportada, ids únicos y no vacíos, `match` no vacío salvo
    /// para `any` (catch-all).
    pub fn validate(&self) -> Result<(), PolicyError> {
        if self.version != 1 {
            return Err(PolicyError::Invalid(format!("version {} no soportada (solo 1)", self.version)));
        }
        let mut seen = std::collections::HashSet::new();
        for (i, r) in self.rules.iter().enumerate() {
            if r.id.trim().is_empty() {
                return Err(PolicyError::Invalid(format!("regla #{i}: id vacío")));
            }
            if !seen.insert(r.id.as_str()) {
                return Err(PolicyError::Invalid(format!("regla `{}`: id repetido", r.id)));
            }
            if r.patterns.is_empty() && r.action != PolicyAction::Any {
                return Err(PolicyError::Invalid(format!(
                    "regla `{}`: `match` vacío solo vale con action = \"any\"",
                    r.id
                )));
            }
            if r.patterns.iter().any(|p| p.trim().is_empty()) {
                return Err(PolicyError::Invalid(format!("regla `{}`: patrón vacío", r.id)));
            }
        }
        Ok(())
    }

    /// Primera regla que matchea `action` (rutas relativas resueltas contra
    /// `root`), o `None`.
    pub fn evaluate(&self, action: &ActionDescriptor, root: &Path) -> Option<&Rule> {
        self.rules.iter().find(|r| rule_matches(r, action, root))
    }
}

fn rule_matches(rule: &Rule, action: &ActionDescriptor, root: &Path) -> bool {
    match (rule.action, action) {
        (PolicyAction::Any, _) => {
            rule.patterns.is_empty() || rule.patterns.iter().any(|p| glob(p, &action.to_string()))
        }
        (PolicyAction::Shell, ActionDescriptor::ShellCommand { command }) => {
            let Some(program) = command.first() else {
                return false;
            };
            let base = Path::new(program)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| program.clone());
            let joined = command.join(" ");
            rule.patterns.iter().any(|p| {
                if p.contains(char::is_whitespace) {
                    glob(p, &joined)
                } else {
                    glob(p, &base)
                }
            })
        }
        (PolicyAction::Write, ActionDescriptor::WriteFile { path })
        | (PolicyAction::Delete, ActionDescriptor::DeleteFile { path })
        | (PolicyAction::Read, ActionDescriptor::ReadPath { path }) => path_matches(&rule.patterns, path, root),
        (PolicyAction::Mcp, ActionDescriptor::McpToolCall { server, tool }) => {
            let full = format!("{server}/{tool}");
            rule.patterns.iter().any(|p| glob(p, &full) || glob(p, server))
        }
        _ => false,
    }
}

fn path_matches(patterns: &[String], path: &Path, root: &Path) -> bool {
    let abs = normalize_lexically(&if path.is_absolute() { path.to_path_buf() } else { root.join(path) });
    let abs_s = abs.to_string_lossy();
    let rel_s = abs.strip_prefix(root).ok().map(|r| r.to_string_lossy().into_owned());
    patterns.iter().any(|p| {
        if p.starts_with('/') {
            glob(p, &abs_s)
        } else {
            rel_s.as_deref().is_some_and(|r| glob(p, r))
        }
    })
}

/// Glob mínimo: `*` = cualquier secuencia sin `/`, `**` = cualquier
/// secuencia (con `/`), `?` = un char que no es `/`. Sin clases ni
/// alternancias: los patrones de una política son cortos y legibles.
pub fn glob(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    glob_match(&p, &t)
}

fn glob_match(p: &[char], t: &[char]) -> bool {
    match p.first() {
        None => t.is_empty(),
        Some('*') if p.get(1) == Some(&'*') => {
            // `**` (y `**/`): cualquier cosa, incluidos separadores y nada.
            let mut rest = &p[2..];
            if rest.first() == Some(&'/') {
                rest = &rest[1..];
            }
            (0..=t.len()).any(|i| glob_match(rest, &t[i..]))
        }
        Some('*') => {
            // `*`: cualquier cosa hasta el próximo `/` (sin cruzarlo).
            let limit = t.iter().position(|&c| c == '/').unwrap_or(t.len());
            (0..=limit).any(|i| glob_match(&p[1..], &t[i..]))
        }
        Some('?') => !t.is_empty() && t[0] != '/' && glob_match(&p[1..], &t[1..]),
        Some(&c) => !t.is_empty() && t[0] == c && glob_match(&p[1..], &t[1..]),
    }
}

/// [`ActionClassifier`] que aplica una [`Policy`] antes del clasificador
/// base: la regla que matchea manda; sin regla, `default` decide (y
/// `inherit` delega al base).
pub struct PolicyClassifier {
    policy: Policy,
    base: Box<dyn ActionClassifier>,
    root: PathBuf,
}

impl PolicyClassifier {
    pub fn new(policy: Policy, base: Box<dyn ActionClassifier>, root: PathBuf) -> Self {
        Self { policy, base, root }
    }

    pub fn policy(&self) -> &Policy {
        &self.policy
    }
}

impl ActionClassifier for PolicyClassifier {
    fn classify(&self, action: &ActionDescriptor) -> Reversibility {
        match self.decide(action).verdict {
            Verdict::Allow => Reversibility::Reversible,
            Verdict::Confirm | Verdict::Deny => Reversibility::Irreversible,
        }
    }

    fn decide(&self, action: &ActionDescriptor) -> Decision {
        if let Some(rule) = self.policy.evaluate(action, &self.root) {
            return Decision {
                verdict: rule.verdict,
                rule: Some(rule.id.clone()),
                reason: (!rule.reason.is_empty()).then(|| rule.reason.clone()),
            };
        }
        match self.policy.default {
            Fallback::Inherit => self.base.decide(action),
            Fallback::Allow => Decision::bare(Verdict::Allow),
            Fallback::Confirm => Decision::bare(Verdict::Confirm),
            Fallback::Deny => Decision {
                verdict: Verdict::Deny,
                rule: Some("default".to_string()),
                reason: Some("la política no permite acciones no listadas".to_string()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::allowlist::WorkdirAllowlist;
    use crate::classifier::DefaultClassifier;

    const SAMPLE: &str = r#"
version = 1
default = "inherit"

[[rule]]
id = "no-network-tools"
action = "shell"
match = ["curl", "wget", "ssh", "scp", "git push*"]
verdict = "deny"
reason = "sin egress"

[[rule]]
id = "reports-writable"
action = "write"
match = ["reports/**"]
verdict = "allow"

[[rule]]
id = "no-secrets"
action = "read"
match = ["**/.env", "**/.env.*", "/etc/shadow"]
verdict = "deny"

[[rule]]
id = "confirm-mcp-db"
action = "mcp"
match = ["db/*"]
verdict = "confirm"
"#;

    fn classifier(policy: Policy) -> PolicyClassifier {
        let root = PathBuf::from("/ws");
        PolicyClassifier::new(
            policy,
            Box::new(DefaultClassifier::new(WorkdirAllowlist::new(root.clone()))),
            root,
        )
    }

    fn sh(cmd: &[&str]) -> ActionDescriptor {
        ActionDescriptor::ShellCommand {
            command: cmd.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn glob_semantics() {
        assert!(glob("curl", "curl"));
        assert!(!glob("curl", "curly"));
        assert!(glob("git push*", "git push origin main"));
        assert!(glob("reports/**", "reports/2026/q3.md"));
        assert!(glob("reports/**", "reports/x.md"));
        assert!(!glob("reports/*", "reports/2026/q3.md"), "* no cruza /");
        assert!(glob("**/.env", ".env"), "** vacío");
        assert!(glob("**/.env", "a/b/.env"));
        assert!(glob("**/.env.*", "a/.env.local"));
        assert!(!glob("**/.env", "a/.envx"));
        assert!(glob("db/?", "db/x") && !glob("db/?", "db/xy"));
    }

    #[test]
    fn first_matching_rule_wins_and_default_inherits() {
        let c = classifier(Policy::from_toml(SAMPLE).unwrap());
        let d = c.decide(&sh(&["curl", "http://x"]));
        assert_eq!((d.verdict, d.rule.as_deref()), (Verdict::Deny, Some("no-network-tools")));
        assert_eq!(d.reason.as_deref(), Some("sin egress"));
        assert_eq!(c.decide(&sh(&["/usr/bin/ssh", "h"])).verdict, Verdict::Deny, "basename");
        assert_eq!(c.decide(&sh(&["git", "push", "origin"])).verdict, Verdict::Deny, "comando completo");
        // Sin regla: hereda del base (git status es seguro → allow; rm → confirm).
        let d = c.decide(&sh(&["git", "status"]));
        assert_eq!((d.verdict, d.rule), (Verdict::Allow, None));
        assert_eq!(c.decide(&sh(&["rm", "x"])).verdict, Verdict::Confirm);
        assert_eq!(c.classify(&sh(&["curl"])), Reversibility::Irreversible);
    }

    #[test]
    fn path_rules_are_relative_to_root_or_absolute() {
        let c = classifier(Policy::from_toml(SAMPLE).unwrap());
        let w = |p: &str| ActionDescriptor::WriteFile { path: PathBuf::from(p) };
        let r = |p: &str| ActionDescriptor::ReadPath { path: PathBuf::from(p) };
        assert_eq!(c.decide(&w("/ws/reports/q3.md")).rule.as_deref(), Some("reports-writable"));
        assert_eq!(c.decide(&w("reports/q3.md")).rule.as_deref(), Some("reports-writable"), "relativa al root");
        assert_eq!(c.decide(&w("/otro/reports/q3.md")).rule, None, "fuera del root no matchea un patrón relativo");
        assert_eq!(c.decide(&r("/ws/app/.env")).verdict, Verdict::Deny);
        assert_eq!(c.decide(&r("/etc/shadow")).verdict, Verdict::Deny, "patrón absoluto");
        // El base sigue mandando donde la política calla: leer dentro del ws es allow.
        assert_eq!(c.decide(&r("/ws/README.md")).verdict, Verdict::Allow);
        let m = ActionDescriptor::McpToolCall { server: "db".into(), tool: "query".into() };
        assert_eq!(c.decide(&m).rule.as_deref(), Some("confirm-mcp-db"));
    }

    #[test]
    fn default_deny_is_a_catch_all_with_a_named_rule() {
        let p = Policy::from_toml("default = \"deny\"\n[[rule]]\nid=\"ok\"\naction=\"shell\"\nmatch=[\"ls\"]\nverdict=\"allow\"\n").unwrap();
        let c = classifier(p);
        assert_eq!(c.decide(&sh(&["ls"])).verdict, Verdict::Allow);
        let d = c.decide(&sh(&["cat", "x"]));
        assert_eq!((d.verdict, d.rule.as_deref()), (Verdict::Deny, Some("default")));
        // `any` sin match: catch-all explícito.
        let p = Policy::from_toml("[[rule]]\nid=\"all\"\naction=\"any\"\nverdict=\"confirm\"\n").unwrap();
        assert_eq!(classifier(p).decide(&sh(&["ls"])).verdict, Verdict::Confirm);
    }

    #[test]
    fn invalid_policies_are_rejected() {
        assert!(Policy::from_toml("version = 2").is_err());
        assert!(Policy::from_toml("[[rule]]\nid=\"\"\naction=\"shell\"\nmatch=[\"x\"]\nverdict=\"deny\"").is_err());
        assert!(Policy::from_toml("[[rule]]\nid=\"a\"\naction=\"shell\"\nmatch=[\"x\"]\nverdict=\"deny\"\n[[rule]]\nid=\"a\"\naction=\"shell\"\nmatch=[\"y\"]\nverdict=\"deny\"").is_err(), "id repetido");
        assert!(Policy::from_toml("[[rule]]\nid=\"a\"\naction=\"shell\"\nverdict=\"deny\"").is_err(), "match vacío fuera de any");
        assert!(Policy::from_toml("[[rule]]\nid=\"a\"\naction=\"shell\"\nmatch=[\"x\"]\nverdict=\"maybe\"").is_err(), "veredicto desconocido");
        assert!(Policy::from_toml("[[rule]]\nid=\"a\"\naction=\"shell\"\nmatch=[\"x\"]\nverdict=\"deny\"\nextra=1").is_err(), "campo desconocido");
        assert!(Policy::from_toml("").unwrap().rules.is_empty());
    }
}
