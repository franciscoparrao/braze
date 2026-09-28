//! `braze-skills` — memoria procedural con disclosure progresivo (D′,
//! docs/harness-engineering-hooks-skills-2026-07-10.md § Parte III).
//!
//! Una skill NO es "más system prompt": es una unidad de guía
//! procedural con metadata barata siempre indexable (`SkillStub`) y un
//! body que solo entra al contexto cuando la tarea lo pide — el mismo
//! patrón de carga diferida que las tools tuvieron desde el MVP,
//! aplicado a instrucciones.
//!
//! Decisiones de la v1, todas del estudio consolidado:
//!
//! - **Registry first-class, no ToolProvider**: el harness puede cargar
//!   la guía ANTES del primer error del executor; una tool `load_skill`
//!   que el modelo tendría que pedir llega tarde para un SLM.
//! - **Solo mención explícita** (`$nombre` en el input del usuario) —
//!   el router automático es el Paquete D del estudio y queda gateado
//!   por A/B: delegar la selección a un modelo chico o a un ranking sin
//!   medir es exactamente lo que el estudio prohíbe.
//! - **Sin URLs remotas**: requieren cache, checksum, permisos y
//!   política de actualización — `allow_remote` ni siquiera existe
//!   todavía.
//! - **NO cargar las skills de un entorno frontier tal cual**: muchas
//!   están escritas para modelos grandes y en un 3B son distractores.
//!   Los `paths` de config son una allowlist deliberada, vacía por
//!   default (= feature apagada).
//!
//! Formato: `SKILL.md` con frontmatter delimitado por `---` conteniendo
//! `name:` y `description:`, body markdown después. Parser mínimo a
//! mano — no vale una dependencia YAML para dos campos.

use std::path::{Path, PathBuf};

/// Cuántos niveles bajo cada path de config se busca `SKILL.md` — cubre
/// el layout habitual `skills/<nombre>/SKILL.md` con margen, sin
/// recorrer un árbol entero por accidente.
const MAX_DISCOVERY_DEPTH: usize = 3;

/// Body máximo leído por archivo (bytes) — una skill más grande que
/// esto está escrita para un modelo frontier, no para un SLM; se indexa
/// igual (stub) pero su body se trunca al cargar.
const MAX_BODY_BYTES: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error("skill path {0:?} is not readable: {1}")]
    Unreadable(PathBuf, std::io::Error),
}

/// La metadata barata, siempre indexable — lo único que existe en
/// memoria hasta que alguien pide el body.
#[derive(Debug, Clone)]
pub struct SkillStub {
    /// Normalizado: lowercase, espacios→guiones — `$Mi Skill` y
    /// `$mi-skill` refieren a la misma.
    pub name: String,
    pub description: String,
    /// El `SKILL.md` de origen (el body se relee de acá al cargar).
    pub path: PathBuf,
    /// ~4 chars/token sobre el body completo — para que quien inyecta
    /// sepa cuánto va a costar ANTES de cargar.
    pub estimated_tokens: u32,
    /// Nombres de tool para las que esta skill es la guía relevante
    /// (frontmatter `tools: edit_file, write_file`), normalizados a
    /// lowercase. Vacío = la skill solo entra por mención explícita, que
    /// es el comportamiento D′ de siempre.
    ///
    /// Habilita la invocación *call-time* (Recuris, arXiv:2608.24876
    /// § 2.2.2): el harness usa el nombre de la tool que el modelo acaba
    /// de redactar como clave de recuperación, de modo que la guía llega
    /// ANTES de que la acción se ejecute y no después de que falle.
    pub tools: Vec<String>,
}

/// Registro inmutable post-discovery. Duplicados por nombre: gana el
/// path de config MÁS TEMPRANO (prioridad de lista, con warning) —
/// determinista, igual que la resolución de providers del ToolRegistry.
#[derive(Debug, Default)]
pub struct SkillRegistry {
    skills: Vec<SkillStub>,
}

