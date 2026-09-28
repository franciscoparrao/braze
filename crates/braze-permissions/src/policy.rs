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
use std::sync::{Arc, RwLock};

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
    /// Salidas a la red de las tools web (braze, perfil operador
    /// 2026-09-28). `match`: glob sobre el HOST de la URL
    /// (`*.wikipedia.org`, `docs.rs`), o sobre la URL completa si el
    /// patrón contiene `://` (`https://github.com/org/**`).
    Fetch,
}

impl PolicyAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Shell => "shell",
            Self::Write => "write",
            Self::Delete => "delete",
            Self::Read => "read",
            Self::Mcp => "mcp",
            Self::Fetch => "fetch",
        }
    }
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

    /// Agrega `rule` al FINAL del archivo `path` (creándolo con cabecera
    /// si no existe), con id único respecto de las reglas presentes, y
    /// valida el archivo resultante ANTES de escribirlo (tmp + rename):
    /// una política que dejó de parsear no arrancaría el binario. Devuelve
    /// la regla tal como quedó escrita (id posiblemente sufijado). Al
    /// final = las reglas anteriores (p.ej. un `deny`) siguen mandando:
    /// primera que matchea gana.
    pub fn append_rule_to_file(path: &Path, rule: &Rule) -> Result<Rule, PolicyError> {
        let existing = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => NEW_POLICY_HEADER.to_string(),
            Err(source) => {
                return Err(PolicyError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        let current = Policy::from_toml(&existing)?;
        let mut rule = rule.clone();
        let base_id = rule.id.clone();
        let mut n = 2;
        while current.rules.iter().any(|r| r.id == rule.id) {
            rule.id = format!("{base_id}-{n}");
            n += 1;
        }
        let new_text = format!("{}\n\n{}", existing.trim_end(), rule.to_toml());
        Policy::from_toml(&new_text)?;
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir).map_err(|source| PolicyError::Io {
                path: dir.to_path_buf(),
                source,
            })?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, new_text.as_bytes()).map_err(|source| PolicyError::Io {
            path: tmp.clone(),
            source,
        })?;
        std::fs::rename(&tmp, path).map_err(|source| PolicyError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(rule)
    }
}

/// Cabecera de un `policy.toml` creado por la respuesta "siempre" cuando
/// no existía ninguno.
const NEW_POLICY_HEADER: &str = "# braze — política de permisos (policy engine).\n\
# Primera regla que matchea gana; sin regla, `default = \"inherit\"` = el\n\
# clasificador base. Validar con `braze permissions policy`.\n\
\n\
version = 1\n\
default = \"inherit\"\n";

impl Rule {
    /// Render TOML de la regla (un bloque `[[rule]]`), con strings
    /// escapados por el crate `toml`.
    pub fn to_toml(&self) -> String {
        let quote = |s: &str| toml::Value::String(s.to_string()).to_string();
        let mut out = format!(
            "[[rule]]\nid = {}\naction = \"{}\"\n",
            quote(&self.id),
            self.action.label()
        );
        if !self.patterns.is_empty() {
            let patterns: Vec<String> = self.patterns.iter().map(|p| quote(p)).collect();
            out.push_str(&format!("match = [{}]\n", patterns.join(", ")));
        }
        out.push_str(&format!("verdict = \"{}\"\n", self.verdict.label()));
        if !self.reason.is_empty() {
            out.push_str(&format!("reason = {}\n", quote(&self.reason)));
        }
        out
    }
}

