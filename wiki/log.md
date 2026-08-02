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

## [2026-08-01 22:38] patron | Hay caracteres que el modelo entiende y NO PUEDE EMITIR — regiones estructuralmente ineditables
- gpt-oss:20b no puede escribir U+1D62 (subíndice i): lo borró 2 de 2 veces copiando bloques (216 y 89 líneas, con el prompt ADVIRTIENDO sobre subíndices), dejando una fórmula matemáticamente falsa que pasa compilación+tests+post-edit check (doc comments son invisibles a los tres). Consecuencia de 2º orden peor: edit_file matchea texto exacto → una región con ese carácter es INEDITABLE por el agente (old_string de 91 líneas perfecto salvo esa, 6 intentos, 20 rondas, 25m, cero avance). Misma clase: ≈ (U+2248) y comillas anidadas en format!. No es tamaño del bloque — es la clase de carácter. Tercera replicación CON el carácter nombrado delante (post-fix): siguió sin poder. Es brecha de capacidad, no descuido; la taxonomía inducción/deducción/abducción no tiene casillero para fallas MOTORAS. docs/roam-metrics-memoria-2026-07-28.md § 7

## [2026-08-01 22:38] infra | Cinco arreglos de harness salidos del hallazgo (b1325fa), el principal verificado en vivo
- (1) edit_file reporta la PRIMERA DIVERGENCIA con codepoint en ambos lados + ambas líneas — reproducción exacta del caso real: 20 rondas/25m08s de deadlock ciego → 4 rondas/7m31s de rechazo honesto sin daño. (2) Guarda de write_file condicionada al tamaño (<=120 líneas ofrece reescritura; encima la prohíbe con el porqué — el modelo citaba la redacción vieja para justificar la reescritura de 268 líneas que desactivó tolerancias de tests en silencio). (3) braze run ya no se cuelga en prompt de permiso sin TTY (chequeo IsTerminal, deniega y lo dice — se pagó solo el mismo día salvando el piloto). (4) search→grep por tabla de sinónimos (7 de 11 alucinaciones de tool en 12 sesiones; revierte un test que decía lo contrario, anotado en el propio test). (5) Canal Harmony desconocido: trazado, no cambiado (invertir el default = turnos mudos, peor que la filtración).

## [2026-08-01 22:38] decision | Umbral de trayectoria ~6 rondas NO replica entre modelos — línea cerrada
- Lo que valía para gpt-oss no generalizó (2bae196, docs/umbral-trayectoria-refutado-2026-07-28.md). Se giró a lo operativo: doc de cómo trabajar con gpt-oss (docs/operar-gpt-oss-2026-07-28.md, md+html) con la receta de subdivisión corregida (3 pasos → 2: visibilidad plegada al paso 1; bloque exacto embebido en el prompt para borrados — 20 rondas → 2m00s). Memoria de proyecto verificada end-to-end con probe sin tools (enable_project_memory inyecta y el modelo la lee); sigue OFF por default, su A/B no corrió.

## [2026-08-01 22:38] hito | Manuscrito CONGELADO para EMSE — paquete listo, dos bloqueantes del autor
- /paper-match confirmó EMSE STRONG FIT 9/9 (ML fit 29,5% vs 6,0% del 2º; el backend tuvo que reentrenarse — emse NO ESTABA en el modelo de 38 clases, reportar esa tabla habría sido recomendación falsa). Data availability agregada (era política del journal y faltaba), anclada al tag emse-submission-2026-07-29. Paquete plano compilado aislado: 40 pág, 0 errores (el 1er intento falló por captions .tex sin copiar — la clase exacta de error que el portal advierte). Bloqueantes: repo PRIVADO con declaración que dice "openly available" (detectado con gh repo view ANTES de subir; el comando del flip quedó en el portapapeles del autor) + IDs OSF (formularios listos, /tmp/osf/). El \todo rojo restante es salvaguarda deliberada, no olvido. Pre-registro nuevo: round-economics (hypothesis-2026-07-28) con sub-hipótesis A (plan de 4 fases de wsff.md como palanca de contexto que funcione) y gate explícito hacia metaheurísticas.

## [2026-08-01 22:38] hito | Auditoría v9 + Paquetes 0-2 ejecutados: el riesgo estaba en el perímetro, no en el código
- v9 (db774a3): 1.117 tests verdes, clippy limpio, CERO bugs de comportamiento en 145 commits — pero la base del Paper 2 (28 archivos memory-distillation, síntesis CERRADA de 140 corridas) NO estaba en git con docs rastreados citándola (6 colgantes, 2 míos), 21 env vars BRAZE_LOCAL_* fuera del sistema de config, y 4.605 untracked sin cubrir. Paquete 0: todo al repo + gitignore + triage (git status 4.605 → 10 deliberados); 2 "colgantes" eran citas defectuosas (autoparser.md era de LLAMA.CPP; empty-response-discriminant era nombre viejo). Paquete 1 (L-7): procedencia de 7 sweeps pre-metadata verificada POR COMMIT — el sweep deepseek corrió con sampling del PROVEEDOR, no 0.2 (el bench no tenía flags hasta el 06-jul). Paquete 2: updated_at se estampa en el saver; decisión formal L-1 — BRAZE_LOCAL_* queda env-only DECLARADA y cada sweep registra el tier en metadata.local_env (test: una API key jamás viaja al JSON). Réplicas-copia (L-9): greedy+estado idéntico = n=1 efectivo; el bench NO afectado (verificado: hasta los sweeps greedy del stencil varían — 41 no es múltiplo de 3).