impl SkillRegistry {
    /// Discovery sobre los `paths` de config, en orden. Un path
    /// inexistente se salta con warning (config global compartida entre
    /// máquinas), nunca falla el arranque.
    pub fn discover(paths: &[PathBuf]) -> Self {
        let mut skills: Vec<SkillStub> = Vec::new();
        for root in paths {
            if !root.exists() {
                tracing::warn!(path = ?root, "skills path does not exist; skipping");
                continue;
            }
            let mut found = Vec::new();
            collect_skill_files(root, 0, &mut found);
            // Orden estable dentro del root: por profundidad primero, así
            // ante nombres duplicados gana el `SKILL.md` más superficial
            // (`skills/<n>/SKILL.md` sobre una copia en
            // `skills/synced/<uuid>/<n>/SKILL.md`, el layout que Claude
            // Code deja al sincronizar — perfil operador, 2026-09-28).
            found.sort();
            for (_, file) in found {
                match parse_skill_file(&file) {
                    Some(stub) => {
                        if let Some(existing) = skills.iter().find(|s| s.name == stub.name) {
                            tracing::warn!(
                                name = %stub.name,
                                kept = ?existing.path,
                                ignored = ?stub.path,
                                "duplicate skill name; earlier config path / shallower file wins"
                            );
                        } else {
                            skills.push(stub);
                        }
                    }
                    None => {
                        tracing::warn!(
                            path = ?file,
                            "SKILL.md without a valid frontmatter (name/description); skipped"
                        );
                    }
                }
            }
        }
        tracing::info!(count = skills.len(), "skill discovery complete");
        Self { skills }
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    pub fn stubs(&self) -> &[SkillStub] {
        &self.skills
    }

    /// Lookup por nombre normalizado.
    pub fn find(&self, name: &str) -> Option<&SkillStub> {
        let wanted = normalize_name(name);
        self.skills.iter().find(|s| s.name == wanted)
    }

    /// La skill declarada como guía de `tool`, si hay alguna — la clave
    /// de recuperación de la invocación *call-time*.
    ///
    /// Determinista ante varias candidatas: gana la primera en el orden
    /// del registro (que ya es el orden de prioridad de paths de config),
    /// igual que la resolución de duplicados por nombre. Una skill sin
    /// `tools:` nunca matchea: sigue siendo explicit-only.
    pub fn for_tool(&self, tool: &str) -> Option<&SkillStub> {
        let wanted = tool.trim().to_lowercase();
        self.skills.iter().find(|s| s.tools.contains(&wanted))
    }

    /// Relee y devuelve el body de `name`, capado a `max_body_tokens`
    /// (~4 chars/token). `None` si la skill no existe o el archivo dejó
    /// de ser legible (se degradó desde el discovery — warning, no
    /// error: la skill simplemente no se carga).
    pub fn load_body(&self, name: &str, max_body_tokens: usize) -> Option<LoadedSkill> {
        self.load_body_with_args(name, max_body_tokens, "")
    }

    /// Como [`Self::load_body`], sustituyendo `$ARGUMENTS` en el body por
    /// `args` (el texto que siguió a la mención — ver
    /// [`Self::mentions_with_args`]), ANTES de capar. Es el contrato de
    /// argumentos de las skills de Claude Code; un body sin `$ARGUMENTS`
    /// no cambia (el texto del usuario sigue en su mensaje de todos
    /// modos).
    pub fn load_body_with_args(
        &self,
        name: &str,
        max_body_tokens: usize,
        args: &str,
    ) -> Option<LoadedSkill> {
        let stub = self.find(name)?;
        let raw = match std::fs::read_to_string(&stub.path) {
            Ok(contents) => contents,
            Err(err) => {
                tracing::warn!(path = ?stub.path, error = %err, "skill body unreadable at load time");
                return None;
            }
        };
        let body = skill_body(&raw);
        let substituted;
        let body: &str = if body.contains(ARGUMENTS_PLACEHOLDER) {
            substituted = body.replace(ARGUMENTS_PLACEHOLDER, args.trim());
            &substituted
        } else {
            body
        };
        let cap_chars = max_body_tokens.saturating_mul(4);
        let (body, truncated) = if body.len() > cap_chars {
            // Corte en boundary de char para no partir UTF-8.
            let mut end = cap_chars;
            while !body.is_char_boundary(end) {
                end -= 1;
            }
            (
                format!(
                    "{}\n[skill body truncated at {max_body_tokens} tokens]",
                    &body[..end]
                ),
                true,
            )
        } else {
            (body.to_string(), false)
        };
        Some(LoadedSkill {
            name: stub.name.clone(),
            body,
            truncated,
            estimated_tokens: stub.estimated_tokens.min(max_body_tokens as u32),
        })
    }

    /// Extrae las menciones `$skill` de un input de usuario que
    /// resuelven contra este registry, en orden de aparición y sin
    /// duplicados. `$` seguido de `[a-z0-9_-]+` (case-insensitive) — el
    /// trigger explícito de la v1. Solo los nombres; ver
    /// [`Self::mentions_with_args`] para los argumentos.
    pub fn explicit_mentions(&self, input: &str) -> Vec<String> {
        self.mentions_with_args(input)
            .into_iter()
            .map(|m| m.name)
            .collect()
    }

    /// Menciones con sus argumentos (perfil operador, 2026-09-28 — paridad
    /// con la invocación de skills de Claude Code):
    ///
    /// - `/nombre args…` al COMIENZO del input (tras whitespace): alias de
    ///   `$nombre`; los argumentos son todo el resto del input.
    /// - `$nombre args…` en cualquier parte: los argumentos son el resto
    ///   de ESA línea (hasta el salto de línea), así varias menciones en
    ///   líneas distintas no se pisan.
    ///
    /// Solo resuelven nombres del registry: `/home/x` sin skill `home` no
    /// es una mención. Orden de aparición, sin duplicados (gana la
    /// primera aparición, con sus args).
    pub fn mentions_with_args(&self, input: &str) -> Vec<Mention> {
        let mut mentions: Vec<Mention> = Vec::new();
        let mut push = |name: String, args: &str| {
            if self.find(&name).is_some() && !mentions.iter().any(|m| m.name == name) {
                mentions.push(Mention {
                    name,
                    args: args.trim().to_string(),
                });
            }
        };
        // `/nombre` solo al comienzo: el alias de slash command.
        let leading = input.trim_start();
        if let Some(rest) = leading.strip_prefix('/') {
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
                .unwrap_or(rest.len());
            if end > 0 {
                push(normalize_name(&rest[..end]), &rest[end..]);
            }
        }
        let bytes = input.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'$' {
                let start = i + 1;
                let mut end = start;
                while end < bytes.len()
                    && (bytes[end].is_ascii_alphanumeric()
                        || bytes[end] == b'-'
                        || bytes[end] == b'_')
                {
                    end += 1;
                }
                if end > start {
                    let line_end = input[end..]
                        .find('\n')
                        .map(|n| end + n)
                        .unwrap_or(input.len());
                    push(normalize_name(&input[start..end]), &input[end..line_end]);
                }
                i = end;
            } else {
                i += 1;
            }
        }
        mentions
    }
}