/// La regla `allow` que "siempre" en el prompt de confirmación deriva de
/// una acción — la misma generalización que Claude Code hace con "always
/// allow": ni la acción exacta (volvería a preguntar con otro argumento)
/// ni todo el programa cuando es un multiplexor (`git`, `cargo`, `npm`…:
/// aprobar `git commit` no aprueba `git push`).
///
/// - shell: basename del programa; para multiplexores conocidos,
///   `programa subcomando*`.
/// - write/delete/read: la ruta relativa exacta si está bajo `root`, o el
///   directorio padre absoluto con `/**` si está fuera.
/// - mcp: `servidor/tool`. fetch: el host.
/// - `Other`: nada (`None`).
pub fn rule_for_always(action: &ActionDescriptor, root: &Path) -> Option<Rule> {
    const MULTIPLEXERS: &[&str] = &[
        "git", "cargo", "npm", "npx", "pnpm", "yarn", "pip", "pip3", "uv", "poetry", "conda",
        "docker", "podman", "kubectl", "systemctl", "apt", "apt-get", "brew", "snap", "gh", "go",
    ];
    let (policy_action, pattern) = match action {
        ActionDescriptor::ShellCommand { command } => {
            let program = command.first()?;
            let base = Path::new(program)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| program.clone());
            let pattern = match command.get(1) {
                Some(sub) if MULTIPLEXERS.contains(&base.as_str()) && !sub.starts_with('-') => {
                    format!("{base} {sub}*")
                }
                _ => base,
            };
            (PolicyAction::Shell, pattern)
        }
        ActionDescriptor::WriteFile { path } => (PolicyAction::Write, path_pattern(path, root)),
        ActionDescriptor::DeleteFile { path } => (PolicyAction::Delete, path_pattern(path, root)),
        ActionDescriptor::ReadPath { path } => (PolicyAction::Read, path_pattern(path, root)),
        ActionDescriptor::McpToolCall { server, tool } => {
            (PolicyAction::Mcp, format!("{server}/{tool}"))
        }
        ActionDescriptor::Fetch { url } => (PolicyAction::Fetch, url_host(url)?),
        ActionDescriptor::Other { .. } => return None,
    };
    // Slug del id: para rutas, los dos últimos componentes (el comienzo
    // de una ruta absoluta es igual en todas y truncar por delante daba
    // ids repetidos); ≤ 32 chars.
    let slug_source: String = if pattern.contains('/') {
        pattern
            .trim_end_matches("/**")
            .rsplit('/')
            .filter(|c| !c.is_empty())
            .take(2)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("-")
    } else {
        pattern.clone()
    };
    let slug: String = slug_source
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(32)
        .collect::<String>()
        .trim_end_matches('-')
        .to_string();
    let slug = if slug.is_empty() {
        "rule".to_string()
    } else {
        slug
    };
    Some(Rule {
        id: format!("always-{}-{}", policy_action.label(), slug),
        action: policy_action,
        patterns: vec![pattern],
        verdict: Verdict::Allow,
        reason: "aprobado con 'siempre' desde el prompt de confirmación".to_string(),
    })
}

fn path_pattern(path: &Path, root: &Path) -> String {
    let abs = normalize_lexically(&if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    });
    match abs.strip_prefix(root) {
        Ok(rel) if !rel.as_os_str().is_empty() => rel.to_string_lossy().into_owned(),
        _ => match abs.parent() {
            // El directorio padre con `/**` solo si tiene al menos dos
            // componentes (`/home/u/notas`): `/**`, `/tmp/**` o `/home/**`
            // serían permisos gigantes derivados de un solo archivo.
            Some(dir) if dir.components().count() >= 3 => {
                format!("{}/**", dir.to_string_lossy().trim_end_matches('/'))
            }
            _ => abs.to_string_lossy().into_owned(),
        },
    }
}

/// Política compartida entre los clasificadores de todos los guards (uno
/// por provider) y el escritor de "siempre": una regla agregada en caliente
/// vale para el próximo `check` de cualquiera de ellos.
pub type SharedPolicy = Arc<RwLock<Policy>>;

/// "Siempre" en el prompt de confirmación: deriva la regla
/// ([`rule_for_always`]), la agrega al archivo de política y a la política
/// viva ([`SharedPolicy`]) en ese orden — si el archivo falla, nada cambia
/// en memoria y el caller decide (aprobar solo esta vez).
pub struct PolicyWriter {
    path: PathBuf,
    shared: SharedPolicy,
    root: PathBuf,
}

