# Pre-registro: A/B de lead-summary y TTC local — las dos palancas v8 sin veredicto

Fecha: 2026-08-10
Estado: **CERRADO el mismo día — TTC RECHAZADO (Δ≤0 en ambos; en llama
significativamente dañino: 0/8 discordantes, p=0,0078 — el modo de un
modelo débil es un error estable que gana la votación), lead-summary
NO-ADOPTADO por tamaño (+6,3pp, IC cruza cero) con señal direccional
positiva (6/0 discordantes, p=0,031).** Mecanismo validado en ambos.
El sweep original sufrió OOM-kill de Ollama en Nitro a mitad de camino
(breaker protegió la estadística); brazos dañados re-corridos con seed
pareado. Ninguna regla se modificó después de correr. Detalle:
`docs/sweep-lead-summary-ttc-2026-08-10.md`.
Criterios fijados ANTES de
correr, disciplina de `docs/constrained-decoding-ab-design.md`. Ambas
palancas están implementadas desde v8 (§ 6 summary-por-lead `c47c478`,
§ 6.15 TTC `908348b`) y nunca se midieron — son las últimas palancas del
proyecto con mecanismo en código y sin dato. Nota de higiene: el roadmap
de técnicas (2026-08-06, línea "prior débil tras el nulo del
lead-summary") afirma un nulo que NO existe en ningún sweep del repo ni
de Nitro — este experimento lo salda con datos; si sale nulo, la frase
queda retroactivamente correcta por suerte, no por evidencia.

## Preguntas

1. **Lead-summary**: cuando la compactación reemplaza eventos por un
   summary, ¿un summary generado por el modelo lead (más capaz) preserva
   mejor el contexto operativo que el digest extractivo determinístico —
   medido como pass rate del worker?
2. **TTC local**: ¿comprar confiabilidad con cómputo local extra (N=3
   rollouts + auto-consistencia sobre `outcome_fingerprint`) paga en los
   executors no saturados?

## Brazos (un solo sweep, `default.toml`, 5 reps, seed 42, temp 0.2, Nitro)

| # | Fila | Qué aísla |
|---|---|---|
| 1 | `ollama:qwen2.5:3b` | baseline TTC (qwen) |
| 2 | `ollama:qwen2.5:3b+ablate:ttc=3` | TTC sobre 1 |
| 3 | `ollama:llama3.2:1b` | baseline TTC (llama) |
| 4 | `ollama:llama3.2:1b+ablate:ttc=3` | TTC sobre 3 |
| 5 | `ollama:qwen2.5:3b+lead:ollama:qwen2.5:7b+ablate:tactical-threshold=4` | lead + compactación forzada + digest determinístico |
| 6 | `ollama:qwen2.5:3b+lead:ollama:qwen2.5:7b+ablate:tactical-threshold=4;lead-summary` | ídem con summary-por-lead |

570 corridas nominales (las filas TTC ejecutan 3 rollouts por
repetición — su costo real se reporta, no se descuenta). El threshold=4
va en AMBOS brazos del par 5/6: la comparación aísla la FUENTE del
summary bajo presión de compactación idéntica, no la compactación.
Executors elegidos por headroom medido hoy (qwen2.5:3b 75,8%,
llama3.2:1b 25,3% en este mismo banco/seed); gemma4:e4b y gpt-oss:20b
quedan fuera por saturación (94,7% / 98,9%).

## Validación de mecanismo (ANTES de mirar pass rates)

- **Par 5/6**: `compaction_count > 0` en ambos brazos, y comparable
  entre ellos. Si la compactación no disparó, el sweep es
  NO-INFORMATIVO para lead-summary y se declara tal cual (no "nulo").
- **Filas TTC**: tokens/walltime ≈ 3× su baseline (los rollouts
  realmente corrieron); `ttc_rollouts` presente en las filas.

## Criterio pre-registrado

- **TTC — adoptar** (recomendar `ttc` para débiles en el paper) si en
  algún executor: Δ ≥ **+10pp** con IC Newcombe 95% fuera de cero
  (McNemar de confirmación), reportando el costo (~3× tokens) junto al
  delta. **Rechazar** si Δ ≤ 0 en ambos. Entre 0 y +10pp o IC cruzando
  cero: no-adoptado por tamaño, se reporta el estimado.
- **Lead-summary — adoptar** si fila 6 − fila 5 ≥ **+10pp** con IC
  fuera de cero Y el mecanismo validó. **Rechazar** si 6 ≤ 5 con
  mecanismo validado. Sin compactación: NO-INFORMATIVO (ni adoptar ni
  rechazar; la frase del roadmap queda sin soporte y se corrige).
- **Sin iteración pre-declarada** para ninguna de las dos: son palancas
  opcionales cuyo valor se decide en una pasada; si el resultado es
  ambiguo, el veredicto es "no-adoptado" y punto.

## Riesgos anotados

- `tactical-threshold=4` es agresivo y puede degradar AMBOS brazos del
  par 5/6 respecto de la fila 1 — irrelevante para el contraste 6−5,
  que es el único que decide; la comparación 5 vs 1 se reporta como
  contexto del costo de compactar seguido.
- TTC vota por `outcome_fingerprint`: en tareas de texto libre
  (`no_tool`) los fingerprints pueden ser todos distintos y el voto
  degenera en "el primero" — se reporta el desglose por skill.
- El lead (qwen2.5:7b) también atiende escalación reactiva normal en
  las filas 5/6 (es el mismo decorator) — idéntico en ambos brazos, no
  confunde el contraste.

## Costo estimado

Filas 1/3/5/6: ~4×95 corridas normales; filas 2/4: ~2×95×3 rollouts.
En Nitro con estos tamaños: ~2-4 h. Análisis con el mismo esquema del
A/B edit-fence (pareo por tarea/repetición, Newcombe, McNemar exacto).
