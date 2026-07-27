//! Provider sintético de herramientas de ruido — el fixture del A/B de
//! `search_tools` (C′.1, docs/harness-engineering-hooks-skills-2026-07-10.md
//! § I.3).
//!
//! Simula un gateway MCP grande (el caso real: 1.500+ tools GIS) con
//! nombres y summaries plausibles pero irrelevantes para las tareas del
//! suite. Determinista por construcción: los nombres salen de un
//! producto fijo de vocabularios, sin aleatoriedad — dos sweeps con el
//! mismo `noise_tools` ven exactamente el mismo catálogo.
//!
//! Las tools de ruido son inertes pero REALES: resuelven schema e
//! invocan (devolviendo un resultado vacío inofensivo) — un modelo que
//! cae en el distractor obtiene una observación inútil y pierde la
//! ronda, que es exactamente el costo que el A/B quiere medir, no un
//! error artificial de harness.

use async_trait::async_trait;
use braze_tools_core::{ToolError, ToolProvider, ToolSchema};
use braze_types::{ToolCall, ToolResult, ToolStub};

/// Vocabularios del producto cartesiano verbo × dominio — 30 × 24 = 720
/// combinaciones disponibles antes de repetir con sufijo numérico;
/// suficiente para cualquier `noise_tools` realista del bench.
const VERBS: [&str; 30] = [
    "clip",
    "buffer",
    "merge",
    "split",
    "reproject",
    "rasterize",
    "vectorize",
    "smooth",
    "interpolate",
    "classify",
    "segment",
    "mosaic",
    "resample",
    "translate",
    "warp",
    "dissolve",
    "simplify",
    "densify",
    "snap",
    "align",
    "extract",
    "convert",
    "index",
    "tile",
    "stitch",
    "mask",
    "filter",
    "aggregate",
    "sample",
    "validate",
];
const DOMAINS: [&str; 24] = [
    "raster",
    "vector",
    "polygon",
    "layer",
    "dem",
    "pointcloud",
    "basin",
    "watershed",
    "contour",
    "grid",
    "mesh",
    "image",
    "band",
    "tile",
    "geometry",
    "feature",
    "dataset",
    "catalog",
    "metadata",
    "projection",
    "extent",
    "attribute",
    "index",
    "cache",
];

/// El provider: `count` tools de ruido, nombres `noise_<verb>_<domain>`
/// (con sufijo `_N` al agotar el producto).
pub struct NoiseToolsProvider {
    count: usize,
}

impl NoiseToolsProvider {
    pub fn new(count: usize) -> Self {
        Self { count }
    }

    fn tool_name(index: usize) -> String {
        let verb = VERBS[index % VERBS.len()];
        let domain = DOMAINS[(index / VERBS.len()) % DOMAINS.len()];
        let round = index / (VERBS.len() * DOMAINS.len());
        if round == 0 {
            format!("noise_{verb}_{domain}")
        } else {
            format!("noise_{verb}_{domain}_{round}")
        }
    }

    fn owns(&self, name: &str) -> bool {
        (0..self.count).any(|i| Self::tool_name(i) == name)
    }
}

#[async_trait]
impl ToolProvider for NoiseToolsProvider {
    fn provider_id(&self) -> &str {
        "bench:noise"
    }

    async fn list_stubs(&self) -> Result<Vec<ToolStub>, ToolError> {
        Ok((0..self.count)
            .map(|i| {
                let name = Self::tool_name(i);
                let verb = VERBS[i % VERBS.len()];
                let domain = DOMAINS[(i / VERBS.len()) % DOMAINS.len()];
                ToolStub {
                    name,
                    summary: format!("{verb} operation over a {domain} input"),
                    source: "bench:noise".to_string(),
                    input_schema: None,
                }
            })
            .collect())
    }

    async fn resolve_schema(&self, name: &str) -> Result<Option<ToolSchema>, ToolError> {
        if self.owns(name) {
            Ok(Some(ToolSchema {
                name: name.to_string(),
                description: "synthetic bench noise tool".to_string(),
                input_schema: serde_json::json!({ "type": "object" }),
            }))
        } else {
            Ok(None)
        }
    }

    async fn invoke(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
        // Inerte pero real: el distractor cuesta una ronda, no un error
        // de harness.
        Ok(ToolResult {
            tool_call_id: call.id.clone(),
            content: "(no output — this tool does not apply to the current task)".to_string(),
            is_error: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Determinism + uniqueness — the properties the A/B's
    /// reproducibility rides on.
    #[tokio::test]
    async fn the_catalog_is_deterministic_and_collision_free() {
        let provider = NoiseToolsProvider::new(200);
        let stubs_a = provider.list_stubs().await.unwrap();
        let stubs_b = provider.list_stubs().await.unwrap();
        assert_eq!(stubs_a.len(), 200);
        let names_a: Vec<&str> = stubs_a.iter().map(|s| s.name.as_str()).collect();
        let names_b: Vec<&str> = stubs_b.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names_a, names_b, "deterministic across calls");
        let unique: std::collections::HashSet<&&str> = names_a.iter().collect();
        assert_eq!(unique.len(), 200, "no name collisions");
    }

    /// Every advertised tool resolves and invokes inertly; unknown names
    /// stay unclaimed (registry routing depends on it).
    #[tokio::test]
    async fn advertised_tools_resolve_and_invoke_inertly() {
        let provider = NoiseToolsProvider::new(5);
        let stubs = provider.list_stubs().await.unwrap();
        let name = stubs[0].name.clone();
        assert!(provider.resolve_schema(&name).await.unwrap().is_some());
        assert!(
            provider
                .resolve_schema("read_file")
                .await
                .unwrap()
                .is_none(),
            "must not claim other providers' tools"
        );
        let result = provider
            .invoke(&ToolCall {
                id: "c1".to_string(),
                name,
                arguments: serde_json::json!({}),
            })
            .await
            .unwrap();
        assert!(!result.is_error);
    }
}
