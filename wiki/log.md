---
type: log
created: 2026-07-14
---

# Log del wiki de braze

Registro cronológico append-only de este proyecto: decisiones, bugs
resueltos, patrones descubiertos, hitos, cambios de infraestructura.

**Convención de entrada** (greppeable):

```
## [YYYY-MM-DD HH:MM] <op> | <título>
- <detalle opcional>
```

Ops sugeridas: `decision` · `bug` · `patron` · `hito` · `infra` · `nota`

**Escribir SIEMPRE vía** `wiki/_bin/wiki-log.sh <op> "<título>" [detalle]` (usa flock).

Consultas típicas:
- `grep "^## \[" log.md | tail -5` — últimas 5 entradas
- `grep "^## \[.*\] decision" log.md` — todas las decisiones
- `grep "^## \[2026-07" log.md` — todo julio 2026

---
## [2026-07-14 00:45] infra | wiki inicializada
- estructura base creada por /wiki init

## [2026-07-14 00:46] infra | índice de sprawl docs/ agregado
- 42 docs agrupados en 6 categorías en index.md — Auditorías(7), Design docs(6), Sweeps(13), Usability logs(5), Review EMSE(3), Sueltos(8)

## [2026-07-14 01:18] patron | gemma4:e4b vs gpt-oss:20b: thinking vs no-thinking
- Empatados en la suite scripteada (92/95 vs 95/95, delta +3.2pp Newcombe [-1.2,+8.9] cruza cero) pero gpt-oss:20b (thinking) tuvo 2 crashes duros + 1 agotamiento de rondas en sesión larga de playground (U-21/U-22), gemma4:e4b (no-thinking) cero crashes en cientos de corridas acumuladas. Ver docs/usability-log-gptoss20b-playground-2026-07-13.md

## [2026-07-14 01:25] decision | Venue del paper: EMSE (Empirical Software Engineering)
- Elegido sobre TMLR/JAIR 2026-07-12 por requisito de IF JCR. Vault de revisión armado en ~/vault/journals/emse/ (outline, 1000 papers, editorial board 130 miembros, style profile). Skill /paper-review-emse creado.

## [2026-07-14 01:25] decision | Review EMSE del paper: Major Revision, 5 issues críticos
- docs/emse-review-2026-07-13-checklist.md — sin baseline de harness externo, sin baseline solo del lead, pre-registro auto-alojado en git, sin validación del grader, manuscrito incompleto. Aparato empírico (pre-registro, Wilson/Newcombe CIs) calificado como sólido.

## [2026-07-14 01:25] patron | gemma4:e4b solo ≈ compuesto braze ≈ loop bare (composición basta)
- Tres mediciones independientes indistinguibles en 87-91% (n=285 pooled): el lead solo, el compuesto completo de braze, y un loop lead+executor implementado desde cero sin ninguna palanca de braze. La claim 'el harness compensa la escala' se revisó — es mejor leída como 'el harness enruta a la capacidad correcta'. docs/gemma4-e4b-solo-baseline-design.md, docs/external-harness-baseline-design.md, docs/power-increase-2026-07-13.md

## [2026-07-14 01:25] infra | BRAZE_BENCH_KEEP_SESSIONS: preservación de transcripciones + validación del grader
- Flag real (antes parche no commiteado) en crates/braze-bench/src/preserve.rs. Validación: 62/62 (100%) de acuerdo humano-automático en transcripciones muestreadas. docs/grader-validation-2026-07-13.md

## [2026-07-14 16:59] nota | Bonsai 27B (PrismML): tool-calling degrada más que math/código bajo compresión ternaria/1-bit
- Sin Ollama (solo MLX/CUDA, kernels custom), pero sí vía API Together.ai — OpenRouterBackend ya es genérico para cualquier endpoint OpenAI-compatible, no requiere backend nuevo. Agregado a wiki/paginas/modelos-locales-thinking.md


