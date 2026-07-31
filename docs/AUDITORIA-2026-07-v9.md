# Auditoría 2026-07 v9 — post-submission-freeze: evidencia, config y el repo a punto de hacerse público

Fecha: 2026-07-31. Sucesora de `docs/AUDITORIA-2026-07-v8.md` (2026-07-18).
Alcance: los **145 commits** entre ambas (arco LocalBackend completo, palanca
de verificación H2 medida y rechazada, suite discriminante v2, piso de ruido,
los cinco arreglos del 28-jul, el hallazgo de emisión de caracteres, y el
congelamiento del manuscrito para EMSE con su tag `emse-submission-2026-07-29`).
Método: verificación completa del workspace corrida hoy + barrido dirigido de
los subsistemas nuevos + cruce de docs rastreados contra el árbol real.

## Veredicto ejecutivo

**El código está sano; el riesgo de esta ronda está alrededor del código.**
`cargo build/test/clippy --workspace` verdes hoy: **1.116 tests, 0 fallos,
clippy `-D warnings` limpio**. Ninguno de los hallazgos de esta auditoría es
un bug de comportamiento. Son tres clases de deuda que la inminencia de dos
eventos —el repo haciéndose público y el manuscrito entrando a review—
convierte de cosmética en operativa:

1. **Evidencia no rastreada** (L-2): la base completa del Paper 2 (28
   archivos, incluida una síntesis CERRADA de 140 corridas) no está en git,
   y docs que SÍ están rastreados la citan → referencias colgantes en el
   repo público.
2. **Un universo de configuración paralelo** (L-1): el LocalBackend creció
   21 variables `BRAZE_LOCAL_*` que no existen para el sistema de config
   del proyecto — la reincidencia sistémica del H-9 de v5, a 21× la escala.
3. **Higiene pre-publicación** (L-3): 4.605 archivos sin rastrear que el
   `.gitignore` no cubre, incluidos artefactos que no deberían poder
   agregarse por accidente.

De los pendientes de v8 quedan abiertos, sin cambios: Landlock write-only,
subagente Viewer/Editor, K-16 (negative-cache MCP), AGENTS.md interop y
P0.2 (costo USD/walltime por turno). Ninguno bloquea la submission.

## 1. Verificación del workspace (2026-07-31)

```
cargo build --workspace   OK (15.9s incremental)
cargo test  --workspace   1116 passed, 0 failed
cargo clippy -D warnings  0 issues
```

Los cinco arreglos del 28-jul están en main y el principal
(`first_divergence`) tiene verificación en vivo documentada: 20 rondas/25m de
deadlock → 4 rondas/7m31s de rechazo honesto, sin daño colateral
(`docs/roam-metrics-memoria-2026-07-28.md` § 8).

## 2. Cierre del roadmap v8

| Ítem v8 | Estado |
|---|---|
| Paquetes 0–3 completos + top-6 S/M del Paquete 4 | hecho (mismo 18-jul, ya anotado en v8) |
| pass^k, prompt caching Anthropic, summary-por-lead, TTC, K-19 McNemar/Holm | hechos |
| P1.1 split de engine.rs | **parcial** — ver L-5 |
| 16. Landlock write-only (M) | **abierto** — sigue siendo un comentario (`braze-permissions/src/lib.rs:5`) |
| 17. Subagente Viewer/Editor (L) | abierto |
| K-16 negative-cache MCP | abierto, sin rastro en código |
| AGENTS.md interop | abierto, sin rastro |
| P0.2 costo USD/walltime por turno | abierto |
| Gemma4: Ollama ≥0.32.1 en Nitro | hecho (20-jul); el A/B de runtime con digest fijo sigue pendiente |

Además, dos líneas se **midieron y cerraron** después de v8, ambas con
resultado nulo/negativo bien documentado — exactamente lo que la disciplina
pide: la palanca de verificación H2 (piloto positivo era falso positivo;
potenciado n=60 NULO, REJECT, wiki 22-jul) y el A/B del stencil (empate en
tres pasadas, sin constraint tax, ya en el paper como replicación in-process).

## 3. Hallazgos nuevos — serie L

### L-1 (M, sistémico) — 21 knobs `BRAZE_LOCAL_*` fuera del sistema de config

