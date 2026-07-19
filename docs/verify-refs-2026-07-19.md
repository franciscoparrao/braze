# verify-refs — pase delta 2026-07-19 (post-integración § 5.3 BFCL)

Delta sobre el pase completo de `docs/verify-refs-2026-07-13.md` (+
entradas nuevas verificadas 2026-07-18, ver cabecera de
`paper/refs.bib`). Motivo: la integración del ancla BFCL agregó texto
que cita `bfcl` con claims nuevos; ninguna entrada nueva entró al .bib.

## Cross-check mecánico tex ↔ bib [VERIFICADO]

15 keys citadas = 15 entradas en el .bib, 0 huérfanas en ambas
direcciones. Único comando de cita: `\citep` (19 usos). Carga máxima:
`yang2024sweagent` (3), `pi-dev`/`goose`/`bfcl` (2 c/u).

## Nivel 1 — existencia [VERIFICADO]

CLI: 8/15 VERIFIED directos. Los 7 NOT_FOUND del CLI, resueltos
manualmente — **ninguno es alucinación**:

- `liu2025tts`, `lu2024smalllm`, `jimenez2024swebench`: los 400 de
  OpenAlex son un artefacto del tool (`title.search` con `?`/`:` en el
  título). Los 3 DOIs resuelven en **DataCite con título EXACTO** al
  del .bib.
- `goose`, `aider`, `pi-dev`, `bfcl`: recursos web (documentado en la
  cabecera del .bib) — las 4 URLs responden **HTTP 200**.

## Nivel 2 — retracciones [VERIFICADO]

`is_retracted = false` en los 10 DOIs del .bib (OpenAlex, 2026-07-19).
Persiste la anomalía documentada: el registro OpenAlex del DOI de
SWE-bench devuelve el título de OTRO trabajo (error del lado de
OpenAlex; DataCite — registrador autoritativo de DOIs arXiv — devuelve
el título correcto).

## Nivel 3 — claims nuevos de § 5.3 (incremental)

- **"consistent with BFCL's own recent repositioning of this category
  as a hallucination measurement"** → [VERIFICADO contra fuente
  primaria, no solo abstract]: el blog BFCL v4
  (`blogs/15_bfcl_v4_web_search.html`) declara "Hallucination
  Measurement (10%)" como categoría ponderada del score general
  ("Overall Score = Agentic × 40% + Multi-Turn × 30% + Live × 10% +
  Non-Live × 10% + Hallucination Measurement × 10%"), con 1.122 ítems.
  Confirmación aritmética de que la categoría ES irrelevance:
  `BFCL_v4_irrelevance.json` (~240) + `BFCL_v4_live_irrelevance.json`
  (~884) ≈ 1.122–1.124 (jsonl ±1), con relevance (~18) listado como
  columna aparte.
- Los demás claims de § 5.3 citan datos propios
  (`docs/sweep-bfcl-anchor-2026-07-18.md`), no literatura — fuera del
  alcance de este skill.

## Veredicto

.bib apto para envío: 15/15 existen, 0 retracciones, 0 huérfanas, y el
claim de carga nueva sobre BFCL v4 verificado contra el blog oficial +
los datasets del repo.
