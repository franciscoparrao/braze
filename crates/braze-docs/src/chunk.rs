//! Chunking de una wiki markdown en [`DocChunk`]s con procedencia.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::DocsError;

/// Tope de palabras por chunk antes de partir una sección por párrafos.
/// Palabras ≈ tokens a groso modo: mantiene cada fragmento chico para no
/// ahogar la síntesis de un modelo chico (el "modo degradado" que el
/// design doc identifica como el enemigo a matar). No parte a mitad de
/// párrafo — un párrafo más largo que el tope queda como un solo chunk.
pub const DEFAULT_MAX_CHUNK_WORDS: usize = 220;

/// Un fragmento recuperable de la documentación, anclado a su origen.
///
/// `heading` y `text` se guardan separados a propósito: el retriever los
/// pesa distinto (heading vale más, igual que `name` vs `summary` en
/// `search_stubs`), y el consumidor arma el prompt como
/// `## {heading}\n{text}` con la cita `[source: {source}]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocChunk {
    /// Id global en el orden de indexación (estable para un mismo árbol).
    pub id: usize,
    /// Ruta relativa a la raíz de la wiki, p.ej. `"procesos/herederos.md"`.
    pub source: String,
    /// Texto del heading ATX más cercano; vacío si el chunk precede a
    /// cualquier heading del archivo.
    pub heading: String,
    /// Cuerpo del fragmento (sin la línea del heading, que vive en `heading`).
    pub text: String,
}

/// Indexa recursivamente todos los archivos markdown bajo `dir` en
/// chunks. Orden determinista (archivos ordenados por ruta), ids
/// globales consecutivos.
pub fn chunk_wiki(dir: &Path) -> Result<Vec<DocChunk>, DocsError> {
    let mut files = Vec::new();
    collect_markdown(dir, dir, &mut files)?;
    files.sort();

    let mut chunks = Vec::new();
    let mut next_id = 0usize;
    for (rel, abs) in files {
        let content = fs::read_to_string(&abs).map_err(|source| DocsError::ReadFile {
            path: abs.clone(),
            source,
        })?;
        let file_chunks = chunk_markdown(&content, &rel, next_id, DEFAULT_MAX_CHUNK_WORDS);
        next_id += file_chunks.len();
        chunks.extend(file_chunks);
    }
    Ok(chunks)
}

/// Parte un solo documento markdown en memoria. Los ids arrancan en
/// `start_id` (para poder concatenar varios archivos con ids únicos).
/// Útil sin tocar disco — el camino que ejercitan los tests.
pub fn chunk_markdown(
    content: &str,
    source: &str,
    start_id: usize,
    max_words: usize,
) -> Vec<DocChunk> {
    let mut chunks = Vec::new();
    let mut id = start_id;
    for (heading, body) in split_sections(content) {
        for (h, text) in split_by_words(&heading, &body, max_words) {
            if h.is_empty() && text.is_empty() {
                continue;
            }
            chunks.push(DocChunk {
                id,
                source: source.to_string(),
                heading: h,
                text,
            });
            id += 1;
        }
    }
    chunks
}

/// Camina `dir` recursivamente juntando `(ruta_relativa, ruta_absoluta)`
/// de cada archivo markdown. `root` fija el prefijo que se recorta para
/// la ruta relativa.
fn collect_markdown(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, PathBuf)>,
) -> Result<(), DocsError> {
    let entries = fs::read_dir(dir).map_err(|source| DocsError::ReadDir {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| DocsError::ReadDir {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|source| DocsError::ReadDir {
            path: path.clone(),
            source,
        })?;
        if file_type.is_dir() {
            collect_markdown(root, &path, out)?;
        } else if file_type.is_file() && is_markdown(&path) {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, path));
        }
    }
    Ok(())
}

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
}

/// Parte el contenido en secciones `(heading, cuerpo)` por headings ATX.
/// El texto anterior al primer heading queda como `("", cuerpo)`.
fn split_sections(content: &str) -> Vec<(String, String)> {
    let mut sections = Vec::new();
    let mut heading = String::new();
    let mut body: Vec<&str> = Vec::new();

    for line in content.lines() {
        if let Some(h) = atx_heading(line) {
            push_section(&mut sections, &heading, &body);
            heading = h;
            body.clear();
        } else {
            body.push(line);
        }
    }
    push_section(&mut sections, &heading, &body);
    sections
}

