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

## [2026-08-07 09:40] decision | A/B enable_project_memory: NO se promueve — el control del mismo prompt salvó la conclusión
- seeded−baseline +13 tareas, McNemar p=0.011 (Holm 0.021): el criterio 2 CUMPLIDO en su letra. Pero empty (prompt idéntico a baseline, probado token-a-token en sesiones preservadas) subió +9 solo, y seeded−empty = +4, p=0.541 — el contenido del seed no explica nada. Sin el brazo de control, hoy la palanca se promovía con un falso positivo publicable. Diagnóstico: cero menciones de .braze en trayectorias (hipótesis del filesystem refutada), flips con dirección invertida en re-run → los 21 discordantes del gate eran RUIDO. Hallazgo colateral real: el piso de ruido es POR CONFIG (~20% de celdas en discriminating+KV-host vs ~2% en default.toml) y no transfiere. Regla nueva: control de mismo prompt como piso in-sweep + e-process como gate. docs/hypothesis-2026-08-04-project-memory-ab.md

## [2026-08-07 18:20] infra | Las dos adopciones de las técnicas #1 y #2 pasan de documento a código
- --sequential-stop (e-process + SPRT sobre discordantes de McNemar) y el chequeo de salud de banco (r punto-biserial ítem-vs-total) quedan en crates/braze-bench/src/sequential.rs. Cierra la "deuda inversa": dos adopciones decididas con evidencia que vivían solo en docs. El flag NO tiene default de p1 — lo deriva del umbral pre-registrado del experimento, porque un p1 genérico confunde "sub-umbral" con "efecto cero" (48% de acuerdo en la retrodicción). Asimetría medida al implementar y documentada: el corte ahorra cuando HAY efecto; un nulo corre completo, que es lo correcto. 1.133 tests. cd0ce7b
## [2026-08-10 19:43] hito | Round-economics CERRADA como no-medible — el poder que falta son ítems, no cómputo
- Piloto de costo (5a12ec8): la manipulación de precio de ronda funciona (4,4× vía gpu-layers, cache de modelo arreglado para no invalidarla) pero la interacción queda dentro del ruido. Análisis de poder (c6b8a36): las réplicas NO compran poder — meseta ~25-30% de R=3 a R=20 porque la inferencia es por tarea y la varianza dominante es entre tareas; la palanca real es autorar 150-300 ítems discriminantes con retorno incierto (winner's curse sobre el efecto observado). Gate hacia metaheurísticas CERRADO. Lo que queda en pie: los instrumentos (wall-clock por turno e6f72ba, deadline de streaming por ronda 0bcc9c0, factorial en un sweep 43916b7, banco round-economics-v1 ad16452) y el nulo piloteado. docs/round-economics-pilot-costo-2026-08-08.md + docs/round-economics-power-2026-08-09.md

## [2026-08-10 19:43] hito | Submission EMSE ejecutada — EMSE-S-26-01210 esperando primera decisión
- Los dos bloqueantes del autor se destrabaron; el manuscrito congelado (tag emse-submission-2026-07-29) quedó sometido. Preparación de la revisión ya escrita ANTES de la decisión: docs/emse-revision-taxonomia-fallos-2026-08-08.md (aplicar la taxonomía de Raj et al. a los issues cuando lleguen los reviews).

## [2026-08-10 19:43] infra | v9 saldada COMPLETA: Paquetes 3-4 + seguridad + gate sintáctico
- P1.1 terminado (cluster run_turn_* a turn.rs, 60bf1a7) y local.rs repartido en local/{fit,decode,sampling,family,cache} ANTES de Fase 2 (344e4e3) — la lección de engine.rs aplicada a tiempo. Paquete 4: interlock write_file tras 2 edit_file fallidos + fail-fast de brazo + K-16 negative-cache MCP + AGENTS.md interop (98b4a49), Landlock write-only (3d4c6b3), subagente editor — la mitad escritora del par con explore (393748e). Seguridad: seccomp io_uring/ptrace + hardening + .git/ protegido (f44edf8) y gate sintáctico pre-aplicación — rechaza la edición ANTES de escribir si rompe la sintaxis de un .rs que parseaba (2e9a3e5, ítem Tier-1 #1 del survey).

## [2026-08-10 19:44] patron | Survey de referencia sobre 6 repos: el harness ajeno como cantera de palancas propias
- magnitude, aider, SWE-agent, codex, gemini-cli (+2 papers evaluados). Señales convergentes convertidas en backlog: gate sintáctico (HECHO), sandbox bwrap por-tool (blueprint gemini-cli, clon preservado), carga JIT de AGENTS.md por subdir, truncado de tool-output por presupuesto-inverso con spill, Dynamic Baseline Verification. La señal cross-repo más fuerte (impuesto JSON) se midió el mismo 10-ago — ver entrada siguiente. docs/reference-agents-survey-2026-08-10.md

## [2026-08-10 19:44] patron | A/B edit-fence RECHAZADO: para SLM tool-tuned el JSON no es impuesto — es la lengua materna
- La señal más fuerte del survey (aider midió degradación de código-en-JSON; SWE-agent parsea texto para débiles) NO reproduce en esta población: B≤A en los tres débiles (-4,2/-8,4/-1,1pp), y el mecanismo revela la inversión — llama3.2:1b y qwen2.5:3b emitieron CERO fences SEARCH/REPLACE válidos en 190 corridas (contaminación cero: ninguna edit_file por nombre memorizado) y perdieron exactamente las tareas edit (-6/15 c/u) al quedarse sin su modalidad entrenada. Tercer nulo de la familia sintáctica (constrained decoding, stencil, edit-fence): la reparación río abajo ya cubre la clase. Cadena de pre-registro completa: criterio congelado antes del sweep, análisis commiteado (92850d9) ANTES de existir el JSON. El contraste con aider es condición de contorno (otra población de modelos), no contradicción. docs/sweep-json-tax-edit-fence-2026-08-10.md

## [2026-08-10 19:44] decision | El fallo sistemático de gemma4:e4b era del BANCO — A/B runtime cerrado sin gastar Nitro
- Con digest fijo verificado (c6eb396dbd59) y 95 corridas frescas: read_file_basic falla 5/5 por assertion_tool_call — e4b responde BIEN (3 líneas, text_found=True, schema_fail=0) pero vía shell_exec (wc -l) en vez del read_file exigido. Preferencia estable de política (3/5 sin seed era la misma preferencia con otra suerte), no capacidad: nada que Ollama 0.32.1 pudiera reparar, la hipótesis de CLAUDE.md (e4b salta a pass^5=100%) muere. Arista MODEL-BENCH lado banco (Raj et al.). gpt-oss:20b retiene el default. Decisión de grader (equivalencia funcional vs comparabilidad histórica) documentada como ABIERTA. docs/gemma4-e4b-diagnostico-read-file-basic-2026-08-10.md

## [2026-08-10 19:44] nota | Pre-registro lead-summary + TTC lanzado — las dos últimas palancas v8 sin veredicto
- 6 filas sobre default.toml; lead-summary aislado con tactical-threshold=4 en ambos brazos (smoke: compact=8, el mecanismo dispara); TTC=3 en qwen2.5:3b y llama3.2:1b por headroom (75,8%/25,3% hoy). Higiene: la frase nulo del lead-summary del roadmap 2026-08-06 NO tiene sweep detrás en repo ni Nitro — este experimento la salda. docs/hypothesis-2026-08-10-lead-summary-ttc.md

## [2026-08-10 20:36] infra | Overlay ask_user de la TUI verificado EN VIVO (backlog v8) — y tres lecciones de asserts pty
- pty+pyte contra el binario real con openrouter:deepseek/deepseek-v4-flash: overlay a 3,8s con opciones numeradas generadas por el modelo, selección 1+Enter consumida, elección citada en [MAYÚSCULAS], salida status=0 por waitpid. El guion falló 3 veces por falsos verdes ANTES de pasar de verdad — (1) el eco del composer satisface cualquier palabra tecleada (fix: opciones por paráfrasis, el modelo genera las palabras); (2) v0.1.0 del banner matchea patrones numerados [12][).] (fix: anclar en texto exclusivo del overlay, Enter responder); (3) el overlay pide dígito Y Enter, el dígito solo deja la selección colgada. Regla destilada: cada assert pty necesita un texto que NADIE más pinta. docs/pty-ask-user-verify-2026-08-10.py

## [2026-08-10 20:51] patron | TTC local RECHAZADO: en un modelo débil la votación elige el error estable — confiabilidad NEGATIVA a triple costo
- ttc=3 con auto-consistencia: qwen2.5:3b Δ=0,0pp exacto (72/95 ambos brazos); llama3.2:1b Δ=-8,4pp con los 8 discordantes favoreciendo TODOS al baseline (McNemar exacto p=0,0078). Mecanismo: el voto por outcome_fingerprint premia el modo de la distribución, y el modo de un débil es un error ESTABLE (outputs degenerados que coinciden entre sí) que le gana al intento único correcto. Comprar cómputo con votación exige que el acierto sea más consistente que el error — en los débiles es al revés. Mecanismo validado (rollouts 95/95, 2,95x/6,59x tokens). docs/sweep-lead-summary-ttc-2026-08-10.md

## [2026-08-10 20:51] decision | Lead-summary NO-ADOPTADO por tamaño — pero con la señal direccional más limpia del día (6/0 discordantes)
- fila 6 vs fila 5 (misma presión de compactación thr=4, solo cambia la fuente del summary): +6,3pp, IC Newcombe cruza cero -> no llega al criterio; pero los 6 discordantes favorecen TODOS al summary-por-lead (p=0,031). Cuando la compactación pierde algo que importaba, el lead lo pierde menos que el digest. Revisita posible: banco con más presión de compactación (aquí solo 25/95 tareas compactaron). Corrige de paso la frase huérfana del roadmap 2026-08-06 (nulo del lead-summary sin sweep detrás). Con esto TODAS las palancas implementadas tienen veredicto medido — el inventario de ablaciones del paper de seguimiento no tiene casillas vacías. docs/sweep-lead-summary-ttc-2026-08-10.md

## [2026-08-10 20:51] bug | OOM-kill de Ollama a mitad de sweep multi-modelo — el breaker salvó la estadística, la regla operativa cambió
- 20:03, journalctl: kernel OOM-kill del servicio con --no-ollama-stop apilando modelos en los 14Gi de Nitro. El circuit breaker abrió contra qwen2.5:7b tras 5 fallos de transporte y clasificó el resto HarnessError FUERA del denominador — el diseño v8 pagó exactamente como se diseñó. Regla nueva en CLAUDE.md: --no-ollama-stop SOLO sweeps de modelo único. Hardening preparado: nitro:~/nitro-ollama-hardening.sh (KEEP_ALIVE=2m, MAX_LOADED_MODELS=2), aplicar con Nitro ocioso. Re-run con seed pareado fusionó limpio (el pareo por repetición sobrevive cortes por diseño de --seed).

