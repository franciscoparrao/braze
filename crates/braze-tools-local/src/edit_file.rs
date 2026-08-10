//! `edit_file` tool: replaces exactly one occurrence of `old_string` with
//! `new_string` in a file. Fails if `old_string` doesn't appear, or
//! appears more than once — same disambiguation principle as Claude
//! Code's Edit tool: an ambiguous edit is refused rather than guessed at.
//! Guarded — treated as a write for permission purposes (there is no
//! separate `ActionDescriptor::EditFile` variant).
//!
//! ## Fuzzy application (docs/SOTA-2026-07.md, adenda Aider)
//!
//! Small models (3-7B, braze's executor target) frequently reproduce the
//! text they intend to replace with small whitespace deviations —
//! trailing spaces dropped, indentation re-emitted at a different depth.
//! Aider measured 9× fewer apply failures from tolerating exactly that
//! class of deviation. So matching runs as a ladder, strictest first,
//! each rung still requiring an *unambiguous* (exactly-one) match:
//!
//! 1. exact substring (unchanged original behavior — always wins);
//! 2. line-window match ignoring *trailing* whitespace per line;
//! 3. line-window match ignoring *leading and trailing* whitespace,
//!    with `new_string` re-indented by the offset observed between the
//!    file's first matched line and `old_string`'s first line — the
//!    file's real indentation wins, not the model's;
//! 4. `old_string` es la región del archivo **menos caracteres que el
//!    modelo no puede emitir** (v9 roadmap técnica #3, medido en
//!    `docs/roam-metrics-memoria-2026-07-28.md` § 7). Solo borrados,
//!    solo no-ASCII, match único y acotado — ver
//!    [`unemittable_deletion_window`] para el certificado completo.
//!
//! Rungs 2-3 are line-window matches: `old_string`'s lines must
//! correspond to whole lines of the file (the observed failure mode is
//! "right lines, wrong whitespace", not partial-line fragments).
//!
//! ## Strict mode (E1, docs/AUDITORIA-2026-07-v3.md)
//!
//! `edit_file`'s `strict` parameter disables rungs 2-3 entirely (only
//! exact matching survives) — not a production default, but the knob
//! `braze-bench`'s `+ablate:strict-edit` needs to actually *measure*
//! whether Aider's fuzzy-matching ladder helps a given model/task, per
//! the SOTA-2026-07 finding that the edit format is a hidden variable for
//! small models. Threaded from `LocalToolsProvider::with_edit_strict_mode`.

use std::path::PathBuf;

use serde::Deserialize;

/// Arguments as they arrive in `ToolCall.arguments`:
/// `{"path": "src/lib.rs", "old_string": "foo", "new_string": "bar"}`.
#[derive(Debug, Deserialize)]
pub struct EditFileArgs {
    pub path: String,
    pub old_string: String,
    pub new_string: String,
}

/// Above this many lines, a whole-file rewrite stops being a cheap
/// fallback and becomes a re-transcription of everything the model was
/// *not* asked to touch. A judgement call, not a measured threshold: the
/// file that produced the evidence below was ~330 lines, and Aider's
/// whole-file evidence (module doc comment) is drawn from small files.
const WHOLE_FILE_REWRITE_LINE_LIMIT: usize = 120;

/// Steering appended to matching failures, chosen by file size.
///
/// For small files the whole-file path is the empirically better edit
/// surface for small models (Aider's leaderboard assigns whole-file to
/// every small model; see the module doc comment), so a model that can't
/// reproduce the exact text gets pointed there instead of retrying the
/// same failing shape.
///
/// For large files it is a trap, and we measured it (roam, 2026-07-28,
/// `docs/roam-metrics-memoria-2026-07-28.md`): after `edit_file` failed,
/// gpt-oss:20b quoted this very sentence to justify rewriting a 268-line
/// file, and the rewrite silently converted two tolerance-based float
/// assertions into exact equality, deleted `U+2248` from three comments,
/// and dropped the edit it had actually been asked to make. Only the
/// last of those is loud; reconstructing the file from the session log
/// confirms the other two pass the project's suite. The lines a
/// whole-file rewrite damages are the ones nobody is reviewing.
fn write_file_steering(original: &str) -> String {
    let lines = original.lines().count();
    if lines <= WHOLE_FILE_REWRITE_LINE_LIMIT {
        "If you cannot reproduce the exact current text, use write_file with the \
         complete updated file content instead."
            .to_string()
    } else {
        format!(
            "Do NOT work around this by rewriting the whole file: at {lines} lines, \
             write_file would re-type everything you were not asked to touch, and \
             transcription errors in those untouched lines are caught by neither the \
             compiler nor the tests. Correct old_string, or report that you cannot \
             reproduce it and stop."
        )
    }
}

/// `Ok(summary)` on success. `Err(message)` covers I/O failures and the
/// disambiguation failures (`old_string` missing / ambiguous) — all
/// recoverable tool-level failures, see `provider.rs::wrap`. `strict`
/// disables the fuzzy rungs (2-3) of the matching ladder — see the module
/// doc comment's "Strict mode" section.
pub async fn edit_file(args: EditFileArgs, strict: bool, gate: bool) -> Result<String, String> {
    if args.old_string.is_empty() {
        return Err("old_string must not be empty".to_string());
    }

    let path = PathBuf::from(&args.path);
    let original = match tokio::fs::read_to_string(&path).await {
        Ok(contents) => contents,
        // A model reaching for edit_file to CREATE a file is a predictable
        // mistake (the tool name reads as "change a file"); a bare OS error
        // ("No such file or directory") gives it nothing to act on, unlike
        // the matching-failure messages below, which all steer somewhere.
        // See docs/AUDITORIA-2026-07-v3.md, hallazgo A4.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "'{}' does not exist. To create a new file, use write_file with its full content.",
                path.display()
            ));
        }
        Err(err) => return Err(format!("failed to read '{}': {err}", path.display())),
    };

    if let Some(marker) = elision_marker(&args.old_string, &args.new_string) {
        return Err(format!(
            "new_string looks abbreviated: it contains {marker} while being shorter than \
             old_string. `edit_file` writes new_string VERBATIM — a placeholder like that \
             would be written into '{}' as literal text, silently corrupting the file. \
             Send the complete replacement text, or use write_file with the file's full \
             updated content.",
            path.display()
        ));
    }

    let (updated, strategy) = apply_edit(&original, &args.old_string, &args.new_string, strict)
        .map_err(|kind| kind.into_message(&path, &original, &args.old_string))?;

    // Gate sintáctico pre-aplicación (survey 2026-08-10): si la edición
    // rompería la sintaxis de un `.rs` que sí parseaba, se rechaza SIN
    // escribir — el archivo queda válido. Ver `crate::syntactic_gate`.
    if gate {
        crate::syntactic_gate::check_rust_edit(&path, Some(&original), &updated, "new_string")?;
    }

    tokio::fs::write(&path, updated.as_bytes())
        .await
        .map_err(|err| format!("failed to write '{}': {err}", path.display()))?;

    Ok(match strategy {
        MatchStrategy::Exact => format!("edited {}", path.display()),
        MatchStrategy::TrailingWhitespace => format!(
            "edited {} (matched ignoring trailing whitespace)",
            path.display()
        ),
        MatchStrategy::RelativeIndentation => format!(
            "edited {} (matched ignoring indentation; the file's real indentation was preserved)",
            path.display()
        ),
        MatchStrategy::UnemittableDeletion(n) => format!(
            "edited {} — WARNING: your old_string was missing {n} character(s) that ARE in the \
             file. The edit was located by treating them as characters you cannot emit, and the \
             match was unique, so it was applied to the file's real text. If your new_string is \
             a copy of old_string, it is missing those characters too and they have now been \
             DELETED from the file. Read the region back before continuing.",
            path.display()
        ),
    })
}