El proyecto tiene un sistema de configuración por capas (defaults → archivo →
env → CLI) con una lista canónica de claves (`braze-config/src/file.rs::KNOWN_OVERRIDE_KEYS`).
El LocalBackend creció **21 variables de entorno** (`BRAZE_LOCAL_GPU_LAYERS`,
`_KV_TYPE`, `_TEMP`, `_SEED`, `_FAMILY`, `_GRAMMAR`, la familia DRY completa,
etc.) de las cuales **cero** existen en ese sistema — más `BRAZE_VERIFY_COMMAND`
(`braze-cli/src/main.rs:780`), env-only también.

Consecuencias concretas: (a) no se pueden fijar en `config.json`, así que
toda receta de LocalBackend es una ristra de exports (los scripts de Nitro lo
muestran); (b) el bench no puede ablacionarlas por fila — la maquinaria
`+ablate:` opera sobre config, no sobre env; (c) el warning de "clave
desconocida" del archivo de config no las conoce, o sea el H-9 de v5
(claves que aplican en silencio) reaparece con 21 claves nuevas. La v5 arregló
5 claves; esto es lo mismo a 4× la escala, y creció en 10 días.

**Propuesta**: decidir explícitamente. O se promueven a `ConfigOverrides` +
`file.rs` (S/M, mecánico, el patrón existe), o se documenta que la familia
`BRAZE_LOCAL_*` es una capa deliberadamente env-only de tuning de despliegue
— pero entonces el doc de config debe decirlo y el bench debe poder
registrarlas en `metadata` del sweep para procedencia.

### L-2 (M, evidencia) — la base del Paper 2 no está en git

`docs/` tiene **28 archivos** de la familia memory-distillation —
`hypothesis-2026-07-16-memory-distillation.md`,
`paper2-memory-distillation-protocol-2026-07-16.md`,
`decision-memory-distillation-pilot-2026-07-16.md`,
`sweep-memory-distillation-3taskB-synthesis-2026-07-17.md` (estado CERRADO,
140 corridas) y ~24 JSON de sweeps — y **cero están rastreados**. Esa línea
tiene un resultado real (la condición de amortización identificada: el
playbook solo paga en la tarea memorizada) que hoy está a un `rm -rf` de
desaparecer, y que el framework de disciplina —que SÍ está rastreado desde
ayer— cita textualmente.

Referencias colgantes confirmadas (doc rastreado → archivo ausente del repo):

| Citado por (rastreado) | Colgante |
|---|---|
| `future-research-lines-2026-07-16.md` | `hypothesis-2026-07-16-memory-distillation.md` |
| `future-research-lines-2026-07-16.md` | `paper2-memory-distillation-protocol-2026-07-16.md` |
| `research-discipline-framework-2026-07-16.md` | `sweep-memory-distillation-3taskB-synthesis-2026-07-17.md` |
| `curve-transport-audit-2026-07-18.md` | `empty-response-discriminant-design-2026-07-18.md` |
| `inference-runtimes-audit-2026-07-25.md` | `autoparser.md` |
| `explorador-aislado-ab-design.md` | `harness-engineering-hooks-skills-2026-07-10.md` |

Nota agravante: dos de esas referencias las introduje **yo, ayer**, al
commitear los tres docs del framework sin commitear los documentos a los que
apuntan. En un repo a punto de hacerse público, cada colgante es un lector
externo encontrando un 404 interno.

**Propuesta**: commitear la familia completa (los JSON pesan poco y son
evidencia primaria), más los 4 colgantes restantes tras triage individual. La
alternativa —decidir que el Paper 2 no se publica todavía— es legítima, pero
entonces hay que quitar las citas de los docs rastreados, no dejarlas rotas.

### L-3 (S, pre-publicación) — 4.605 archivos sin rastrear que el `.gitignore` no cubre

El `.gitignore` actual no menciona: `braze-bench-preserved-sessions/`
(~4.500 archivos: sandboxes y rollouts JSONL de sesiones preservadas del
bench, con rutas absolutas y username), `.Rhistory` (raíz y `docs/`), los PDF
de terceros en la **raíz** (`235_Position_LLMs_can_t_jump.pdf` — el patrón
existente `docs/*.pdf` no cubre la raíz; es un paper de ICML con copyright,
no debe entrar al repo), y los sueltos de la raíz (`prueba.md`, `ejemplo.md`,
`hardware_report.md`, `SPEC_BRAZE_SURTGIS.md`, `outline.md`).

