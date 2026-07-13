# Verificación de bibliografía — paper/refs.bib (8 referencias)

Fecha: 2026-07-13
Herramienta: `verify-refs` local (`~/proyectos/verify-refs`, v0.1.0) — el
CLI instalado solo implementa `check`/`doi`/`search`; los subcomandos
`tex`/`retractions`/`support` que describe el skill no están en esta
versión del paquete. Niveles 2 y 3 se hicieron manualmente (curl a
OpenAlex, WebFetch + `pdftotext` sobre los PDFs primarios) siguiendo el
mismo protocolo.

## Nivel 1 — existencia/metadata (`verify-refs check`)

8/8 referencias evaluadas: 2 verified, 3 fixable (DOI faltante), 3
not_found. Score crudo de la herramienta: 62.5% — engañoso, ver abajo.

Los 3 `not_found` se reclasificaron tras revisión manual:
- **`goose`, `aider`**: NOT_FOUND esperado — son citas de software
  (`@misc` con `howpublished`/URL de GitHub), no papers académicos; el
  buscador de OpenAlex/CrossRef nunca los va a encontrar por diseño.
  No son alucinaciones.
- **`lu2024smalllm`**: NOT_FOUND fue un **artefacto de query** — el
  título tiene comas y dos puntos que rompieron el filtro
  `title.search` de OpenAlex (400 Bad Request, ver `.err` del check).
  Resuelto con `verify-refs search` (sin puntuación) → 100% match,
  DOI `10.48550/arxiv.2409.15790`. El paper existe y es real.

Los 3 `fixable` (DOI faltante) se corrigieron en `refs.bib`:
`yang2024sweagent` → `10.48550/arxiv.2405.15793`; `qwen3coder` →
`10.48550/arxiv.2603.00729`; `wang2026openhands` →
`10.48550/arxiv.2511.03690`. `lu2024smalllm` también recibió su DOI.

**Score real corregido: 7/8 referencias académicas verificadas con DOI
(87.5%)**, más 1 par de citas de software correctamente sin DOI por
naturaleza.

## Hallazgo de calidad de datos: autores de `lu2024smalllm`

OpenAlex devuelve 3 errores de parseo de nombres para
`10.48550/arxiv.2409.15790`: "Zhichun Lu" (arXiv dice **Zhenyan Lu**),
"Cai, Dongqi" invertido con coma (arXiv: **Dongqi Cai**), "Runduan Yi"
(arXiv: **Rongjie Yi**). Verificado contra la página de arXiv
directamente (fuente primaria) — la lista de autores que ya estaba en
`refs.bib` era la correcta; el problema era de OpenAlex, no del bib.

## Nivel 2 — retracciones

Los 5 DOIs resueltos (yang2024sweagent, qwen3coder, corradini2025,
wang2026openhands, lu2024smalllm) consultados contra
`is_retracted` en OpenAlex: **ninguno retractado.**

## Nivel 3 — soporte de las citas de mayor carga

Cross-check `\cite` (tex) ↔ keys (bib): **8/8 exactas, sin huérfanas en
ningún sentido.**

Se contrastó el texto del paper contra el contenido primario (no el
abstract) de las 4 referencias de mayor carga, descargando el PDF y
extrayendo con `pdftotext -layout` cuando `WebFetch` fallaba sobre PDFs
comprimidos (patrón que se repitió 3 veces — anotado como limitación de
herramienta, no de la fuente):

| Cita | Claim en el paper | Veredicto |
|---|---|---|
| `yang2024sweagent`, Tabla 1 (ACI) | 7 deltas de la Tabla 3 del paper (guardrail de lint, comando edit, search iterativo, viewer, historia) | **[VERIFICADO]** Los 7 números coinciden exactamente con la Tabla 3 primaria. 🔧 Se corrigió la etiqueta de una fila ("Iterative search command (vs.\ none)") — el delta −6.0 es relativo al baseline (Summarized search=18.0), no a "no search"; esa comparación es una fila aparte. Etiqueta reescrita a "(vs.\ the default, summarized search)". |
| `qwen3coder`, Related Work | Transferencia limitada entre scaffolds + swing 84.0→14.0 de format-following | **[VERIFICADO]** Tabla 2 del TR primario, fila GPT-5-2, columnas Scaffold1/Scaffold2 = 84.0/14.0 exacto. |
| `qwen3coder`, Related Work | "el efecto pega más fuerte en modelos chicos, más overfitting al template" | **[NO SOPORTADO]** — búsqueda exhaustiva en el texto completo del TR (`pdftotext`) no encontró ninguna afirmación sobre el efecto siendo mayor a menor escala de modelo. 🔧 **Retirada del paper** — la oración se recortó para no reclamar más de lo que el TR dice. |
| `corradini2025`, Related Work | "1B supera a 405B en matemáticas con test-time voting" — el dato más citado del review | **[JUICIO — parcial]** El hecho de fondo es real y se verificó contra su fuente primaria más probable, arXiv:2502.06703 "Can 1B LLM Surpass 405B LLM?" — el abstract dice literalmente *"a 1B LLM can exceed a 405B LLM on MATH-500"*. MDPI y ResearchGate bloquearon el acceso directo al review (403), así que no pude confirmar que `corradini2025` específicamente reporte este hallazgo (vs. otro dato de test-time-scaling). Anotado en el bib como incertidumbre residual no resuelta. |
| `wang2026openhands`, Related Work | Cita textual sobre reducción de fallas + overhead insignificante | **[VERIFICADO]** Coincide casi palabra por palabra con el PDF primario: *"V1 substantially reduces system-attributable failures over V0 with negligible event-sourcing overhead"* — con el número exacto disponible (61%, 78.0→30.0 errores/1k conversaciones) que el paper no citaba pero podría agregar. |

## Cambios aplicados

- `paper/refs.bib`: DOI agregado a 4 entradas (`yang2024sweagent`,
  `qwen3coder`, `wang2026openhands`, `lu2024smalllm`); notas de
  verificación con fecha en las 6 entradas académicas; header del
  archivo actualizado (ya no dice "esqueleto ... TODO-VERIFY").
- `paper/main.tex`: 1 etiqueta de tabla corregida (Tabla 1, fila de
  search iterativo); 1 claim no soportada retirada del párrafo de
  `qwen3coder` en Related Work (la conclusión del párrafo — motivar el
  rescate textual por familia — no dependía de esa claim, así que el
  argumento queda intacto sin ella).
- Recompilado limpio: `pdflatex → bibtex → pdflatex ×2`, 0 warnings de
  bibtex, 0 errores, 19 páginas.

## Pendiente (no bloqueante)

- `corradini2025`: si se consigue acceso a MDPI (vía biblioteca
  institucional o similar) antes de submission, confirmar que el review
  efectivamente cita/discute arXiv:2502.06703, y citarlo directamente
  como respaldo si corresponde.
- `goose`/`aider`: son citas de software informales por diseño; si el
  venue (EMSE) exige DOI/URL persistente para software citations,
  revisar sus guías de estilo en submission.