/// Incidente roam #12 (2026-07-20): un modelo que abrevia el reemplazo
/// ("lazy diff") manda un `new_string` con una elisión — la forma
/// observada fue literalmente `"…fn test_mcp_square_with_interior_point()
/// {\n..."`. `edit_file` escribe `new_string` VERBATIM, así que esa
/// llamada habría dejado `...` como texto dentro de `lib.rs` y borrado
/// el resto del bloque. Solo se salvó porque `old_string` no matcheó:
/// una corrupción silenciosa a un fallo de suerte de distancia.
///
/// La heurística exige DOS señales para no castigar código legítimo
/// (`...` es sintaxis válida en Python, y un comentario puede
/// mencionarlo): un marcador de elisión Y un `new_string` más corto que
/// el `old_string` que reemplaza. Abreviar es, por definición, acortar.
/// Un falso positivo no bloquea al modelo: el mensaje lo manda a
/// `write_file`, que es la salida correcta de todos modos.
fn elision_marker(old_string: &str, new_string: &str) -> Option<&'static str> {
    if new_string.len() >= old_string.len() {
        return None;
    }
    for line in new_string.lines() {
        let t = line.trim();
        if t == "..." || t == "…" {
            return Some("a bare `...` line");
        }
        let comment = t
            .trim_start_matches(['/', '#', '-', '<', '!', '*', ';', '%'])
            .trim_start_matches("--")
            .trim();
        if t != comment
            && (comment.starts_with("...")
                || comment.starts_with('…')
                || comment.contains("rest of the")
                || comment.contains("unchanged")
                || comment.contains("same as before")
                || comment.contains("existing code"))
        {
            return Some("an elision comment");
        }
    }
    None
}

/// Which rung of the matching ladder produced the edit — surfaced in the
/// success summary so session logs (and the bench) can tell exact edits
/// apart from fuzzily-applied ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchStrategy {
    Exact,
    TrailingWhitespace,
    RelativeIndentation,
    /// Rung 4 — see [`unemittable_deletion_window`]. Carries how many
    /// characters the model's `old_string` was missing, for the summary.
    UnemittableDeletion(usize),
}

/// Cuántos caracteres puede faltarle a `old_string` antes de que el
/// peldaño 4 se niegue. El caso real (roam, fórmula del KDE) tenía 3
/// (`Σᵢ`, `xᵢ`, `yᵢ`); 8 deja margen para un bloque con varias
/// ocurrencias sin volverse un comodín.
const MAX_UNEMITTABLE_DELETIONS: usize = 8;

/// Largo mínimo de `old_string` para que el peldaño 4 se active. Un
/// fragmento corto que resulta ser subsecuencia de una región del
/// archivo es coincidencia plausible; 40 caracteres no lo son.
const MIN_UNEMITTABLE_OLD_STRING: usize = 40;

/// Peldaño 4 de la escalera: recupera la edición cuando `old_string` es
/// exactamente la región del archivo **menos** caracteres que el modelo
/// no puede emitir.
///
/// Por qué existe (medido, no supuesto —
/// `docs/roam-metrics-memoria-2026-07-28.md` § 7): `gpt-oss:20b` no
/// puede escribir `U+1D62`. Produjo un `old_string` de 91 líneas
/// correcto en todas menos una, y ninguna cantidad de reintentos lo
/// arregla: el diagnóstico de `first_divergence` le nombra el codepoint
/// y aun así vuelve a omitirlo. Esa región del archivo queda
/// **estructuralmente ineditable** por el agente. Este peldaño la
/// devuelve al alcance sin pedirle al modelo lo que no puede dar.
///
/// El certificado que lo hace seguro tiene cuatro partes, y basta que
/// una falle para negarse:
///
/// 1. **Solo borrados.** `old_string` debe ser subsecuencia de la región
///    del archivo: nada de sustituciones, inserciones ni reordenamientos
///    — el `format!` corrupto del mismo incidente (comilla movida) NO
///    califica, y no debe: mover una comilla puede ser intención.
/// 2. **Solo no-ASCII.** Cada carácter ausente debe ser no-ASCII. Un
///    `)` o un `;` que falta es semántico; un `ᵢ` que falta es motor.
/// 3. **Unicidad.** Si dos regiones del archivo admiten el mismo
///    alineamiento, se rechaza por ambiguo — igual que los peldaños 2-3.
/// 4. **Acotado.** Máximo [`MAX_UNEMITTABLE_DELETIONS`] borrados, y
///    `old_string` de al menos [`MIN_UNEMITTABLE_OLD_STRING`] caracteres.
///
/// Direccionalidad, que es la otra mitad de la seguridad: el peldaño
/// solo corrige **dónde** matchea, nunca **con qué** se reemplaza. Si el
/// modelo quisiera borrar esos caracteres, los pondría en `old_string`
/// (copiando el archivo) y los omitiría en `new_string` — la dirección
/// contraria a la que este peldaño acepta.
///
/// `Ok(None)` = no aplica. `Err(count)` = ambiguo.
fn unemittable_deletion_window(
    original: &str,
    old_string: &str,
) -> Result<Option<(usize, usize, Vec<char>)>, usize> {
    if old_string.len() < MIN_UNEMITTABLE_OLD_STRING {
        return Ok(None);
    }
    let mut hits: Vec<(usize, usize, Vec<char>)> = Vec::new();
    let starts = std::iter::once(0).chain(original.match_indices('\n').map(|(i, _)| i + 1));
    for start in starts {
        if let Some((end, dropped)) = greedy_deletion_match(&original[start..], old_string) {
            hits.push((start, start + end, dropped));
            if hits.len() > 1 {
                return Err(hits.len());
            }
        }
    }
    Ok(hits.pop())
}

