//! Herramientas diferidas en DOS niveles — C′.1 del estudio consolidado
//! (docs/harness-engineering-hooks-skills-2026-07-10.md § I.3).
//!
//! El nivel uno ya existía: los stubs (nombre+summary) van al contexto y
//! el schema completo se resuelve al dispatch. Este módulo agrega el
//! nivel dos: cuando un provider aporta MÁS de
//! [`DEFAULT_TOOL_SEARCH_THRESHOLD`] stubs, ni siquiera sus *nombres*
//! entran al inventario del modelo — se reemplazan por un único
//! meta-tool [`SEARCH_TOOL_NAME`] que busca por relevancia sobre
//! nombre+summary y "activa" los hits para las rondas siguientes.
//!
//! El caso que lo motiva es real, no hipotético: el MCP gateway GIS del
//! usuario de braze expone 1.500+ herramientas — solo los
//! nombres+summaries saturan el `num_ctx=8192` de un modelo local antes
//! de que el turno empiece, y para un 3B son distractores (la skill
//! `distractor_selection` mide exactamente esa debilidad con 2-3 tools
//! de ruido). Es el mismo argumento del colapso ACI, aplicado al
//! inventario.
//!
//! El umbral es POR PROVIDER (`ToolStub::source`): las 6 tools locales
//! siempre quedan visibles; un gateway gigante queda detrás del search.
//! La activación vive en el `Engine` (por sesión) — un hit de búsqueda
//! queda invocable el resto de la sesión, igual que en el harness que
//! inspiró el diseño.

use std::collections::{HashMap, HashSet};

use braze_types::ToolStub;

/// Stubs por provider a partir de los cuales sus tools dejan de listarse
/// y pasan detrás de `search_tools`. Con margen sobre el inventario
/// realista de un servidor MCP mediano (~decenas), y muy por debajo del
/// caso gateway (cientos-miles). Overrideable por config
/// (`tool_search_threshold`) y por fila de sweep
/// (`+ablate:tool-search-threshold=N`).
pub(crate) const DEFAULT_TOOL_SEARCH_THRESHOLD: usize = 40;

/// Cuántos hits devuelve (y activa) cada búsqueda — suficientes para que
/// el modelo elija, pocos para no reconstruir el problema de inventario
/// que este módulo existe para evitar.
pub(crate) const SEARCH_RESULTS_LIMIT: usize = 8;

/// Nombre del meta-tool. Un provider real que anuncie este mismo nombre
/// queda shadowed SOLO cuando la deferral está activa (hay tools
/// ocultas); sin deferral el nombre pasa intacto al registry.
pub(crate) const SEARCH_TOOL_NAME: &str = "search_tools";

/// Resultado de aplicar la deferral al inventario de una ronda.
pub(crate) struct DeferredInventory {
    /// Lo que el modelo ve: stubs de providers chicos + activados de
    /// providers grandes + (si algo quedó oculto) el stub de
    /// `search_tools`.
    pub visible: Vec<ToolStub>,
    /// Los stubs que quedaron detrás del search este round — el corpus
    /// sobre el que `search_tools` busca.
    pub hidden: Vec<ToolStub>,
}

/// Particiona `stubs` según el umbral por provider: los providers con
/// `<= threshold` stubs pasan enteros a `visible`; de los que lo
/// superan, solo sus stubs `activated`. Si algo quedó oculto, agrega el
/// stub del meta-tool al final de `visible`.
pub(crate) fn apply_deferral(
    stubs: Vec<ToolStub>,
    threshold: usize,
    activated: &HashSet<String>,
) -> DeferredInventory {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for stub in &stubs {
        *counts.entry(stub.source.clone()).or_default() += 1;
    }

    let mut visible = Vec::new();
    let mut hidden = Vec::new();
    for stub in stubs {
        let deferred_source = counts
            .get(&stub.source)
            .is_some_and(|&count| count > threshold);
        if !deferred_source || activated.contains(&stub.name) {
            visible.push(stub);
        } else {
            hidden.push(stub);
        }
    }

    if !hidden.is_empty() {
        visible.push(search_tool_stub(hidden.len()));
    }
    DeferredInventory { visible, hidden }
}

/// El inventario que el modelo VE en la primera ronda de una sesión
/// fresca (nada activado todavía): la partición de [`apply_deferral`]
/// con el set de activadas vacío, incluyendo el stub del meta-tool si
/// algo quedó oculto. Público para que los composition roots que
/// dimensionan presupuestos de contexto (J-17,
/// docs/AUDITORIA-2026-07-v7.md: `braze-bench::runner`) midan los bytes
/// del prompt real y no del catálogo completo pre-deferral — con N
/// noise tools ocultas, presupuestar sobre el catálogo entero le
/// achicaba el budget justo al brazo con deferral activa.
pub fn initially_visible_stubs(stubs: Vec<ToolStub>, threshold: usize) -> Vec<ToolStub> {
    apply_deferral(stubs, threshold, &HashSet::new()).visible
}

