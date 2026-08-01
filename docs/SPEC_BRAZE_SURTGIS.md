# SPEC — Braze × SurtGIS: análisis geoespacial agéntico como dominio-benchmark de modelos pequeños

> **Propósito:** enchufar SurtGIS (motor geoespacial Rust, 100+ algoritmos,
> validado a precisión de máquina vs GDAL/GRASS/TauDEM/TopoToolbox) como
> **dominio de tools de Braze**, para (a) obtener un agente GIS local, offline,
> single-binary, y (b) —el payload alto— un **benchmark con oráculo automático**
> que pone a prueba la tesis de Braze ("el harness compensa la escala del
> modelo") en un dominio novel y con ground truth chequeable por máquina, no en
> otro SWE-bench.
> **Estado:** propuesta de diseño (2026-07-12). **NO es implementación** — es el
> contrato para andamiar tres piezas: un `ToolProvider` nativo, un set de skills
> afinadas para SLM, y una suite de bench con verificador raster.
> **Repos:** todo el trabajo vive en `braze/`; SurtGIS entra solo como dependencia
> (`surtgis-algorithms`/`surtgis-core` de crates.io). **Nada agéntico entra a
> `surtgis-core`.**

---

## 0. Por qué esta mezcla (y por qué ahora)

Braze es un harness "maestro en modelos pequeños": la calidad del ACI compensa
la escala del modelo. Esa tesis necesita **dominios donde medirse**, y el default
del campo es SWE-bench una y otra vez. El análisis geoespacial es un banco de
pruebas casi ideal y sin explotar:

- **Es multi-paso y tool-heavy** (leer DEM → fill sinks → flow direction →
  flow accumulation → extraer red → delimitar cuenca; o leer bandas → band-math
  NDVI → reclasificar → exportar). Justo donde el diseño del ACI pesa más que el
  tamaño del modelo.
- **Tiene ground truth verificable por máquina.** SurtGIS está validado a
  precisión de máquina contra GDAL/GRASS/TauDEM/TopoToolbox. Eso da un **oráculo
  automático**: "¿la cuenca que calculó el agente coincide con la referencia
  dentro de tolerancia?" es un check numérico, no un juez LLM.

El resultado —modelo chico local (Ollama) + harness Braze + tools SurtGIS, un
binario offline con oráculo determinista— es algo que ninguno de los dos
proyectos tiene solo, y le da al paper de Braze el dominio de evaluación con
ground truth que hoy le falta.

---

## 1. Decisión de acople: `ToolProvider` nativo, NO MCP

Registrada explícitamente porque fue la primera pregunta de diseño. El trait
`braze_tools_core::ToolProvider` (`provider_id`/`list_stubs`/`resolve_schema`/
`invoke`) lo implementan como **hermanos** `LocalToolsProvider` (nativo,
in-process) y el cliente MCP. Escalera de acople, de liviano a pesado:

| Rung | Vía | Peso | Veredicto |
|------|-----|------|-----------|
| 0 | Agente usa `shell_exec` → binario `surtgis` | Cero código | Solo spike inicial. CLI cruda = ACI pobre para un SLM (sin stubs, sin deferral). |
| 1 | `braze-mcp-client` → server MCP Python (rasterio) | Subproceso Python + serialización | **Rechazado como vía principal:** rompe single-binary/offline. Útil solo como opción portable (otros agentes) o prototipo. |
| 2 | **`braze-tools-surtgis` nativo `impl ToolProvider`** | In-process, sin subproceso | **Elegido.** On-ethos para ambos; un binario, offline; control total del ACI. |

Los ~42 wrappers del server MCP existente (`surtgis-mcp-server`) se portan casi
1:1 al provider nativo. El trabajo real no es el plumbing sino el diseño del ACI
(ver §2.1). Los rásters se intercambian **por ruta de archivo** usando el I/O
GeoTIFF nativo de `surtgis-core` — sin GDAL, sin rasterio.

---

## 2. Las tres piezas

### 2.1 Pieza 1 (prerrequisito) — `braze-tools-surtgis`: provider nativo

Nuevo crate paralelo a `braze-tools-local`. Cada algoritmo GIS expuesto sigue el
mismo patrón que los tools locales: un `stub()` → `ToolStub` (nombre +
one-liner, siempre en contexto) y un `invoke` que resuelve el schema bajo demanda
y ejecuta.

```rust
pub struct SurtgisToolsProvider { work_dir: PathBuf, /* … */ }

#[async_trait]
impl ToolProvider for SurtgisToolsProvider {
    fn provider_id(&self) -> &str { "surtgis" }
    async fn list_stubs(&self) -> Result<Vec<ToolStub>, ToolError> { /* slope, aspect,
        hillshade, fill_sinks, flow_direction, flow_accumulation, watershed, hand,
        stream_network, ndvi, band_math, reclassify, … */ }
    async fn resolve_schema(&self, name: &str) -> Result<Option<ToolSchema>, ToolError> { … }
    async fn invoke(&self, call: &ToolCall) -> Result<ToolResult, ToolError> { … }
}
```

**Diseño del ACI (esto es lo que importa, no el wrapper):**

- **Entradas/salidas por ruta.** El tool recibe `input: "dem.tif"`, `output:
  "flow.tif"` (rutas relativas al sandbox). Nunca serializar arrays por el canal
  del modelo.
- **Observaciones *shaped* para SLM.** El `ToolResult` de una op raster NO
  vuelca el raster: devuelve un resumen compacto —`wrote flow.tif [2000×2000
  f32]; min=0 max=1.4e6 mean=312 nodata=1.2%`— para que el modelo chico razone
  el siguiente paso sin ahogarse en datos.
- **Defaults sensatos + parámetros mínimos.** `slope` no debería exigir
  `z_factor`/`units` salvo que se pidan; cada parámetro extra es una oportunidad
  de que un 3B alucine. Firmas cortas.
- **Errores accionables de vuelta al modelo** (mismo espíritu que el guardrail
  `cargo check` post-edit de Braze): "input CRS is geographic; flow routing needs
  a projected grid — reproject first" en el `ToolResult`, no un stack trace.

*Verificar primero:* el patrón exacto `stub()`/`list_stubs`/`resolve_schema` en
`braze-tools-local/src/provider.rs`, y la forma de `ToolStub`/`ToolSchema`/
`ToolResult` en `braze-types`, para calcar la convención sin inventar otra.

### 2.2 Pieza 2 — Skills geoespaciales afinadas para SLM

`braze-skills` es explícito (`crates/braze-skills/src/lib.rs`): las skills son
`SKILL.md` (frontmatter `name:`/`description:` + body markdown), con **carga
diferida por mención explícita `$nombre`**, allowlist de paths **vacía por
default**, body capado a 64 KB, y una advertencia de diseño central:

> **NO cargar las skills de un entorno frontier tal cual — en un 3B son
> distractores.**

Por lo tanto los skills `/analisis-cuenca` y `/mapa-ndvi` que ya existen para
Claude Code **no sirven verbatim**: hay que reescribirlos **cortos y
procedurales** para el modelo chico. Cada uno es una receta canónica:

- `$cuenca` — fill sinks → flow direction → flow accumulation → umbral de red →
  delimitar watershed en el pour point dado. 15-20 líneas, imperativas.
- `$ndvi` — identificar bandas red/nir → band-math (nir−red)/(nir+red) →
  (opcional) reclasificar → exportar. 

Son **memoria procedural**, no system prompt: entran solo cuando la tarea las
menciona. Viven en `skills/<nombre>/SKILL.md` bajo un path de la allowlist de
config del run.

*Verificar primero:* `SkillRegistry::discover`, `load_body`,
`LoadedSkill::prompt_addendum` y `explicit_mentions` para el contrato exacto de
formato y disparo.

### 2.3 Pieza 3 (el payload) — Suite de bench + oráculo raster

`braze-bench` corre cada `TaskDef` (TOML) por el `Engine::run_turn` real en un
`TaskSandbox` aislado, y `metrics::compute_metrics` convierte los campos en
veredicto. El `TaskDef` actual verifica por substring de texto/archivo
(`expect_text_contains`, `expect_file_contains`), presupuestos
(`expect_max_rounds`/`_tokens`/`_cost_usd`), label `skill`, y `noise_tools`.

**El gap:** no hay oráculo **numérico/raster**. "¿El agente calculó la cuenca
correcta?" no se puede verificar por substring. Extensión mínima (fiel al
principio del bench de "no un lenguaje de aserción general", solo un campo más):