/// Intenta consumir `old_string` desde el inicio de `haystack`
/// permitiendo saltar caracteres no-ASCII del archivo. Determinístico:
/// solo salta cuando el carácter del archivo difiere del esperado, así
/// que no hay ramificación que explorar. Devuelve el offset (en bytes,
/// relativo a `haystack`) donde termina la región y los caracteres
/// saltados.
fn greedy_deletion_match(haystack: &str, old_string: &str) -> Option<(usize, Vec<char>)> {
    let mut dropped = Vec::new();
    let mut hay = haystack.char_indices().peekable();
    let mut consumed_end = 0usize;
    for want in old_string.chars() {
        loop {
            let (idx, got) = *hay.peek()?;
            if got == want {
                hay.next();
                consumed_end = idx + got.len_utf8();
                break;
            }
            // Solo un no-ASCII puede estar ausente del old_string.
            if got.is_ascii() || dropped.len() >= MAX_UNEMITTABLE_DELETIONS {
                return None;
            }
            dropped.push(got);
            hay.next();
        }
    }
    // Un match sin ningún borrado ya lo habría tomado el peldaño 1.
    if dropped.is_empty() {
        return None;
    }
    Some((consumed_end, dropped))
}

/// Why no rung of the ladder could apply the edit.
enum MatchFailure {
    NotFound,
    /// Occurrence count and the rung that found the ambiguity —
    /// ambiguity at ANY rung refuses the edit rather than guessing.
    Ambiguous(usize, MatchStrategy),
}

impl MatchFailure {
    fn into_message(self, path: &std::path::Path, original: &str, old_string: &str) -> String {
        match self {
            MatchFailure::NotFound => {
                // `first_divergence` supersedes the closest-line hint when it
                // fires: it names the exact character, which the hint cannot
                // do once `old_string`'s opening lines are correct.
                let hint = first_divergence(original, old_string)
                    .map(|d| format!(" {d}"))
                    .or_else(|| {
                        find_closest_line(original, old_string).map(|(line_no, line)| {
                            format!(
                                " The closest match in the file is line {line_no}: `{}`.",
                                line.trim()
                            )
                        })
                    })
                    .unwrap_or_default();
                format!(
                    "old_string not found in '{}' (also tried whitespace-tolerant matching).\
                     {hint} {}",
                    path.display(),
                    write_file_steering(original)
                )
            }
            MatchFailure::Ambiguous(count, strategy) => format!(
                "old_string is ambiguous in '{}': found {count} occurrences{}, expected \
                 exactly 1. Include more surrounding context in old_string to disambiguate. \
                 {}",
                path.display(),
                match strategy {
                    MatchStrategy::Exact => "",
                    MatchStrategy::UnemittableDeletion(_) => {
                        " (treating characters absent from old_string as unemittable)"
                    }
                    _ => " (under whitespace-tolerant matching)",
                },
                write_file_steering(original)
            ),
        }
    }
}

/// Best-effort "did you mean" hint for a failed match: the file line with
/// the most words in common with `old_string`'s first non-blank line.
/// `None` when nothing shares even one word with it — a small model gets
/// no material to correct with a plain "not found"; this gives it a real
/// anchor line to compare against instead (docs/AUDITORIA-2026-07-v3.md,
/// hallazgo A3).
fn find_closest_line<'a>(original: &'a str, old_string: &str) -> Option<(usize, &'a str)> {
    let needle = old_string.lines().find(|l| !l.trim().is_empty())?.trim();
    let needle_words: std::collections::HashSet<&str> = needle
        .split_whitespace()
        .filter(|w| !w.is_empty())
        .collect();
    if needle_words.is_empty() {
        return None;
    }

    original
        .lines()
        .enumerate()
        .map(|(i, line)| {
            let overlap = line
                .split_whitespace()
                .filter(|w| needle_words.contains(w))
                .count();
            (i + 1, line, overlap)
        })
        .filter(|&(_, _, overlap)| overlap > 0)
        .max_by_key(|&(_, _, overlap)| overlap)
        .map(|(line_no, line, _)| (line_no, line))
}

/// Minimum aligned prefix before a divergence report is worth making —
/// below this, the "alignment" is coincidence and [`find_closest_line`]
/// is the better hint.
const MIN_ALIGNED_PREFIX_BYTES: usize = 16;

/// Names the first character where `old_string` diverges from the file,
/// with the codepoint on each side.
///
/// Why this exists, and why [`find_closest_line`] was not enough: that
/// hint anchors on `old_string`'s FIRST line, so it says nothing when
/// that line is correct and the divergence is dozens of lines in.
/// Measured against roam (2026-07-28,
/// `docs/roam-metrics-memoria-2026-07-28.md`): gpt-oss:20b cannot emit
/// `U+1D62`. Asked to delete a block whose exact text was supplied
/// verbatim in the prompt, it produced a 91-line `old_string` correct on
/// every line but the one carrying that character, then spent six
/// attempts, the turn's full 20-round budget and 25 minutes re-issuing
/// the same unmatchable call — grepping and re-reading the file without
/// ever recovering the cause, because a bare "not found" carries no
/// signal about WHICH character is wrong.
///
/// The failure class this addresses is not a typo but a capability gap:
/// a model can read and reason about a character it cannot reproduce,
/// and since `edit_file` matches on exact text, a region holding such a
/// character is otherwise *structurally uneditable* by the agent with
/// nothing in the loop able to discover why.
fn first_divergence(original: &str, old_string: &str) -> Option<String> {
    let (offset, common) = best_alignment(original, old_string)?;
    if common < MIN_ALIGNED_PREFIX_BYTES {
        return None;
    }
    let file_char = original[offset + common..].chars().next();
    let old_char = old_string[common..].chars().next();
    if file_char.is_none() && old_char.is_none() {
        return None; // identical — cannot happen on a NotFound, but stay total.
    }

    let matched = &old_string[..common];
    let old_line_no = matched.matches('\n').count() + 1;
    let column = matched.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    let file_line_no = original[..offset + common].matches('\n').count() + 1;

    Some(format!(
        "First difference: line {old_line_no}, column {column} of old_string \
         (line {file_line_no} of the file) — the file has {} where old_string has {}.\n\
         \x20 file:       {}\n\
         \x20 old_string: {}\n\
         Copy that line from the file exactly. If you cannot reproduce that character, \
         say so and stop rather than editing around it.",
        describe_char(file_char),
        describe_char(old_char),
        excerpt(line_at(original, offset + common)),
        excerpt(line_at(old_string, common)),
    ))
}

/// The line-start offset in `original` whose following text shares the
/// longest prefix with `old_string`, plus that prefix's byte length.
/// Line starts only: an edit that fails to match still almost always
/// begins at a line boundary, and it bounds the scan.
fn best_alignment(original: &str, old_string: &str) -> Option<(usize, usize)> {
    std::iter::once(0)
        .chain(original.match_indices('\n').map(|(i, _)| i + 1))
        .map(|offset| (offset, common_prefix_bytes(&original[offset..], old_string)))
        .filter(|&(_, common)| common > 0)
        .max_by_key(|&(_, common)| common)
}