impl PolicyWriter {
    pub fn new(path: PathBuf, shared: SharedPolicy, root: PathBuf) -> Self {
        Self { path, shared, root }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn allow_always(&self, action: &ActionDescriptor) -> Result<Rule, PolicyError> {
        let rule = rule_for_always(action, &self.root).ok_or_else(|| {
            PolicyError::Invalid("esta acción no admite una regla permanente".to_string())
        })?;
        let rule = Policy::append_rule_to_file(&self.path, &rule)?;
        self.shared
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .rules
            .push(rule.clone());
        Ok(rule)
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
        (PolicyAction::Fetch, ActionDescriptor::Fetch { url }) => {
            let host = url_host(url).unwrap_or_default();
            rule.patterns.iter().any(|p| {
                if p.contains("://") {
                    glob(p, url)
                } else {
                    !host.is_empty() && glob(p, &host)
                }
            })
        }
        _ => false,
    }
}

/// Host de una URL `scheme://[user@]host[:port]/…`, en minúsculas y sin
/// puerto ni userinfo. `None` si no tiene `://`.
pub fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let host = authority.split(':').next().unwrap_or(authority);
    Some(host.to_ascii_lowercase())
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
    policy: SharedPolicy,
    base: Box<dyn ActionClassifier>,
    root: PathBuf,
}

impl PolicyClassifier {
    pub fn new(policy: Policy, base: Box<dyn ActionClassifier>, root: PathBuf) -> Self {
        Self::new_shared(Arc::new(RwLock::new(policy)), base, root)
    }

    /// Con una política compartida (ver [`SharedPolicy`]): las reglas que
    /// "siempre" agregue en caliente se ven en el próximo `decide`.
    pub fn new_shared(policy: SharedPolicy, base: Box<dyn ActionClassifier>, root: PathBuf) -> Self {
        Self { policy, base, root }
    }