/// Placeholder de argumentos en el body de una skill (contrato de Claude
/// Code): se sustituye por el texto que siguió a la mención.
pub const ARGUMENTS_PLACEHOLDER: &str = "$ARGUMENTS";

/// Una mención resuelta con sus argumentos — ver
/// [`SkillRegistry::mentions_with_args`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mention {
    pub name: String,
    /// Texto que siguió a la mención, ya `trim()`eado; vacío si nada.
    pub args: String,
}

/// Un body ya leído y capado, listo para inyectarse como addendum.
#[derive(Debug, Clone)]
pub struct LoadedSkill {
    pub name: String,
    pub body: String,
    pub truncated: bool,
    pub estimated_tokens: u32,
}

impl LoadedSkill {
    /// El addendum que entra al system prompt — encabezado + guía de uso
    /// acotado, per el formato del estudio.
    pub fn prompt_addendum(&self) -> String {
        format!(
            "\n\nLoaded skill: {}\nUse this procedural guidance only where relevant to the \
             current task.\n{}",
            self.name, self.body
        )
    }
}

fn normalize_name(raw: &str) -> String {
    raw.trim().to_lowercase().replace(' ', "-")
}

/// Recorre `dir` hasta [`MAX_DISCOVERY_DEPTH`] juntando cada `SKILL.md`
/// con su profundidad. Salta los directorios ocultos (`.trash/`, `.git/`):
/// una skill "borrada" que Claude Code movió a `.trash/` no es una skill.
fn collect_skill_files(dir: &Path, depth: usize, out: &mut Vec<(usize, PathBuf)>) {
    if depth > MAX_DISCOVERY_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let hidden = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'));
            if !hidden {
                collect_skill_files(&path, depth + 1, out);
            }
        } else if path.file_name().is_some_and(|n| n == "SKILL.md") {
            out.push((depth, path));
        }
    }
}