/// Byte length of the longest common prefix, counted on char boundaries
/// so multi-byte characters are never split.
fn common_prefix_bytes(a: &str, b: &str) -> usize {
    a.char_indices()
        .zip(b.chars())
        .take_while(|((_, left), right)| left == right)
        .map(|((i, left), _)| i + left.len_utf8())
        .last()
        .unwrap_or(0)
}

/// The whole line containing byte offset `at` — the divergence is easier
/// to see against its own line than as a bare codepoint.
fn line_at(text: &str, at: usize) -> &str {
    let start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let end = text[at..].find('\n').map_or(text.len(), |i| at + i);
    &text[start..end]
}

/// Keeps a quoted line short enough not to crowd the message out of a
/// small model's context.
fn excerpt(line: &str) -> String {
    const MAX: usize = 160;
    let trimmed = line.trim_end();
    if trimmed.chars().count() <= MAX {
        return trimmed.to_string();
    }
    let cut: String = trimmed.chars().take(MAX).collect();
    format!("{cut}…")
}

/// Renders a character as a codepoint plus a readable form — a bare
/// glyph is useless here, since the whole point is that the two sides
/// may look identical in a terminal.
fn describe_char(c: Option<char>) -> String {
    match c {
        None => "end of text".to_string(),
        Some('\n') => "U+000A (newline)".to_string(),
        Some('\t') => "U+0009 (tab)".to_string(),
        Some(' ') => "U+0020 (space)".to_string(),
        Some(c) => format!("U+{:04X} ('{c}')", c as u32),
    }
}

/// Pure core of the tool: runs the matching ladder over `original` and
/// returns the updated content plus the strategy that matched. `strict`
/// stops after rung 1 (exact match only) — see the module doc comment's
/// "Strict mode" section.
fn apply_edit(
    original: &str,
    old_string: &str,
    new_string: &str,
    strict: bool,
) -> Result<(String, MatchStrategy), MatchFailure> {
    // Rung 1: exact substring — always takes precedence, strict or not.
    let exact = original.matches(old_string).count();
    if exact == 1 {
        return Ok((
            original.replacen(old_string, new_string, 1),
            MatchStrategy::Exact,
        ));
    }
    if exact > 1 {
        return Err(MatchFailure::Ambiguous(exact, MatchStrategy::Exact));
    }

    if strict {
        return Err(MatchFailure::NotFound);
    }

    // Rungs 2-3: line-window matching.
    for (strategy, line_eq) in [
        (
            MatchStrategy::TrailingWhitespace,
            (|a: &str, b: &str| a.trim_end() == b.trim_end()) as fn(&str, &str) -> bool,
        ),
        (MatchStrategy::RelativeIndentation, |a: &str, b: &str| {
            a.trim() == b.trim()
        }),
    ] {
        match find_line_window(original, old_string, line_eq) {
            Ok(Some(window_start)) => {
                return Ok((
                    replace_line_window(original, old_string, new_string, window_start, strategy),
                    strategy,
                ));
            }
            Ok(None) => {}
            Err(count) => return Err(MatchFailure::Ambiguous(count, strategy)),
        }
    }

    // Rung 4: `old_string` es la región del archivo menos caracteres que
    // el modelo no puede emitir. Va última porque es la más permisiva, y
    // solo corre cuando todo lo demás falló — no puede empeorar ningún
    // caso que hoy funcione. Ver `unemittable_deletion_window`.
    match unemittable_deletion_window(original, old_string) {
        Ok(Some((start, end, dropped))) => {
            let mut updated =
                String::with_capacity(original.len() - (end - start) + new_string.len());
            updated.push_str(&original[..start]);
            updated.push_str(new_string);
            updated.push_str(&original[end..]);
            return Ok((updated, MatchStrategy::UnemittableDeletion(dropped.len())));
        }
        Ok(None) => {}
        Err(count) => {
            return Err(MatchFailure::Ambiguous(
                count,
                MatchStrategy::UnemittableDeletion(0),
            ));
        }
    }

    Err(MatchFailure::NotFound)
}

/// Finds the unique window of whole file lines whose lines are pairwise
/// `line_eq`-equal to `old_string`'s lines. `Ok(Some(start))` for exactly
/// one match (start = line index), `Ok(None)` for zero, `Err(count)` for
/// ambiguity. Blank-only `old_string` windows are rejected (nothing to
/// anchor on once whitespace is ignored).
fn find_line_window(
    original: &str,
    old_string: &str,
    line_eq: fn(&str, &str) -> bool,
) -> Result<Option<usize>, usize> {
    let old_lines: Vec<&str> = old_string.lines().collect();
    if old_lines.is_empty() || old_lines.iter().all(|l| l.trim().is_empty()) {
        return Ok(None);
    }
    let file_lines: Vec<&str> = original.lines().collect();
    if old_lines.len() > file_lines.len() {
        return Ok(None);
    }

    let matches: Vec<usize> = (0..=file_lines.len() - old_lines.len())
        .filter(|&start| {
            old_lines
                .iter()
                .enumerate()
                .all(|(i, old)| line_eq(file_lines[start + i], old))
        })
        .collect();

    match matches.as_slice() {
        [] => Ok(None),
        [only] => Ok(Some(*only)),
        many => Err(many.len()),
    }
}

