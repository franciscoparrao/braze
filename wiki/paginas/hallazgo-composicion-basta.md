---
type: wiki-page
created: 2026-07-14
tags: [paper, empirico, hallazgo]
---

# Hallazgo: "la composición basta" (gemma4:e4b ≈ compuesto braze ≈ loop bare)

## Qué es

Tres mediciones independientes de pass rate sobre la suite
`default.toml`, escala 1B, resultaron **mutuamente indistinguibles**
(pooled a $n{=}285$ por brazo):

| Brazo | Pass rate | Wilson 95% CI |
|---|---|---|
| `gemma4:e4b` solo (sin executor 1B) | 91.2% | [87.4, 94.0] |
| Compuesto completo de `braze` (1B+lead) | 88.8% | [84.6, 91.9] |
| Loop bare lead+executor (sin ninguna palanca de `braze`) | 87.4% | [83.0, 90.7] |

Ningún delta pareado excluye cero (Newcombe 95%, el más cercano a
significativo: bare − solo = −3.9pp [−9.0, +1.3]).

## Por qué existe

El headline original del paper (`paper/main.tex`) era "un executor de
1B con lead (89%) supera a los baselines sin asistir de 3B (68%) y 7B
(80%) — el harness compensa la escala". La review EMSE (Persona B,
calibrada contra el board real del journal) señaló el hueco: nunca se
midió qué saca `gemma4:e4b` —el modelo que abre el turno en TODO arm
`+lead`— por sí solo. Sin ese control, "el compuesto supera a 3B/7B" no
distingue "el harness fabrica capacidad" de "el compuesto simplemente
hereda el techo del lead model".

## Detalles

### Cadena de resolución (2026-07-13)

1. **Fase 1** — baseline solo de `gemma4:e4b`: 87/95 (91.6%),
   indistinguible del compuesto (85/95, 89.5%). Primer disparo del
   criterio pre-registrado "REVISAR FRAMING".
2. **Fase 3** — baseline de harness externo: se construyó
   `BareLeadExecutor` (`crates/braze-bench/src/bare_lead_baseline.rs`),
   un loop lead+executor **implementado desde cero** (no reusa
   `EscalatingBackend` ni `Engine`), sin rescate textual, sin
   compactación, sin tool deferral, sin post-edit check — solo la
   composición cruda. Resultado: 84/95 (88.4%), también indistinguible
   de los otros dos.
3. **Aumento de potencia** — se replicaron los tres brazos a
   $n{=}285$ (10 repeticiones más cada uno). El nulo se confirmó con
   el doble de precisión (semiancho ~3.5pp vs ~6-7pp original), mismos
   tres puntos estimados estables (cambio <1pp entre rondas).

### Qué significa (y qué NO significa)

**Sí significa**: el salto de 19% (1B solo) a 89% (1B+lead) sigue
siendo real y grande — el compuesto rescata genuinamente al 1B de su
propio baseline. Lo que deja de sostenerse es la atribución: nada en
los datos separa "el compuesto logra algo más allá del techo de
`gemma4:e4b`" de "ni la composición lead+executor ni la ingeniería
adicional de `braze` agregan una ganancia medible sobre ese techo, en
esta suite, a esta escala".

**No significa** que la ingeniería de `braze` (rescate, compactación,
deferral) no sirva para nada — es un resultado nulo por falta de
potencia para detectar un efecto, no evidencia de que el efecto sea
exactamente cero. Los CIs, incluso a $n{=}285$, siguen siendo
compatibles con un efecto real de hasta ~5-9pp.

### Impacto en el paper

El throughline se corrió de *"el harness fabrica capacidad"* a *"el
harness decide a qué capacidad enrutar, y eso también hay que
medirlo"* — reflejado en abstract, contribuciones, nueva
`\S\ref{sec:external}`, `\S\ref{sec:threats}` y la conclusión.

## Relacionado

- [[venue-y-review-emse]] — el review que motivó esta investigación
- [[modelos-locales-thinking]] — `gemma4:e4b` también resultó ser el
  candidato "no-thinking" más fuerte del proyecto

## Referencias

- `docs/gemma4-e4b-solo-baseline-design.md`
- `docs/external-harness-baseline-design.md`
- `docs/power-increase-2026-07-13.md`
- `docs/sweep-gemma4-e4b-solo-2026-07-13.json` / `-power-2026-07-13.json`
- `docs/sweep-external-bare-lead-2026-07-13.json` / `-power-2026-07-13.json`
- `crates/braze-bench/src/bare_lead_baseline.rs`
- `crates/braze-bench/src/external.rs`