```rust
// en TaskDef
#[serde(default)]
pub expect_raster_matches: HashMap<String, RasterExpectation>,

pub struct RasterExpectation {
    pub reference: String,          // ruta a la referencia (fuera del sandbox)
    pub metric: RasterMetric,       // Rmse | Mae | CategoricalAgreement
    pub tol: f64,                   // p.ej. rmse ≤ 1e-3, o agreement ≥ 0.99
}
```

**El cierre elegante: el verificador es SurtGIS.** Tras el turno,
`compute_metrics` lee el output del sandbox y la referencia con el I/O de
`surtgis-core` y calcula la métrica (RMSE / MAE / acuerdo categórico). La
**referencia** la genera el pipeline ya validado de SurtGIS (un `make
references` que corre la secuencia correcta sobre los DEMs de prueba). Así el
oráculo es determinista y no depende de un juez LLM ni de Python.

**Qué mide la suite (más allá de pass/fail):**

- **Correctitud** por `expect_raster_matches` (el mapa está bien) + `skill` label
  para desglosar por capacidad (`single_tool`, `multi_step`, `error_recovery`).
- **Eficiencia** por `expect_max_rounds`/`_tokens`/`_cost_usd`: un config que
  acierta en 3 rounds es mejor que uno que acierta en 14.
- **El A/B de deferral de tools** vía `noise_tools` + el
  `+ablate:tool-search-threshold`: con ~40 tools GIS el catálogo *always-in-
  context* es grande — ¿un SLM rinde mejor si se esconde tras `search_tools`?
  Resultado medible y publicable.