/// El stub del meta-tool, con el conteo de ocultas en el summary para
/// que el modelo sepa que el inventario visible no es todo lo que hay.
fn search_tool_stub(hidden_count: usize) -> ToolStub {
    ToolStub {
        name: SEARCH_TOOL_NAME.to_string(),
        summary: format!(
            "Search a catalog of {hidden_count} additional tools not listed here. \
             Pass keywords describing what you need (e.g. \"raster clip\", \"send \
             email\"); matching tools become available to call afterwards."
        ),
        source: "harness".to_string(),
        input_schema: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Keywords describing the capability you need."
                }
            },
            "required": ["query"],
            "additionalProperties": false
        })),
    }
}

/// Ranking léxico simple sobre nombre+summary, case-insensitive: cada
/// término del query suma 3 si aparece en el nombre, 1 si aparece en el
/// summary; score 0 no matchea. Deliberadamente no-BM25: el corpus son
/// frases de una línea y el consumidor es un modelo chico — determinismo
/// y explicabilidad valen más que ranking fino (misma postura que el
/// router de skills propuesto en la Parte III del estudio).
pub(crate) fn search_stubs<'a>(
    hidden: &'a [ToolStub],
    query: &str,
    limit: usize,
) -> Vec<&'a ToolStub> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|t| t.to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    if terms.is_empty() {
        return Vec::new();
    }

    let mut scored: Vec<(usize, &ToolStub)> = hidden
        .iter()
        .filter_map(|stub| {
            let name = stub.name.to_lowercase();
            let summary = stub.summary.to_lowercase();
            let score: usize = terms
                .iter()
                .map(|term| {
                    let mut s = 0;
                    if name.contains(term) {
                        s += 3;
                    }
                    if summary.contains(term) {
                        s += 1;
                    }
                    s
                })
                .sum();
            (score > 0).then_some((score, stub))
        })
        .collect();
    // Stable sort: equal scores keep catalog order — deterministic
    // results for a deterministic bench.
    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    scored.into_iter().take(limit).map(|(_, stub)| stub).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub(name: &str, summary: &str, source: &str) -> ToolStub {
        ToolStub {
            name: name.to_string(),
            summary: summary.to_string(),
            source: source.to_string(),
            input_schema: None,
        }
    }

    /// Below the threshold nothing changes — the 6 local tools must
    /// never end up behind a search step.
    #[test]
    fn small_providers_pass_through_untouched() {
        let stubs = vec![
            stub("read_file", "read a file", "local"),
            stub("write_file", "write a file", "local"),
        ];
        let inventory = apply_deferral(stubs, 40, &HashSet::new());
        assert_eq!(inventory.visible.len(), 2);
        assert!(inventory.hidden.is_empty());
        assert!(
            !inventory
                .visible
                .iter()
                .any(|s| s.name == SEARCH_TOOL_NAME),
            "no search stub when nothing is hidden"
        );
    }

    /// Over the threshold: the big provider's stubs hide, the small
    /// provider's stay, the search stub appears, and activation
    /// resurfaces individual tools.
    #[test]
    fn a_big_provider_hides_behind_the_search_stub_and_activation_resurfaces() {
        let mut stubs = vec![stub("read_file", "read a file", "local")];
        for i in 0..5 {
            stubs.push(stub(
                &format!("gis_tool_{i}"),
                "a gis operation",
                "mcp__gateway",
            ));
        }
        let mut activated = HashSet::new();
        activated.insert("gis_tool_3".to_string());

        let inventory = apply_deferral(stubs, 3, &activated);
        let visible_names: Vec<&str> =
            inventory.visible.iter().map(|s| s.name.as_str()).collect();
        assert!(visible_names.contains(&"read_file"));
        assert!(visible_names.contains(&"gis_tool_3"), "activated resurfaces");
        assert!(visible_names.contains(&SEARCH_TOOL_NAME));
        assert_eq!(inventory.hidden.len(), 4);
        assert!(
            inventory
                .visible
                .iter()
                .find(|s| s.name == SEARCH_TOOL_NAME)
                .unwrap()
                .summary
                .contains("4 additional tools"),
            "the model is told how much is hidden"
        );
    }

    /// Name hits outrank summary-only hits; zero-score stubs never
    /// appear; the limit caps the result.
    #[test]
    fn search_ranks_name_matches_above_summary_matches() {
        let hidden = vec![
            stub("buffer_vector", "buffer around geometries", "mcp"),
            stub("clip_raster", "clip a raster with a polygon buffer", "mcp"),
            stub("send_email", "send an email", "mcp"),
        ];
        let hits = search_stubs(&hidden, "buffer", 8);
        let names: Vec<&str> = hits.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["buffer_vector", "clip_raster"]);

        assert!(search_stubs(&hidden, "quaternion", 8).is_empty());
        assert_eq!(search_stubs(&hidden, "buffer", 1).len(), 1);
        assert!(search_stubs(&hidden, "   ", 8).is_empty());
    }
}