Nada de esto se publica al hacer el repo público (untracked no viaja), pero:
(a) el ruido de `git status` ya enterró señales reales esta semana — L-2 pasó
inadvertido en parte por esto; (b) un `git add -A` descuidado a partir de
mañana publica rollouts de sesiones con rutas locales; (c) `SPEC_BRAZE_SURTGIS.md`
en particular hay que triagearlo — puede pertenecer al otro proyecto.

**Propuesta**: bloque nuevo en `.gitignore` (`braze-bench-preserved-sessions/`,
`.Rhistory`, `/*.pdf`) + triage de los 5 sueltos de raíz (commitear, mover o
borrar, uno por uno). 15 minutos, y elimina la clase de accidente.

### L-4 (S, deuda) — `local.rs` es el nuevo `engine.rs`

2.046 líneas, 48 funciones, tests inline. Ya se le extrajeron `harmony.rs`,
`gemma.rs` y `stencil.rs`, pero el cuerpo (auto-fit, caché de modelo,
sampling, KV placement, loop de decode, familia de plantillas) sigue
monolítico y creció ~1.300 líneas en dos semanas — la misma curva que
`engine.rs` recorrió antes de necesitar el P1.1. No urge (los tests pasan y
las costuras son visibles), pero conviene cortarlo **antes** de las features
de Fase 2, que es exactamente la lección que v7/v8 dejaron con engine.

### L-5 (S, P1.1 resto) — `engine/mod.rs` sigue en 6.796 líneas

El reparto del `mod tests` quedó a medias desde el 21-jul: fixtures y cuatro
clusters ya viven en sus módulos, pero el cluster grande
(`run_turn_*`/summary-round, ~50 tests) sigue en `mod.rs` (tests desde la
línea 668). Es la última pieza del P1.1 y es mecánica.

### L-6 (S, docs) — CLAUDE.md y wiki corren dos semanas detrás del proyecto

El encabezado de CLAUDE.md dice "Estado (2026-07-18)". Desde entonces:
LocalBackend Fases 1-3 + KV-quant, la suite discriminante v2 (34 tareas), el
piso de ruido, el REJECT de la palanca H2, los cinco arreglos, el hallazgo de
emisión de caracteres, la línea round-economics, y **la submission a EMSE con
su tag**. Nada de eso está en el doc que un colaborador (o yo, tras un
compact) lee primero. La wiki tiene su última entrada el 22-jul. El proyecto
documenta obsesivamente en `docs/` pero sus dos índices están desactualizados.

### L-7 (S, procedencia) — 7 sweeps en formato viejo, sin `metadata.sampling`

`sweep-planner-ab.json` (citado por el paper), `sweep-deepseek-v4-flash.json`
(citado por CLAUDE.md), los 4 de `sweep-nitro-sampling-2026-07-06/` y un
offline-grades del ancla BFCL son **arrays JSON válidos** del formato
pre-metadata: el sampling con que corrieron no está embebido en el archivo
(está documentado fuera de banda: 0.2, el default del bench de esa época).
Los 38 sweeps del formato nuevo registran `temperature=0.2, seed=None` —
repeticiones genuinamente independientes, verificado hoy contra la sospecha
de determinismo (ver L-9). Fix barato: nota de procedencia en el apéndice del
paper o un sidecar `.provenance.md`; no vale la pena tocar los JSON.

### L-8 (S, memoria) — dos mentiras de doc conocidas desde el 28-jul, sin arreglar

`TouchedFile::at` se documenta *"RFC3339-ish timestamp string"*
(`braze-memory/src/memory.rs:25`) pero su único productor escribe epoch crudo
(`project_memory_hook.rs::now`). `MemoryMeta::updated_at` queda `null` para
siempre — nada lo escribe. Ambos anotados en
`docs/roam-metrics-memoria-2026-07-28.md` § 6. Arreglo de 10 minutos: o
producir RFC3339 de verdad, o corregir los dos doc comments y poblar
`updated_at` en `save`.