    pub fn shared_policy(&self) -> SharedPolicy {
        Arc::clone(&self.policy)
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
        let policy = self
            .policy
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(rule) = policy.evaluate(action, &self.root) {
            return Decision {
                verdict: rule.verdict,
                rule: Some(rule.id.clone()),
                reason: (!rule.reason.is_empty()).then(|| rule.reason.clone()),
            };
        }
        match policy.default {
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

    /// `fetch`: glob sobre el host (sin puerto/userinfo, case-insensitive)
    /// o sobre la URL completa si el patrón lleva `://`; sin regla, el
    /// base lo marca `Confirm` (default-deny de red).
    #[test]
    fn fetch_rules_match_host_or_full_url() {
        let p = Policy::from_toml(
            "[[rule]]\nid=\"docs\"\naction=\"fetch\"\nmatch=[\"docs.rs\", \"*.wikipedia.org\", \"https://github.com/braze/**\"]\nverdict=\"allow\"\n\
             [[rule]]\nid=\"no-evil\"\naction=\"fetch\"\nmatch=[\"evil.example\"]\nverdict=\"deny\"\n",
        )
        .unwrap();
        let c = classifier(p);
        let f = |u: &str| ActionDescriptor::Fetch { url: u.to_string() };
        assert_eq!(c.decide(&f("https://docs.rs/tokio")).rule.as_deref(), Some("docs"));
        assert_eq!(c.decide(&f("https://user@DOCS.RS:443/x?q=1")).rule.as_deref(), Some("docs"), "host normalizado");
        assert_eq!(c.decide(&f("https://en.wikipedia.org/wiki/Rust")).verdict, Verdict::Allow);
        assert_eq!(c.decide(&f("https://wikipedia.org/")).verdict, Verdict::Confirm, "* no cubre el apex");
        assert_eq!(c.decide(&f("https://github.com/braze/x/blob/main/a.rs")).verdict, Verdict::Allow);
        assert_eq!(c.decide(&f("https://github.com/otro/x")).verdict, Verdict::Confirm, "URL completa");
        assert_eq!(c.decide(&f("http://evil.example/?d=secreto")).verdict, Verdict::Deny);
        assert_eq!(url_host("not a url"), None);
        assert_eq!(url_host("https://a.b:8080/c").as_deref(), Some("a.b"));
    }

    /// "Siempre": la regla derivada generaliza como Claude Code (programa
    /// / `git sub*` / directorio padre / host), el archivo se crea o
    /// extiende con id único y queda válido, y la política viva la ve en
    /// el acto para cualquier clasificador que la comparta.
    #[test]
    fn always_derives_a_rule_appends_it_to_the_file_and_applies_it_live() {
        let root = PathBuf::from("/ws");
        let r = |a: &ActionDescriptor| rule_for_always(a, &root).unwrap();
        assert_eq!(r(&sh(&["python3", "x.py"])).patterns, vec!["python3"]);
        assert_eq!(r(&sh(&["/usr/bin/git", "commit", "-m", "x"])).patterns, vec!["git commit*"]);
        assert_eq!(r(&sh(&["git", "-C", "/x", "status"])).patterns, vec!["git"], "flag primero: solo el programa");
        let w = |p: &str| ActionDescriptor::WriteFile { path: PathBuf::from(p) };
        assert_eq!(r(&w("/ws/.git/hooks/pre-commit")).patterns, vec![".git/hooks/pre-commit"]);
        assert_eq!(r(&w("/home/u/notas/a.md")).patterns, vec!["/home/u/notas/**"]);
        assert_eq!(r(&w("/home/u/notas/a.md")).id, "always-write-u-notas");
        assert_eq!(r(&w("/tmp/x.txt")).patterns, vec!["/tmp/x.txt"], "padre muy alto: ruta exacta");
        assert_eq!(r(&w("/x")).patterns, vec!["/x"]);
        assert_eq!(r(&ActionDescriptor::Fetch { url: "https://Docs.rs/x".into() }).patterns, vec!["docs.rs"]);
        assert_eq!(r(&ActionDescriptor::McpToolCall { server: "db".into(), tool: "query".into() }).patterns, vec!["db/query"]);
        assert!(rule_for_always(&ActionDescriptor::Other { label: "x".into() }, &root).is_none());
        let rule = r(&sh(&["python3"]));
        assert_eq!(rule.id, "always-shell-python3");
        assert!(rule.to_toml().starts_with("[[rule]]\nid = \"always-shell-python3\"\naction = \"shell\"\nmatch = [\"python3\"]\nverdict = \"allow\"\n"));

        let dir = std::env::temp_dir().join(format!("braze-policy-always-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("policy.toml");
        // Sin archivo: se crea con cabecera. Con archivo: se extiende, id único.
        let shared: SharedPolicy = Arc::new(RwLock::new(Policy::empty()));
        let writer = PolicyWriter::new(path.clone(), Arc::clone(&shared), root.clone());
        let classifier = PolicyClassifier::new_shared(
            Arc::clone(&shared),
            Box::new(DefaultClassifier::new(WorkdirAllowlist::new(root.clone()))),
            root.clone(),
        );
        assert_eq!(classifier.decide(&sh(&["python3", "a.py"])).verdict, Verdict::Confirm);
        let written = writer.allow_always(&sh(&["python3", "a.py"])).unwrap();
        assert_eq!(written.id, "always-shell-python3");
        assert_eq!(classifier.decide(&sh(&["python3", "otro.py"])).rule.as_deref(), Some("always-shell-python3"), "vale en caliente y para otro argumento");
        let again = writer.allow_always(&sh(&["python3"])).unwrap();
        assert_eq!(again.id, "always-shell-python3-2", "id único");
        let on_disk = Policy::load(&path).unwrap();
        assert_eq!(on_disk.rules.len(), 2);
        assert_eq!(on_disk.default, Fallback::Inherit);
        // Un deny anterior sigue mandando: la regla nueva va al final.
        std::fs::write(&path, "[[rule]]\nid=\"no-curl\"\naction=\"shell\"\nmatch=[\"curl\"]\nverdict=\"deny\"\n").unwrap();
        *shared.write().unwrap() = Policy::load(&path).unwrap();
        writer.allow_always(&sh(&["curl", "x"])).unwrap();
        assert_eq!(classifier.decide(&sh(&["curl", "y"])).verdict, Verdict::Deny);
        // Archivo inválido: error, nada cambia en memoria.
        std::fs::write(&path, "version = 9").unwrap();
        let before = shared.read().unwrap().rules.len();
        assert!(writer.allow_always(&sh(&["ls"])).is_err());
        assert_eq!(shared.read().unwrap().rules.len(), before);
        let _ = std::fs::remove_dir_all(&dir);
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
