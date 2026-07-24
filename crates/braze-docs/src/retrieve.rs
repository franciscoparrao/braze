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
/// scoring de `search_stubs`), **multiplicado por el peso IDF del
/// término** cuando `use_idf` está activo. Empates conservan el orden de
/// indexación.
///
/// **IDF (2026-07-23):** el port original de `search_stubs` no pesaba por
/// rareza — y eso lo rompía en queries definicionales sobre corpus donde
/// un término es ubicuo. Caso real: "qué es braze" sobre los `docs/` del
/// proyecto, donde "braze" aparece en 69/77 archivos: sin IDF, "braze"
/// suma +3 a headings tangenciales y distorsiona el ranking en vez de
/// discriminar. El IDF (BM25 suavizado) lleva a ~0 el peso de un término
/// que está en (casi) todos los chunks. Kill-switch `BRAZE_DOCS_IDF=off`
/// (brazo de ablación) reproduce el scoring entero original.
pub struct LexicalIndex {
    chunks: Vec<DocChunk>,
    use_idf: bool,
}

impl LexicalIndex {
    /// IDF activado (el default nuevo — down-weight de términos ubicuos).
    pub fn new(chunks: Vec<DocChunk>) -> Self {
        Self {
            chunks,
            use_idf: true,
        }
    }

    /// Constructor explícito para el brazo de ablación: `use_idf=false`
    /// reproduce el scoring entero original de `search_stubs`.
    pub fn with_idf(chunks: Vec<DocChunk>, use_idf: bool) -> Self {
        Self { chunks, use_idf }
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

    /// Peso IDF de un término (ya en minúsculas): BM25 suavizado sobre el
    /// document-frequency del corpus. Siempre ≥ 0; ~0 para un término en
    /// (casi) todos los chunks. Con `use_idf=false` devuelve 1.0 →
    /// scoring plano idéntico al original.
    fn term_idf(&self, term: &str) -> f64 {
        if !self.use_idf {
            return 1.0;
        }
        let n = self.chunks.len() as f64;
        let df = self
            .chunks
            .iter()
            .filter(|c| {
                c.heading.to_lowercase().contains(term) || c.text.to_lowercase().contains(term)
            })
            .count() as f64;
        (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
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
        let weights: Vec<f64> = terms.iter().map(|t| self.term_idf(t)).collect();

        let mut scored: Vec<(f64, usize, &DocChunk)> = self
            .chunks
            .iter()
            .enumerate()
            .filter_map(|(idx, chunk)| {
                let heading = chunk.heading.to_lowercase();
                let text = chunk.text.to_lowercase();
                let score: f64 = terms
                    .iter()
                    .zip(&weights)
                    .map(|(term, weight)| {
                        let mut s = 0.0;
                        if heading.contains(term.as_str()) {
                            s += HEADING_WEIGHT as f64;
                        }
                        if text.contains(term.as_str()) {
                            s += TEXT_WEIGHT as f64;
                        }
                        s * weight
                    })
                    .sum();
                (score > 0.0).then_some((score, idx, chunk))
            })
            .collect();

        // Score desc, y ante empate el orden de indexación asc →
        // determinista (misma postura que search_stubs para el bench).
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.1.cmp(&b.1))
        });
        scored
            .into_iter()
            .take(k)
            .map(|(_, _, chunk)| chunk)
            .collect()
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

    /// El arreglo de fondo: cuando un término es ubicuo ("braze" en todos
    /// los chunks) y otro es discriminante ("localbackend" en uno solo),
    /// el IDF hace que el raro mande. Sin IDF, el ubicuo arrastra chunks
    /// irrelevantes arriba del único relevante.
    #[test]
    fn idf_lets_the_rare_term_win_over_a_ubiquitous_one() {
        let corpus = || {
            vec![
                chunk(0, "braze overview", "braze es un motor braze"),
                chunk(1, "notas", "el localbackend de braze"),
                chunk(2, "braze braze", "braze braze braze"),
            ]
        };
        // Sin IDF: "braze" (+3 heading) empuja 0 y 2 sobre el único chunk
        // que habla de localbackend → el relevante queda último.
        let plain = LexicalIndex::with_idf(corpus(), false);
        let plain_hits = plain.top_k("braze localbackend", 3);
        assert_eq!(
            plain_hits.last().unwrap().id,
            1,
            "sin IDF el chunk relevante (localbackend) queda al final"
        );
        // Con IDF: "braze" (df=3/3) pesa ~0, "localbackend" (df=1) domina.
        let idf = LexicalIndex::new(corpus());
        let idf_hits = idf.top_k("braze localbackend", 3);
        assert_eq!(
            idf_hits[0].id, 1,
            "con IDF el chunk relevante (localbackend) va primero"
        );
    }
}