- **El efecto de las skills** (Pieza 2): A/B con/sin `$cuenca` cargado — ¿la
  memoria procedural sube la tasa de éxito del modelo chico? Es exactamente la
  tesis harness-compensates-for-scale, cuantificada.

*Verificar primero:* la firma real de `metrics::compute_metrics` y `TaskResult`,
y dónde se engancharía la lectura post-turno del sandbox (`sandbox.rs`), para
insertar el verificador raster sin romper los oráculos existentes.

---

## 3. Modelo de ejecución y single-binary

- Con el provider nativo, un solo binario Braze linkea `braze-engine` +
  `braze-tools-surtgis` (→ `surtgis-algorithms`) + backend Ollama = **agente GIS
  offline, sin Python, sin GDAL, sin red**.
- Los rásters de prueba deben ser **chicos** (DEMs sintéticos tipo `fbm_256` o un
  recorte real pequeño) para que cada tarea del sandbox sea liviana y el bench
  corra en minutos sobre muchos modelos.
- El sandbox ya aísla filesystem por tarea (`TaskSandbox`); las referencias viven
  fuera del sandbox (solo-lectura para el verificador).

---

## 4. Criterios de aceptación (Definition of Done del scaffold)

**Pieza 1 — provider**
- [ ] Crate `braze-tools-surtgis` con `SurtgisToolsProvider` implementando
      `ToolProvider`; ≥6 tools reales (slope, hillshade, fill_sinks,
      flow_direction, flow_accumulation, watershed) con `stub()` + `invoke`
      (cuerpo real, llamando a `surtgis-algorithms`).
- [ ] Observaciones *shaped* (resumen numérico, no raster) verificadas en un test.
- [ ] Se compone en el `ToolRegistry` del engine junto a `LocalToolsProvider`.

**Pieza 2 — skills**
- [ ] `skills/cuenca/SKILL.md` y `skills/ndvi/SKILL.md`, cortos, cargados por
      `$cuenca`/`$ndvi`, dentro del cap de 64 KB.

**Pieza 3 — bench**
- [ ] `TaskDef::expect_raster_matches` + `RasterExpectation` + verificador en
      `compute_metrics` (RMSE con tolerancia), con la lectura de raster vía
      `surtgis-core`.
- [ ] `make references` que genera las referencias con el pipeline validado.
- [ ] Suite `suites/geospatial.toml` con ≥5 tareas (1 single-tool, 2 multi-step,
      1 error-recovery con CRS geográfico, 1 NDVI), cada una con su
      `expect_raster_matches`.
- [ ] Un run end-to-end contra ≥2 backends (un SLM Ollama + un baseline) que
      produce un reporte con tasa de éxito por `skill` y por backend. CI verde.

---

## 5. No-objetivos

- **NO** meter nada agéntico en `surtgis-core`/`surtgis-algorithms`: SurtGIS es
  proveedor de tools; Braze es el harness. El acople es un `ToolProvider`.
- **NO** usar MCP como vía principal (ver §1); queda como opción portable.
- **NO** un juez LLM para el oráculo: el ground truth geoespacial es numérico y
  determinista — usarlo.
- **NO** portar las skills frontier verbatim (son distractores para un 3B).
- **NO** rásters grandes en el bench (mantener las tareas livianas).

## 6. Preguntas abiertas

- **Categóricos vs continuos:** flow direction (D8) es categórico (8 clases) →
  el oráculo debe ser "acuerdo de clase ≥ X%", no RMSE. Watershed es máscara
  binaria → IoU/agreement. Definir `RasterMetric` para cubrir ambos desde el
  principio.
- **Tolerancia de pour point:** en tareas de cuenca, ¿el prompt fija el pour
  point exacto, o el agente lo elige? Si lo elige, la referencia depende de su
  elección → fijar el pour point en el prompt para que la referencia sea única.
- **Datos:** ¿DEM sintético (reproducible, ya lo tienes) o recorte real (más
  representativo pero pesado)? Probable: sintético para correctitud, uno real
  chico para una tarea "realista".
- **¿Provider nativo o CLI-shell para el bench?** El nativo da observaciones
  *shaped*; el shell (Rung 0) es cero código. Para el bench, el nativo es el que
  mide el ACI de verdad — la Pieza 1 es prerrequisito real, no opcional.

## 7. Orden de ejecución

1. **Pieza 1** (provider nativo, ≥6 tools) — sin esto no hay ACI que medir.
2. **Pieza 3** (oráculo raster + suite mínima) — el payload; empieza a dar señal
   apenas hay 5 tareas.
3. **Pieza 2** (skills) — habilita el A/B con/sin memoria procedural.
4. Escalar tools y tareas; correr el barrido de backends para el paper.

## 8. Qué gana cada proyecto

- **Braze:** un dominio de evaluación novel (agentic geospatial) con **oráculo
  automático y determinista** — evidencia directa, no-SWE-bench, para la tesis
  harness-compensates-for-scale (efecto del ACI shaping, del deferral de tools,
  y de las skills, todo cuantificado por modelo).
- **SurtGIS:** un frontend agéntico y una historia de "agente GIS local y
  privado, un binario, offline" que ninguna librería GIS mainstream tiene.