fn push_section(sections: &mut Vec<(String, String)>, heading: &str, body: &[&str]) {
    let text = body.join("\n").trim().to_string();
    if heading.is_empty() && text.is_empty() {
        return;
    }
    sections.push((heading.to_string(), text));
}

/// Reconoce un heading ATX (`#`..`######` seguido de espacio o fin de
/// línea) y devuelve su texto sin los `#` de apertura/cierre. `#hashtag`
/// (sin espacio) NO es heading.
fn atx_heading(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let hashes = trimmed.chars().take_while(|&c| c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &trimmed[hashes..];
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    Some(rest.trim().trim_end_matches('#').trim().to_string())
}

/// Si el cuerpo excede `max_words`, lo reparte en varios chunks por
/// párrafos (empaque greedy), todos compartiendo el mismo heading. Un
/// párrafo más largo que el tope queda como su propio chunk sin cortarse.
fn split_by_words(heading: &str, body: &str, max_words: usize) -> Vec<(String, String)> {
    if max_words == 0 || word_count(body) <= max_words {
        return vec![(heading.to_string(), body.to_string())];
    }

    let mut out = Vec::new();
    let mut current = String::new();
    let mut current_words = 0usize;

    for para in body.split("\n\n").map(str::trim).filter(|p| !p.is_empty()) {
        let para_words = word_count(para);
        if current_words > 0 && current_words + para_words > max_words {
            out.push((heading.to_string(), std::mem::take(&mut current)));
            current_words = 0;
        }
        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(para);
        current_words += para_words;
    }
    if !current.is_empty() {
        out.push((heading.to_string(), current));
    }
    if out.is_empty() {
        out.push((heading.to_string(), body.to_string()));
    }
    out
}

fn word_count(s: &str) -> usize {
    s.split_whitespace().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_by_headings_and_anchors_source() {
        let md = "# Instalación\nCorre el instalador.\n\n## Requisitos\nRAM 4GB.";
        let chunks = chunk_markdown(md, "guia.md", 0, DEFAULT_MAX_CHUNK_WORDS);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].heading, "Instalación");
        assert_eq!(chunks[0].text, "Corre el instalador.");
        assert_eq!(chunks[0].source, "guia.md");
        assert_eq!(chunks[0].id, 0);
        assert_eq!(chunks[1].heading, "Requisitos");
        assert_eq!(chunks[1].id, 1);
    }

    #[test]
    fn preamble_before_first_heading_becomes_a_chunk() {
        let md = "Texto sin heading.\n\n# Sección\nCuerpo.";
        let chunks = chunk_markdown(md, "x.md", 0, DEFAULT_MAX_CHUNK_WORDS);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].heading, "");
        assert_eq!(chunks[0].text, "Texto sin heading.");
    }

    #[test]
    fn hashtag_without_space_is_not_a_heading() {
        assert_eq!(atx_heading("#hashtag"), None);
        assert_eq!(atx_heading("### Título"), Some("Título".to_string()));
        assert_eq!(atx_heading("####### demasiados"), None);
        assert_eq!(atx_heading("## Cierre ##"), Some("Cierre".to_string()));
    }

    #[test]
    fn oversized_section_splits_by_paragraphs() {
        // Dos párrafos de 6 palabras; con tope 8 no caben juntos.
        let body = "una dos tres cuatro cinco seis\n\nsiete ocho nueve diez once doce";
        let md = format!("# H\n{body}");
        let chunks = chunk_markdown(&md, "x.md", 0, 8);
        assert_eq!(chunks.len(), 2);
        assert!(chunks.iter().all(|c| c.heading == "H"));
        assert_eq!(chunks[0].text, "una dos tres cuatro cinco seis");
        assert_eq!(chunks[1].text, "siete ocho nueve diez once doce");
    }

    #[test]
    fn empty_input_yields_no_chunks() {
        assert!(chunk_markdown("", "x.md", 0, DEFAULT_MAX_CHUNK_WORDS).is_empty());
        assert!(chunk_markdown("\n\n   \n", "x.md", 0, DEFAULT_MAX_CHUNK_WORDS).is_empty());
    }
}
