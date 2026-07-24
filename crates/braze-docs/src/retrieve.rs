//! Recuperación léxica por solape de tokens — port de
//! `braze_engine::tool_search::search_stubs` (heading↔name, text↔summary).

use crate::chunk::DocChunk;

/// Peso de un término que aparece en el heading del chunk.
const HEADING_WEIGHT: usize = 3;
/// Peso de un término que aparece en el cuerpo del chunk.
const TEXT_WEIGHT: usize = 1;

/// Abstracción del retrieval para que un backend distinto (p.ej.
/// embeddings) pueda entrar sin tocar el chunker ni el loop. El MVP solo
/// implementa [`LexicalIndex`].
pub trait Retriever {
    /// Los `k` chunks más relevantes para `query`, de mayor a menor
    /// score. Query vacía o sin hits ⇒ vector vacío (no es un error).
    fn top_k(&self, query: &str, k: usize) -> Vec<&DocChunk>;
}

/// Índice léxico en memoria. Score por término = suma de:
/// [`HEADING_WEIGHT`] si el heading lo contiene + [`TEXT_WEIGHT`] si el
/// cuerpo lo contiene (una vez por término, no por ocurrencia — fiel al
/// scoring de `search_stubs`). Empates conservan el orden de indexación.
pub struct LexicalIndex {
    chunks: Vec<DocChunk>,
}

impl LexicalIndex {
    pub fn new(chunks: Vec<DocChunk>) -> Self {
        Self { chunks }
    }

    pub fn chunks(&self) -> &[DocChunk] {
        &self.chunks
    }

    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }
}

impl Retriever for LexicalIndex {
    fn top_k(&self, query: &str, k: usize) -> Vec<&DocChunk> {
        let terms: Vec<String> = query
            .split_whitespace()
            .map(|t| t.to_lowercase())
            .filter(|t| !t.is_empty())
            .collect();
        if terms.is_empty() {
            return Vec::new();
        }

        let mut scored: Vec<(usize, &DocChunk)> = self
            .chunks
            .iter()
            .filter_map(|chunk| {
                let heading = chunk.heading.to_lowercase();
                let text = chunk.text.to_lowercase();
                let score: usize = terms
                    .iter()
                    .map(|term| {
                        let mut s = 0;
                        if heading.contains(term.as_str()) {
                            s += HEADING_WEIGHT;
                        }
                        if text.contains(term.as_str()) {
                            s += TEXT_WEIGHT;
                        }
                        s
                    })
                    .sum();
                (score > 0).then_some((score, chunk))
            })
            .collect();

        // Stable sort: empates conservan el orden de catálogo → resultado
        // determinista (misma postura que search_stubs para el bench).
        scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        scored.into_iter().take(k).map(|(_, chunk)| chunk).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(id: usize, heading: &str, text: &str) -> DocChunk {
        DocChunk {
            id,
            source: "x.md".to_string(),
            heading: heading.to_string(),
            text: text.to_string(),
        }
    }

    #[test]
    fn heading_hits_outrank_text_only_hits() {
        let index = LexicalIndex::new(vec![
            chunk(0, "Configuración general", "opciones varias"),
            chunk(1, "Instalación", "revisa la configuración antes"),
        ]);
        let hits = index.top_k("configuración", 5);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].id, 0, "el hit en heading (peso 3) va primero");
        assert_eq!(hits[1].id, 1);
    }

    #[test]
    fn zero_score_chunks_are_excluded() {
        let index = LexicalIndex::new(vec![
            chunk(0, "Impresora", "cómo configurar la impresora"),
            chunk(1, "Red", "cómo configurar la red"),
        ]);
        let hits = index.top_k("impresora", 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, 0);
    }

    #[test]
    fn empty_query_returns_nothing() {
        let index = LexicalIndex::new(vec![chunk(0, "H", "cuerpo")]);
        assert!(index.top_k("", 5).is_empty());
        assert!(index.top_k("   ", 5).is_empty());
    }

    #[test]
    fn k_caps_the_result_count() {
        let index = LexicalIndex::new(vec![
            chunk(0, "heredero uno", "x"),
            chunk(1, "heredero dos", "x"),
            chunk(2, "heredero tres", "x"),
        ]);
        assert_eq!(index.top_k("heredero", 2).len(), 2);
    }

    #[test]
    fn case_insensitive_matching() {
        let index = LexicalIndex::new(vec![chunk(0, "Herederos", "Aprobar o Rechazar")]);
        assert_eq!(index.top_k("HEREDEROS rechazar", 5).len(), 1);
    }
}