### L-9 (nota, determinismo) — `braze run` + greedy = réplicas que son copias

Medido el 29-jul en el piloto de roam: con el default greedy del LocalBackend
y estado inicial idéntico, tres "réplicas" dieron 36/36/36 rondas y 9s de
dispersión — n=1 efectivo. **El bench NO está afectado** (verificado: sus
sweeps corren a temp 0.2, y hasta los sweeps greedy del stencil varían entre
repeticiones porque el directorio temporal único entra al prompt). Pero
cualquier A/B artesanal montado sobre `braze run` repetido cae en la trampa,
y no hay nada que avise. Candidato barato: una línea en el doc operativo
(`operar-gpt-oss-2026-07-28.md`) y/o un aviso del bench si detecta
temperatura 0 con repeticiones >1 sobre estado idéntico.

### L-10 (S, harness) — el interlock duro de `write_file` sigue diferido

La redacción de la guarda ya depende del tamaño del archivo (28-jul), y en la
verificación en vivo el modelo se detuvo solo. Pero el corte duro —bloquear
`write_file` sobre un archivo que acaba de fallar `edit_file` dos veces—
necesita estado por turno en el engine y sigue sin existir. La rama de daño
está desincentivada, no cerrada.

### L-11 (nota, bench) — fail-fast de brazo, pendiente desde el 21-jul

57 fallos instantáneos de carga de modelo siguen quemando un brazo entero en
silencio. Sin rastro en `runner.rs`. Subió de prioridad cuando los binarios
desincronizados de Nitro lo provocaron dos veces; sigue igual.

## 4. Estado de las líneas de investigación

Sincronizado ayer en `docs/research-discipline-framework-2026-07-16.md`
(tabla + ordenamiento round-economics → metaheurísticas con gate explícito).
No se repite aquí. Lo único que esta auditoría agrega: el piloto de contexto
de round-economics tiene el defecto L-9 (réplicas-copia) y **no debe
interpretarse** hasta re-correrse con `BRAZE_LOCAL_TEMP>0` y semillas.

## 5. Roadmap v9 — priorizado

**Paquete 0 — antes de (o junto con) hacer público el repo:**
1. L-3: `.gitignore` + triage de la raíz (15 min).
2. L-2: commitear la familia memory-distillation + los 4 colgantes con
   triage — o quitar las citas. Decisión, no solo mecánica.
3. Tras el flip a público: verificar 200 anónimo del tag y de un JSON de
   sweep (ya acordado).

**Paquete 1 — integridad de la submission (esta semana):**
4. L-7: nota de procedencia para los 7 sweeps pre-metadata.
5. Los dos bloqueantes ya conocidos (visibilidad + IDs OSF) y el retiro del
   último `\todo` + `\newcommand{\todo}` al cerrarlos.

**Paquete 2 — config e higiene de código (S/M):**
6. L-1: decidir y ejecutar (promover al config file, o documentar la capa
   env-only + registrarla en metadata del bench).
7. L-8: los dos fixes de braze-memory.

**Paquete 3 — deuda estructural (después de lo anterior):**
8. L-5: terminar P1.1 (cluster `run_turn_*` → `turn.rs`).
9. L-4: split de `local.rs` antes de cualquier feature nueva del backend.

**Paquete 4 — harness (hereda de v8, orden sin cambios):**
10. L-10 interlock duro; L-11 fail-fast de brazo; K-16; AGENTS.md interop;
    Landlock write-only (M); Viewer/Editor (L).

## Cierre

La v8 cerró con el proyecto convertido en un instrumento de medición
disciplinado; los 145 commits desde entonces lo usaron para producir
resultados — incluidos dos nulos bien muertos (H2, stencil) y un hallazgo
nuevo con verificación en vivo (emisión de caracteres). Esta v9 no encontró
nada roto en el código. Encontró que el **perímetro** del proyecto —qué está
en git, qué conoce el sistema de config, qué dicen los índices— quedó dos
semanas detrás del contenido, justo cuando el perímetro está a punto de
volverse la cara pública. Los Paquetes 0-1 son un día de trabajo y deberían
preceder al flip de visibilidad; el resto es la cadencia normal.