/// Parser mínimo del frontmatter: bloque `---`...`---` inicial con
/// líneas `clave: valor`; exige `name` y `description` no vacíos.
fn parse_skill_file(path: &Path) -> Option<SkillStub> {
    let raw = std::fs::read_to_string(path).ok()?;
    if raw.len() > MAX_BODY_BYTES {
        tracing::warn!(path = ?path, bytes = raw.len(), "SKILL.md over the size cap; skipped");
        return None;
    }
    let (name, description, tools) = parse_frontmatter(&raw)?;
    let body = skill_body(&raw);
    Some(SkillStub {
        name: normalize_name(&name),
        description,
        path: path.to_path_buf(),
        estimated_tokens: (body.len() / 4) as u32,
        tools,
    })
}

fn parse_frontmatter(raw: &str) -> Option<(String, String, Vec<String>)> {
    let rest = raw.strip_prefix("---")?;
    let end = rest.find("\n---")?;
    let block = &rest[..end];
    let mut name = None;
    let mut description = None;
    let mut tools = Vec::new();
    for line in block.lines() {
        if let Some(value) = line.strip_prefix("name:") {
            name = Some(value.trim().to_string()).filter(|v| !v.is_empty());
        } else if let Some(value) = line.strip_prefix("description:") {
            description = Some(value.trim().to_string()).filter(|v| !v.is_empty());
        } else if let Some(value) = line.strip_prefix("tools:") {
            // Lista separada por comas. Opcional: su ausencia deja la
            // skill como explicit-only, que es el default D′.
            tools = value
                .trim()
                .trim_start_matches('[')
                .trim_end_matches(']')
                .split(',')
                .map(|t| t.trim().trim_matches(['"', '\'']).to_lowercase())
                .filter(|t| !t.is_empty())
                .collect();
        }
    }
    Some((name?, description?, tools))
}