## [2026-07-21 17:30] infra | LocalBackend completo: Harmony + Gemma + stencil GBNF (Fase 3)
- gpt-oss:20b corre por el LocalBackend (plantilla Harmony nativa, parser de canales backend-side, verificado CPU + GPU parcial + TUI con ask_user/multicall/deny). Familia Gemma como tercera plantilla (gemma-4-e4b y 12B verificados). Stencil: envelope qwen + args harmony + gramática derivada del JSON Schema por tool, laziness manual (swap de sampler). Commits d284130..6e65d77.

## [2026-07-21 17:30] patron | Serie "blob de Ollama ≠ GGUF de llama.cpp"
- qwen reusa blobs de Ollama; gpt-oss (arch gptoss, attn_out) y TODA la familia Gemma (gemma3: metadata faltante; gemma4:e4b: 720 vs 2131 tensores, MatFormer del engine propio) requieren GGUF canónico. GGUFs en ~/models/ de ambos nodos: gpt-oss-20b-MXFP4, gemma-4-E4B-QAT, gemma-4-12B-QAT.

## [2026-07-21 17:30] decision | A/B stencil: resultado nulo publicado como tal (3 pasadas)
- Pass rate empatado (41/40, 41/40, 40/40; McNemar p=1.0), sin constraint tax. El retry de validación del engine ya absorbe los schema_fail (todas las corridas con schema_fail pasaron). Hipótesis pre-registrada mal planteada (rescues = extracción normal). El valor demostrado es la garantía por construcción a costo cero, no un delta medible en default.toml/qwen2.5:3b. docs/sweep-stencil-ab-2026-07-21.md

## [2026-07-21 17:30] patron | 3 bugs latentes de Fase 1 solo visibles bajo constrained decoding
- Double-accept del sampler (sample() ya acepta; fatal solo con gramática GBNF), prompt>n_batch=abort C++ del proceso entero, token de control espurio=error duro de stream. Los tres invisibles a smokes y tests; los destapó el A/B en vivo. Refuerza la convención "compilar ≠ funcionar" como método, no eslogan.

## [2026-07-21 21:30] patron | Ranking SML: parámetros activos > parámetros totales en hardware modesto
- gpt-oss:20b (MoE 3.6B activos, CPU) 57/57 pass^3=100% vs gemma-4-12B (denso, GPU 14/48 capas) 30/57 con 26 timeouts PERO 97% condicional — capacidad casi empatada, throughput decide (McNemar p=1.5e-08). El 57/57 es el mejor número del proyecto y la primera suite completa del camino Harmony del LocalBackend. docs/sweep-ranking-12b-vs-gptoss-2026-07-21.md

## [2026-07-22 06:30] patron | Palanca de verificación H2: POSITIVO subpotenciado — la primera de confiabilidad que sube pass rate local
- Gate de fin de turno (corre cargo check, inyecta el fallo, da ronda de arreglo). A/B: qwen2.5:3b 3/18->6/18 (+16pp), gemma4:e4b 12/18->17/18 (+27pp), 0 reversiones, McNemar marginal (gemma p=0.062, n=18). Responde la pregunta profunda del #15: el modelo SÍ actúa sobre el fallo inyectado (gemma recupera 5/6, qwen 3/15 — escala con la capacidad de usar el feedback). Costo <=1.5x rondas. Contrasta con el nulo del stencil. Primer hook H2 (muta el turno). docs/sweep-verification-lever-ab-2026-07-22.md

## [2026-07-22 09:00] patron | Palanca de verificación H2: el piloto POSITIVO era falso positivo — potenciado da NULO (REJECT)
- Piloto n=18: +16/+27pp, 0 reversiones, "prometedor subpotenciado". Potenciado n=60 (20 bugs Rust distintos x3): qwen 17/60 vs 16/60 (p=1.0), gemma 55/60 vs 53/60 (p=0.73) — NULO. El gate recupera tantos fallos como rompe (qwen recupera 9 rompe 10). La ronda extra es arma de doble filo (roam #16). Se une al stencil como nulo, refuerza "not all scaffolding helps". VALIDA no meterla al paper 1 (habría sido falso positivo publicado). docs/sweep-verification-lever-ab-powered-2026-07-22.md