/// Rebuilds `original` with the matched line window replaced by
/// `new_string`'s lines. Under `RelativeIndentation`, every `new_string`
/// line is re-indented by the offset between the file's first matched
/// line and `old_string`'s first line, so the file's real indentation is
/// preserved even though the model emitted the block at another depth.
/// The original's trailing-newline presence is preserved.
fn replace_line_window(
    original: &str,
    old_string: &str,
    new_string: &str,
    window_start: usize,
    strategy: MatchStrategy,
) -> String {
    let file_lines: Vec<&str> = original.lines().collect();
    let window_len = old_string.lines().count();

    let new_lines: Vec<String> = match strategy {
        MatchStrategy::RelativeIndentation => {
            let file_indent = leading_whitespace(file_lines[window_start]);
            let old_indent = leading_whitespace(old_string.lines().next().unwrap_or_default());
            new_string
                .lines()
                .map(|line| reindent(line, old_indent, file_indent))
                .collect()
        }
        _ => new_string.lines().map(str::to_string).collect(),
    };

    let mut out_lines: Vec<String> = Vec::with_capacity(file_lines.len());
    out_lines.extend(file_lines[..window_start].iter().map(|l| l.to_string()));
    out_lines.extend(new_lines);
    out_lines.extend(
        file_lines[window_start + window_len..]
            .iter()
            .map(|l| l.to_string()),
    );

    let mut out = out_lines.join("\n");
    if original.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn leading_whitespace(line: &str) -> &str {
    &line[..line.len() - line.trim_start().len()]
}

/// Swaps `old_indent` for `file_indent` at the start of `line`, when
/// present — lines indented *deeper* than the block's first line keep
/// their extra depth relative to the new base.
fn reindent(line: &str, old_indent: &str, file_indent: &str) -> String {
    match line.strip_prefix(old_indent) {
        Some(rest) => format!("{file_indent}{rest}"),
        // The line is shallower than the block's first line (or uses
        // different whitespace characters) — keep it untouched rather
        // than guessing.
        None => line.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::unique_temp_dir;

    async fn fixture_file(dir: &std::path::Path, contents: &str) -> PathBuf {
        tokio::fs::create_dir_all(dir)
            .await
            .expect("create temp dir");
        let file_path = dir.join("fixture.txt");
        tokio::fs::write(&file_path, contents)
            .await
            .expect("write fixture file");
        file_path
    }

    /// Thin wrapper preserving every pre-existing test's call shape after
    /// `edit_file` grew its `strict` parameter (E1,
    /// docs/AUDITORIA-2026-07-v3.md) — `false` keeps the fuzzy ladder on,
    /// the behavior every test below except the dedicated strict-mode
    /// ones was written to exercise.
    async fn edit_file_fuzzy(args: EditFileArgs) -> Result<String, String> {
        edit_file(args, false, false).await
    }

    /// Incidente roam #12: la llamada exacta que gpt-oss:20b produjo
    /// contra roam. Si `old_string` hubiera matcheado, el `...` quedaba
    /// escrito dentro de `lib.rs` y el resto del bloque desaparecía.
    #[tokio::test]
    async fn an_abbreviated_new_string_is_rejected_before_it_corrupts_the_file() {
        let dir = unique_temp_dir("edit-file-elision");
        let original = "    #[test]\n    fn t() {\n        let a = 1;\n        let b = 2;\n    }\n";
        let file_path = fixture_file(&dir, original).await;

        let err = edit_file_fuzzy(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            old_string: original.to_string(),
            new_string: "    #[test]\n    fn t() {\n...".to_string(),
        })
        .await
        .expect_err("an abbreviated replacement must be rejected");

        assert!(err.contains("looks abbreviated"), "got: {err}");
        assert!(
            err.contains("write_file"),
            "the rejection must steer somewhere: {err}"
        );
        assert_eq!(
            tokio::fs::read_to_string(&file_path)
                .await
                .expect("read back"),
            original,
            "the file must be untouched by a rejected edit"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// La otra forma habitual del lazy diff: el comentario que promete
    /// que lo omitido sigue ahí.
    #[tokio::test]
    async fn an_elision_comment_is_rejected_too() {
        let dir = unique_temp_dir("edit-file-elision-comment");
        let original = "fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\nfn e() {}\n";
        let file_path = fixture_file(&dir, original).await;

        let err = edit_file_fuzzy(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            old_string: original.to_string(),
            new_string: "fn a() {}\n// ... rest of the functions unchanged\n".to_string(),
        })
        .await
        .expect_err("an elision comment must be rejected");
        assert!(err.contains("looks abbreviated"), "got: {err}");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// La guarda no puede castigar a un edit legítimo que además crezca
    /// — `...` es sintaxis válida en varios lenguajes y un comentario
    /// puede mencionarlo sin estar abreviando nada.
    #[tokio::test]
    async fn a_longer_replacement_mentioning_dots_is_allowed() {
        let dir = unique_temp_dir("edit-file-elision-fp");
        let file_path = fixture_file(&dir, "x = 1\n").await;

        let result = edit_file_fuzzy(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            old_string: "x = 1\n".to_string(),
            new_string: "def f():\n    ...\n\nx = 1\ny = 2\nz = 3\n".to_string(),
        })
        .await;

        assert!(
            result.is_ok(),
            "a replacement that GROWS is not an abbreviation: {result:?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn replaces_the_single_occurrence() {
        let dir = unique_temp_dir("edit-file-happy");
        let file_path = fixture_file(&dir, "hello world").await;

        let result = edit_file_fuzzy(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            old_string: "world".to_string(),
            new_string: "braze".to_string(),
        })
        .await;

        assert!(result.is_ok());
        let contents = tokio::fs::read_to_string(&file_path)
            .await
            .expect("read back");
        assert_eq!(contents, "hello braze");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn old_string_not_found_is_an_error() {
        let dir = unique_temp_dir("edit-file-not-found");
        let file_path = fixture_file(&dir, "hello world").await;

        let result = edit_file_fuzzy(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            old_string: "missing".to_string(),
            new_string: "x".to_string(),
        })
        .await;

        assert!(result.is_err());
        let contents = tokio::fs::read_to_string(&file_path)
            .await
            .expect("read back");
        assert_eq!(contents, "hello world", "file must be untouched");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn multiple_occurrences_is_an_ambiguity_error() {
        let dir = unique_temp_dir("edit-file-ambiguous");
        let file_path = fixture_file(&dir, "foo foo foo").await;

        let result = edit_file_fuzzy(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            old_string: "foo".to_string(),
            new_string: "bar".to_string(),
        })
        .await;

        assert!(result.is_err());
        let contents = tokio::fs::read_to_string(&file_path)
            .await
            .expect("read back");
        assert_eq!(contents, "foo foo foo", "file must be untouched");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // --- fuzzy application ladder (docs/SOTA-2026-07.md, adenda Aider) ---

    /// The file has trailing whitespace the model didn't reproduce —
    /// rung 2 (trailing-whitespace-insensitive) must apply the edit.
    #[tokio::test]
    async fn trailing_whitespace_difference_still_matches() {
        let dir = unique_temp_dir("edit-file-fuzzy-trailing");
        let file_path = fixture_file(&dir, "fn main() {   \n    hola();\n}\n").await;

        let result = edit_file_fuzzy(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            // Note: no trailing spaces after "{", unlike the file.
            old_string: "fn main() {\n    hola();".to_string(),
            new_string: "fn main() {\n    chao();".to_string(),
        })
        .await
        .expect("fuzzy match should apply the edit");
        assert!(result.contains("trailing whitespace"), "got: {result}");

        let contents = tokio::fs::read_to_string(&file_path)
            .await
            .expect("read back");
        assert_eq!(contents, "fn main() {\n    chao();\n}\n");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// The model re-emitted the block at a different indentation depth —
    /// rung 3 must match AND preserve the file's real indentation, both
    /// for same-depth lines and deeper continuation lines.
    #[tokio::test]
    async fn indentation_difference_matches_and_preserves_the_files_indentation() {
        let dir = unique_temp_dir("edit-file-fuzzy-indent");
        let original = "mod x {\n        fn f() {\n            uno();\n        }\n}\n";
        let file_path = fixture_file(&dir, original).await;

        let result = edit_file_fuzzy(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            // The model emitted the block with 4-space base indentation;
            // the file actually uses 8.
            old_string: "    fn f() {\n        uno();\n    }".to_string(),
            new_string: "    fn f() {\n        dos();\n    }".to_string(),
        })
        .await
        .expect("indentation-relative match should apply the edit");
        assert!(result.contains("indentation"), "got: {result}");

        let contents = tokio::fs::read_to_string(&file_path)
            .await
            .expect("read back");
        assert_eq!(
            contents, "mod x {\n        fn f() {\n            dos();\n        }\n}\n",
            "the file's 8-space indentation must win over the model's 4"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// An exact match elsewhere always beats any fuzzy candidate — the
    /// ladder never skips rung 1.
    #[tokio::test]
    async fn exact_match_takes_precedence_over_fuzzy_candidates() {
        let dir = unique_temp_dir("edit-file-exact-precedence");
        // "x();" appears exactly (line 1) and fuzzily ("  x();  ", line 2).
        let file_path = fixture_file(&dir, "x();\n  x();  \n").await;

        edit_file_fuzzy(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            old_string: "x();".to_string(),
            new_string: "y();".to_string(),
        })
        .await
        .expect_err("ambiguous at the EXACT rung: 'x();' is a substring of both lines");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Fuzzy ambiguity refuses the edit — same disambiguation principle
    /// as the exact rung.
    #[tokio::test]
    async fn fuzzy_ambiguity_is_refused() {
        let dir = unique_temp_dir("edit-file-fuzzy-ambiguous");
        let original = "  foo()\n  bar()\n    foo()\n";
        let file_path = fixture_file(&dir, original).await;

        let result = edit_file_fuzzy(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            // Trimmed, this matches both "  foo()" and "    foo()".
            old_string: "foo()".to_string(),
            new_string: "baz()".to_string(),
        })
        .await;

        assert!(result.is_err());
        let contents = tokio::fs::read_to_string(&file_path)
            .await
            .expect("read back");
        assert_eq!(contents, original, "file must be untouched");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Matching failures steer the model toward the whole-file path —
    /// the empirically better edit surface for small models.
    #[tokio::test]
    async fn not_found_error_steers_toward_write_file() {
        let dir = unique_temp_dir("edit-file-steering");
        let file_path = fixture_file(&dir, "hello world").await;

        let err = edit_file_fuzzy(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            old_string: "missing entirely".to_string(),
            new_string: "x".to_string(),
        })
        .await
        .expect_err("must fail");

        assert!(err.contains("write_file"), "got: {err}");
        assert!(err.contains("whitespace-tolerant"), "got: {err}");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// A blank-only old_string must not fuzzy-match everything once
    /// whitespace is ignored.
    #[tokio::test]
    async fn blank_only_old_string_never_fuzzy_matches() {
        let dir = unique_temp_dir("edit-file-blank");
        let original = "a\n\nb\n";
        let file_path = fixture_file(&dir, original).await;

        let result = edit_file_fuzzy(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            old_string: "   \n   ".to_string(),
            new_string: "x".to_string(),
        })
        .await;

        assert!(result.is_err());
        let contents = tokio::fs::read_to_string(&file_path)
            .await
            .expect("read back");
        assert_eq!(contents, original, "file must be untouched");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// A file without a trailing newline must not gain one through a
    /// fuzzy line-window edit. The multi-line old_string with no
    /// trailing spaces is NOT an exact substring (the file has trailing
    /// spaces on line 1), so this genuinely exercises rung 2.
    #[tokio::test]
    async fn fuzzy_edit_preserves_missing_trailing_newline() {
        let dir = unique_temp_dir("edit-file-no-trailing-nl");
        let file_path = fixture_file(&dir, "uno()   \ndos()").await;

        let result = edit_file_fuzzy(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            old_string: "uno()\ndos()".to_string(),
            new_string: "tres()\ndos()".to_string(),
        })
        .await
        .expect("fuzzy match should apply");
        assert!(result.contains("trailing whitespace"), "got: {result}");

        let contents = tokio::fs::read_to_string(&file_path)
            .await
            .expect("read back");
        assert_eq!(contents, "tres()\ndos()", "no trailing newline must appear");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // --- closest-line hint (docs/AUDITORIA-2026-07-v3.md, hallazgo A3) ---

    #[tokio::test]
    async fn not_found_error_suggests_the_closest_line_by_word_overlap() {
        let dir = unique_temp_dir("edit-file-closest-line");
        let original = "fn alpha() {}\nfn beta() {}\nfn compute_total(x: i32) -> i32 { x }\n";
        let file_path = fixture_file(&dir, original).await;

        let err = edit_file_fuzzy(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            // Not present verbatim, but shares "compute_total" with line 3.
            old_string: "fn compute_total(y: i32) -> i32 { y }".to_string(),
            new_string: "x".to_string(),
        })
        .await
        .expect_err("must fail: old_string isn't in the file");

        // Once `old_string` aligns with a real line, the divergence report
        // supersedes the word-overlap hint: it names the character.
        assert!(err.contains("First difference"), "got: {err}");
        assert!(err.contains("line 3 of the file"), "got: {err}");
        assert!(err.contains("U+0078 ('x')"), "got: {err}");
        assert!(err.contains("U+0079 ('y')"), "got: {err}");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// El diagnóstico sigue siendo la red cuando el peldaño 4 NO puede
    /// rescatar. Reorientado el 2026-08-07: el fixture original (borrado
    /// puro de `U+1D62`) ahora lo recupera el peldaño 4, así que este
    /// test usa una **sustitución** alrededor del mismo carácter — fuera
    /// del alcance del rescate por diseño, y exactamente donde nombrar el
    /// codepoint sigue siendo lo único que el harness puede ofrecer.
    #[tokio::test]
    async fn first_divergence_names_the_codepoint_the_model_could_not_emit() {
        let dir = unique_temp_dir("edit-file-unemittable-char");
        let original = "impl T {\n\
                        \x20   /// At each cell center:\n\
                        \x20   ///   d(x,y) = k * \u{3a3}\u{1d62} exp(-0.5 * (x-x\u{1d62})\u{b2})\n\
                        \x20   /// Note: planar space.\n\
                        \x20   pub fn kde(&self) {}\n\
                        }\n";
        let file_path = fixture_file(&dir, original).await;

        // Byte-identical to the file except that U+1D62 is dropped — the
        // exact corruption gpt-oss:20b produced, twice.
        // Sustitución, no borrado: el subíndice fue REEMPLAZADO por `j`.
        // El peldaño 4 se niega (solo acepta borrados) y el diagnóstico
        // tiene que nombrar el carácter igual.
        let old_string = "    /// At each cell center:\n\
                          \x20   ///   d(x,y) = k * \u{3a3}j exp(-0.5 * (x-xj)\u{b2})\n\
                          \x20   /// Note: planar space.\n";

        let err = edit_file_fuzzy(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            old_string: old_string.to_string(),
            new_string: String::new(),
        })
        .await
        .expect_err("must fail: the subscript is missing from old_string");

        assert!(err.contains("First difference"), "got: {err}");
        // Second line of old_string, third line of the file.
        assert!(err.contains("line 2, column"), "got: {err}");
        assert!(err.contains("line 3 of the file"), "got: {err}");
        // The whole point: the offending codepoint is named on both sides.
        assert!(err.contains("U+1D62"), "got: {err}");
        assert!(
            err.contains("cannot reproduce that character"),
            "must offer stopping as the correct move: {err}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Below the line limit the whole-file fallback stays on offer
    /// (Aider's small-model evidence, see `write_file_steering`).
    #[tokio::test]
    async fn small_files_still_steer_to_whole_file_rewrite() {
        let dir = unique_temp_dir("edit-file-small-steering");
        let file_path = fixture_file(&dir, "hello world\n").await;

        let err = edit_file_fuzzy(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            old_string: "totally unrelated content".to_string(),
            new_string: "x".to_string(),
        })
        .await
        .expect_err("must fail");

        assert!(err.contains("use write_file"), "got: {err}");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Above it the steering inverts: a whole-file rewrite of a large file
    /// re-types every untouched line, which is how the roam damage
    /// happened (`write_file_steering`'s doc comment).
    #[tokio::test]
    async fn large_files_steer_away_from_whole_file_rewrite() {
        let dir = unique_temp_dir("edit-file-large-steering");
        let original: String = (0..WHOLE_FILE_REWRITE_LINE_LIMIT + 40)
            .map(|i| format!("let v{i} = {i};\n"))
            .collect();
        let file_path = fixture_file(&dir, &original).await;

        let err = edit_file_fuzzy(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            old_string: "zzz absent zzz".to_string(),
            new_string: "x".to_string(),
        })
        .await
        .expect_err("must fail");

        assert!(err.contains("Do NOT work around this"), "got: {err}");
        assert!(
            !err.contains("use write_file with the complete"),
            "the small-file steering must not also appear: {err}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn not_found_error_has_no_hint_when_nothing_overlaps() {
        let dir = unique_temp_dir("edit-file-no-closest-line");
        let file_path = fixture_file(&dir, "hello world").await;

        let err = edit_file_fuzzy(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            old_string: "totally unrelated content".to_string(),
            new_string: "x".to_string(),
        })
        .await
        .expect_err("must fail");

        assert!(!err.contains("closest match"), "got: {err}");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // --- nonexistent file steers to write_file (hallazgo A4) ---

    #[tokio::test]
    async fn editing_a_nonexistent_file_steers_toward_write_file() {
        let dir = unique_temp_dir("edit-file-missing-file");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        let missing = dir.join("does-not-exist.txt");

        let err = edit_file_fuzzy(EditFileArgs {
            path: missing.to_string_lossy().into_owned(),
            old_string: "anything".to_string(),
            new_string: "x".to_string(),
        })
        .await
        .expect_err("must fail: the file doesn't exist");

        assert!(err.contains("does not exist"), "got: {err}");
        assert!(err.contains("write_file"), "got: {err}");
        assert!(
            !err.contains("os error"),
            "should not leak the raw OS error: {err}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // --- strict mode (hallazgo E1, docs/AUDITORIA-2026-07-v3.md) ---

    #[tokio::test]
    async fn strict_mode_still_applies_an_exact_match() {
        let dir = unique_temp_dir("edit-file-strict-exact");
        let file_path = fixture_file(&dir, "hello world").await;

        let result = edit_file(
            EditFileArgs {
                path: file_path.to_string_lossy().into_owned(),
                old_string: "world".to_string(),
                new_string: "braze".to_string(),
            },
            true,
            false, // gate: estos tests miden el matching, no el gate
        )
        .await;

        assert!(result.is_ok());
        let contents = tokio::fs::read_to_string(&file_path)
            .await
            .expect("read back");
        assert_eq!(contents, "hello braze");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn strict_mode_refuses_a_trailing_whitespace_only_match() {
        // Same fixture as `trailing_whitespace_difference_still_matches`
        // (fuzzy mode) — strict mode must refuse it instead of applying
        // rung 2.
        let dir = unique_temp_dir("edit-file-strict-refuses-fuzzy");
        let file_path = fixture_file(&dir, "fn main() {   \n    hola();\n}\n").await;

        let result = edit_file(
            EditFileArgs {
                path: file_path.to_string_lossy().into_owned(),
                old_string: "fn main() {\n    hola();".to_string(),
                new_string: "fn main() {\n    chao();".to_string(),
            },
            true,
            false, // gate: estos tests miden el matching, no el gate
        )
        .await;

        assert!(
            result.is_err(),
            "strict mode must not fall back to whitespace-tolerant matching"
        );
        let contents = tokio::fs::read_to_string(&file_path)
            .await
            .expect("read back");
        assert_eq!(
            contents, "fn main() {   \n    hola();\n}\n",
            "file must be untouched"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn strict_mode_refuses_an_indentation_only_match() {
        // Same fixture as
        // `indentation_difference_matches_and_preserves_the_files_indentation`
        // (fuzzy mode) — strict mode must refuse rung 3 too.
        let dir = unique_temp_dir("edit-file-strict-refuses-indent");
        let original = "mod x {\n        fn f() {\n            uno();\n        }\n}\n";
        let file_path = fixture_file(&dir, original).await;

        let result = edit_file(
            EditFileArgs {
                path: file_path.to_string_lossy().into_owned(),
                old_string: "    fn f() {\n        uno();\n    }".to_string(),
                new_string: "    fn f() {\n        dos();\n    }".to_string(),
            },
            true,
            false, // gate: estos tests miden el matching, no el gate
        )
        .await;

        assert!(result.is_err());
        let contents = tokio::fs::read_to_string(&file_path)
            .await
            .expect("read back");
        assert_eq!(contents, original, "file must be untouched");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // --- Peldaño 4: borrado de caracteres inemitibles (roadmap #3) ---

    /// El caso REAL, replay del incidente de roam (2026-07-28): el modelo
    /// mandó el bloque correcto salvo los tres `U+1D62` de la fórmula del
    /// KDE, que no puede emitir. Antes: rechazo honesto tras 4 rondas.
    /// Ahora: la edición se aplica al texto REAL del archivo.
    #[tokio::test]
    async fn the_roam_kde_block_is_recovered_from_dropped_subscripts() {
        let dir = unique_temp_dir("edit-file-rung4-roam");
        let original = "impl Trajectory {\n    /// At each cell center (x, y):\n    ///   density(x,y) = (1 / (n * 2*PI * h\u{b2})) * \u{3a3}\u{1d62} exp(-0.5 * ((x-x\u{1d62})\u{b2} + (y-y\u{1d62})\u{b2}) / h\u{b2})\n    /// The grid covers the bounding box.\n    pub fn kde(&self) -> f64 { 0.0 }\n}\n";
        let file_path = fixture_file(&dir, original).await;

        // Byte-idéntico salvo los tres ᵢ ausentes — lo que gpt-oss:20b
        // produjo, dos veces, en dos sesiones distintas.
        let old_string = "    /// At each cell center (x, y):\n    ///   density(x,y) = (1 / (n * 2*PI * h\u{b2})) * \u{3a3} exp(-0.5 * ((x-x)\u{b2} + (y-y)\u{b2}) / h\u{b2})\n    /// The grid covers the bounding box.\n";

        let summary = edit_file_fuzzy(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            old_string: old_string.to_string(),
            new_string: String::new(),
        })
        .await
        .expect("rung 4 must recover the block");

        assert!(summary.contains("missing 3 character(s)"), "got: {summary}");
        assert!(
            summary.contains("WARNING"),
            "el rescate nunca es silencioso: {summary}"
        );

        let after = tokio::fs::read_to_string(&file_path).await.unwrap();
        assert!(
            !after.contains("density"),
            "el bloque debe borrarse: {after}"
        );
        assert!(after.contains("impl Trajectory {"), "{after}");
        assert!(
            after.contains("pub fn kde(&self) -> f64 { 0.0 }"),
            "{after}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// MUTACIÓN 1 — falta un carácter ASCII. Un `)` ausente es semántico,
    /// no motor: negarse aunque el resto alinee perfecto.
    #[tokio::test]
    async fn rung4_refuses_when_the_missing_character_is_ascii() {
        let dir = unique_temp_dir("edit-file-rung4-ascii");
        let original =
            "fn compute(a: f64, b: f64) -> f64 {\n    let total = (a + b) * 2.0;\n    total\n}\n";
        let file_path = fixture_file(&dir, original).await;
        let old_string = "fn compute(a: f64, b: f64) -> f64 {\n    let total = (a + b * 2.0;\n";

        let err = edit_file_fuzzy(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            old_string: old_string.to_string(),
            new_string: "x".to_string(),
        })
        .await
        .expect_err("una omisión ASCII nunca se repara sola");
        assert!(err.contains("not found"), "got: {err}");
        assert_eq!(
            tokio::fs::read_to_string(&file_path).await.unwrap(),
            original,
            "el archivo no puede haber cambiado"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// MUTACIÓN 2 — sustitución, no borrado. El `format!` corrupto del
    /// mismo incidente (comilla MOVIDA) no califica: mover una comilla
    /// puede ser intención deliberada.
    #[tokio::test]
    async fn rung4_refuses_a_substitution_even_with_non_ascii_around() {
        let dir = unique_temp_dir("edit-file-rung4-subst");
        let original = "let msg = format!(\"cannot read '{}': {} \u{2014} aborting\", path, e);\nlet other = 1;\n";
        let file_path = fixture_file(&dir, original).await;
        let old_string =
            "let msg = format!(\"cannot read '{}'\": {} \u{2014} aborting, path, e);\n";

        let err = edit_file_fuzzy(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            old_string: old_string.to_string(),
            new_string: "y".to_string(),
        })
        .await
        .expect_err("una sustitución no es un borrado");
        assert!(err.contains("not found"), "got: {err}");
        assert_eq!(
            tokio::fs::read_to_string(&file_path).await.unwrap(),
            original
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// MUTACIÓN 3 — dos regiones admiten el mismo alineamiento. Sin
    /// unicidad no hay certificado: ambiguo, igual que los peldaños 2-3.
    #[tokio::test]
    async fn rung4_refuses_when_two_regions_admit_the_same_alignment() {
        let dir = unique_temp_dir("edit-file-rung4-ambig");
        let block = "    /// resumen: \u{3a3}\u{1d62} de los pesos normalizados del bloque\n    pub fn total(&self) -> f64 { 0.0 }\n";
        let original = format!("mod a {{\n{block}}}\n\nmod b {{\n{block}}}\n");
        let file_path = fixture_file(&dir, &original).await;
        let old_string = "    /// resumen: \u{3a3} de los pesos normalizados del bloque\n    pub fn total(&self) -> f64 { 0.0 }\n";

        let err = edit_file_fuzzy(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            old_string: old_string.to_string(),
            new_string: String::new(),
        })
        .await
        .expect_err("dos candidatos = ambiguo, no se adivina");
        assert!(err.contains("ambiguous"), "got: {err}");
        assert_eq!(
            tokio::fs::read_to_string(&file_path).await.unwrap(),
            original,
            "un ambiguo no toca el archivo"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// MUTACIÓN 4 — `old_string` corto: coincidencia plausible por azar.
    #[tokio::test]
    async fn rung4_refuses_a_short_old_string() {
        let dir = unique_temp_dir("edit-file-rung4-short");
        let original = "let x = \u{3a3}\u{1d62} + 1;\nlet y = 2;\n";
        let file_path = fixture_file(&dir, original).await;

        let err = edit_file_fuzzy(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            old_string: "let x = \u{3a3} + 1;\n".to_string(),
            new_string: "z".to_string(),
        })
        .await
        .expect_err("bajo el mínimo de longitud no aplica");
        assert!(err.contains("not found"), "got: {err}");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// El peldaño 4 es parte de la escalera fuzzy: `strict` (el
    /// `+ablate:strict-edit` del bench) debe apagarlo con los demás.
    #[tokio::test]
    async fn strict_mode_disables_rung4() {
        let dir = unique_temp_dir("edit-file-rung4-strict");
        let original = "impl T {\n    /// suma sobre los indices: \u{3a3}\u{1d62} w\u{1d62} x\u{1d62} normalizados aqui\n    pub fn f(&self) {}\n}\n";
        let file_path = fixture_file(&dir, original).await;
        let old_string = "    /// suma sobre los indices: \u{3a3} w x normalizados aqui\n    pub fn f(&self) {}\n";

        edit_file(
            EditFileArgs {
                path: file_path.to_string_lossy().into_owned(),
                old_string: old_string.to_string(),
                new_string: "    // borrado\n".to_string(),
            },
            false,
            false, // gate
        )
        .await
        .expect("fuzzy debe recuperarlo");

        tokio::fs::write(&file_path, original).await.unwrap();
        edit_file(
            EditFileArgs {
                path: file_path.to_string_lossy().into_owned(),
                old_string: old_string.to_string(),
                new_string: "    // borrado\n".to_string(),
            },
            true,
            false, // gate: estos tests miden el matching, no el gate
        )
        .await
        .expect_err("strict debe apagar el peldaño 4");
        assert_eq!(
            tokio::fs::read_to_string(&file_path).await.unwrap(),
            original
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