/// El markdown después del frontmatter (o el archivo entero si no hay
/// frontmatter — ese caso ya fue rechazado en el parse del stub).
fn skill_body(raw: &str) -> &str {
    if let Some(rest) = raw.strip_prefix("---")
        && let Some(end) = rest.find("\n---")
    {
        let after = &rest[end + "\n---".len()..];
        return after.trim_start_matches(['-']).trim_start();
    }
    raw.trim_start()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// El frontmatter `tools:` es lo que habilita la invocación
    /// call-time: sin él la skill sigue siendo explicit-only (D′), que es
    /// el default y el brazo de control del A/B.
    #[test]
    fn tools_frontmatter_maps_a_skill_to_its_tools() {
        let dir = temp_skills_dir("tools-frontmatter");
        let skill = dir.join("editing");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: editing\ndescription: d\ntools: edit_file, Write_File\n---\n\nbody",
        )
        .unwrap();
        let plain = dir.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::write(
            plain.join("SKILL.md"),
            "---\nname: plain\ndescription: d\n---\n\nbody",
        )
        .unwrap();

        let registry = SkillRegistry::discover(std::slice::from_ref(&dir));
        // Case-insensitive en ambos lados de la comparación.
        assert_eq!(registry.for_tool("edit_file").map(|s| s.name.as_str()), Some("editing"));
        assert_eq!(registry.for_tool("write_file").map(|s| s.name.as_str()), Some("editing"));
        assert_eq!(registry.for_tool("EDIT_FILE").map(|s| s.name.as_str()), Some("editing"));
        // Una tool sin skill declarada no matchea nada...
        assert!(registry.for_tool("shell_exec").is_none());
        // ...y una skill sin `tools:` no es candidata de ninguna.
        assert!(registry.find("plain").is_some_and(|s| s.tools.is_empty()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn temp_skills_dir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("braze-skills-test-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_skill(root: &Path, folder: &str, name: &str, description: &str, body: &str) {
        let dir = root.join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}"),
        )
        .unwrap();
    }

    /// Discovery finds nested SKILL.md files, parses their frontmatter,
    /// and rejects invalid ones without failing the rest.
    #[test]
    fn discovery_indexes_valid_skills_and_skips_invalid_ones() {
        let root = temp_skills_dir("discovery");
        write_skill(
            &root,
            "testing",
            "Testing",
            "how to write tests here",
            "Run cargo test.",
        );
        write_skill(
            &root,
            "review",
            "review",
            "review checklist",
            "Check invariants.",
        );
        std::fs::write(root.join("broken.md"), "not a skill").unwrap();
        std::fs::create_dir_all(root.join("no-front")).unwrap();
        std::fs::write(root.join("no-front/SKILL.md"), "body sin frontmatter").unwrap();

        let registry = SkillRegistry::discover(std::slice::from_ref(&root));
        assert_eq!(registry.stubs().len(), 2);
        // Names normalize: "Testing" → "testing".
        assert!(registry.find("testing").is_some());
        assert!(registry.find("TESTING").is_some(), "lookup normalizes too");
        assert!(registry.find("review").is_some());

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Duplicate names: the earlier config path wins, deterministically.
    #[test]
    fn duplicate_names_resolve_by_path_priority() {
        let root_a = temp_skills_dir("dup-a");
        let root_b = temp_skills_dir("dup-b");
        write_skill(&root_a, "s", "shared", "from A", "body A");
        write_skill(&root_b, "s", "shared", "from B", "body B");

        let registry = SkillRegistry::discover(&[root_a.clone(), root_b.clone()]);
        assert_eq!(registry.stubs().len(), 1);
        assert_eq!(registry.find("shared").unwrap().description, "from A");

        let _ = std::fs::remove_dir_all(&root_a);
        let _ = std::fs::remove_dir_all(&root_b);
    }

    /// Body loading caps at max_body_tokens and flags the truncation.
    #[test]
    fn load_body_caps_and_flags_truncation() {
        let root = temp_skills_dir("cap");
        write_skill(&root, "big", "big", "a big skill", &"x".repeat(10_000));

        let registry = SkillRegistry::discover(std::slice::from_ref(&root));
        let loaded = registry.load_body("big", 100).expect("skill exists");
        assert!(loaded.truncated);
        assert!(
            loaded.body.len() < 1_000,
            "capped near 100 tokens ≈ 400 chars"
        );
        assert!(loaded.body.contains("[skill body truncated"));
        assert_eq!(loaded.estimated_tokens, 100);

        let small = registry.load_body("big", 100_000).expect("skill exists");
        assert!(!small.truncated);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// `$mention` extraction: resolves against the registry, in order,
    /// deduplicated; unknown names and bare `$` are ignored.
    #[test]
    fn explicit_mentions_resolve_in_order_without_duplicates() {
        let root = temp_skills_dir("mentions");
        write_skill(&root, "t", "testing", "d", "b");
        write_skill(&root, "r", "review", "d", "b");
        let registry = SkillRegistry::discover(std::slice::from_ref(&root));

        let mentions = registry.explicit_mentions(
            "usa $review y $testing, de nuevo $review; $desconocida y $ solo no",
        );
        assert_eq!(mentions, vec!["review".to_string(), "testing".to_string()]);
        assert!(registry.explicit_mentions("sin menciones").is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Perfil operador: los directorios ocultos no se recorren (`.trash/`
    /// de Claude Code) y, ante nombres duplicados dentro de un mismo root,
    /// gana el archivo más superficial (`<n>/SKILL.md` sobre
    /// `synced/<uuid>/<n>/SKILL.md`).
    #[test]
    fn discovery_skips_hidden_dirs_and_prefers_the_shallower_duplicate() {
        let root = temp_skills_dir("hidden-shallow");
        write_skill(&root, "memo", "memo", "la buena", "body");
        write_skill(&root, "synced/uuid-1/memo", "memo", "copia sincronizada", "body");
        write_skill(&root, ".trash/123/old", "old", "borrada", "body");
        let registry = SkillRegistry::discover(std::slice::from_ref(&root));
        assert_eq!(registry.stubs().len(), 1, "{:?}", registry.stubs());
        assert_eq!(registry.find("memo").unwrap().description, "la buena");
        assert!(registry.find("old").is_none(), ".trash/ no se recorre");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Paridad con Claude Code: `/nombre args` al comienzo es alias de
    /// `$nombre`, los args de `$nombre` son el resto de su línea, y
    /// `$ARGUMENTS` en el body se sustituye antes de capar.
    #[test]
    fn slash_alias_arguments_and_placeholder_substitution() {
        let root = temp_skills_dir("args");
        write_skill(&root, "d", "destelegrafiar", "d", "Modo: $ARGUMENTS\nfin");
        write_skill(&root, "r", "review", "d", "sin placeholder");
        let registry = SkillRegistry::discover(std::slice::from_ref(&root));

        let m = registry.mentions_with_args("  /destelegrafiar check borrador.md\nsegunda línea");
        assert_eq!(
            m,
            vec![Mention {
                name: "destelegrafiar".into(),
                args: "check borrador.md\nsegunda línea".into()
            }]
        );
        let m = registry.mentions_with_args("usa $review a fondo\ny $destelegrafiar fix x.md");
        assert_eq!(m[0], Mention { name: "review".into(), args: "a fondo".into() });
        assert_eq!(m[1], Mention { name: "destelegrafiar".into(), args: "fix x.md".into() });
        assert!(registry.mentions_with_args("/home/x no es skill").is_empty());
        assert!(registry.mentions_with_args("ruta /review/x").is_empty(), "solo al comienzo");
        assert_eq!(registry.explicit_mentions("$review $review"), vec!["review".to_string()]);

        let loaded = registry.load_body_with_args("destelegrafiar", 1000, "check borrador.md").unwrap();
        assert_eq!(loaded.body, "Modo: check borrador.md\nfin");
        assert_eq!(registry.load_body("destelegrafiar", 1000).unwrap().body, "Modo: \nfin");
        assert_eq!(registry.load_body_with_args("review", 1000, "algo").unwrap().body, "sin placeholder");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The addendum carries the header, the scoping instruction, and the
    /// body — the exact shape the study specified.
    #[test]
    fn the_addendum_has_the_studied_shape() {
        let loaded = LoadedSkill {
            name: "testing".to_string(),
            body: "Run cargo test first.".to_string(),
            truncated: false,
            estimated_tokens: 6,
        };
        let addendum = loaded.prompt_addendum();
        assert!(addendum.contains("Loaded skill: testing"));
        assert!(addendum.contains("only where relevant"));
        assert!(addendum.contains("Run cargo test first."));
    }
}
