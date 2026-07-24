# braze — motor agéntico genérico en Rust (experimento)

> **Estado (2026-07-18):** MVP completo, reorientado a **"maestro en
> modelos pequeños"** — el harness como variable que compensa la escala
> del modelo (respaldo: SWE-agent/ACI y el TR de Qwen3-Coder-Next, ver
> `docs/SOTA-2026-07.md`; el encuadre ya tiene nombre de disciplina:
> "harness engineering", delta de literatura en
> `docs/AUDITORIA-2026-07-v8.md` § 6). Fases 1-5 + TUI (2 fases) +
> auditorías **v1-v8** (v8 ejecutó en el día sus Paquetes 0-3 completos
> y el top-6 S/M del Paquete 4; abiertos vigentes en
> `docs/AUDITORIA-2026-07-v8.md`); split planificador/ejecutor rescatado
> bajo pre-registro (`docs/sweep-planlead-2026-07-11.md`); **P1.1
> ejecutado**: `engine.rs` (11.4k líneas) partido en 9 módulos
> (`engine/` + `rescue.rs`), queda solo repartir su `mod tests`;
> evidencia experimental del paper completa, manuscrito en `paper/`, y
> el ancla BFCL corrida el 2026-07-18 (análisis en curso); y el
> **LocalBackend completo** (2026-07-20/21: inferencia in-process
> llama.cpp, 3 familias de plantilla, stencil GBNF — ver su § abajo;
> gpt-oss:20b por el camino Harmony logró 57/57, el mejor número del
> proyecto). Creado 2026-07-03. Ver `PLAN.md` para la arquitectura y
> verificación por incremento.

**Wiki de referencia**: ver `wiki/index.md`

## Qué es

Un CLI agéntico genérico ("coding agent" al estilo OpenAI Codex / Claude
Code) construido desde cero en Rust, como **ejercicio de experimentación**
explícito — no es un compromiso de producto. El alcance del MVP favorece un
sistema pequeño y funcional sobre cobertura exhaustiva.

## El gap que llena

No es un fork de `openai/codex` ni de `google-gemini/gemini-cli`. Es un
diseño híbrido: la infraestructura que Codex ya resuelve bien (sandboxing,
cliente MCP, abstracción de proveedor de modelo) combinada con principios de
cómo opera Claude Code que no están explícitos en la estructura de crates de
Codex — carga diferida de herramientas (solo nombres en contexto, schema
completo bajo demanda), compactación diferencial de contexto (estado
durable vs. ventana táctica), ejecución en background con notificación push
(no polling), y un modelo de permisos de dos capas.

## Alcance MVP

Ver `PLAN.md` § "Alcance del MVP" para el detalle completo. Resumen:
loop de tool-calling funcionando contra **Anthropic** (proveedor
principal), **Ollama local** (segundo implementador de `ModelBackend`,
gratis) y **OpenRouter** (tercer implementador, agregado 2026-07-05 —
API OpenAI-compatible, da acceso a modelos de terceros vía una única
cuenta/key); herramientas locales + cliente MCP implementando el mismo
trait `ToolProvider`; carga diferida de herramientas end-to-end;
persistencia de sesión a disco con compactación diferencial simple; capa
mínima de confirmación de permisos (sin sandboxing de SO todavía).

Diferido a Fase 2: sandboxing Landlock/seccomp, multi-agente/grafo de
threads, observabilidad OTLP, paquetes de skills cargables, sistema de
hooks plugueable.

**TUI (`braze-tui`)**: completa con fases 2 y 3 incluidas (fase 3
"profesional", 2026-07-19 — ver PLAN.md § "Fase TUI 3") — `braze chat
--tui`, opt-in (promoverla a default sigue diferido). Viewport inline +
scrollback nativo, streaming markdown con gateo por fence, tool-call
cells, approval overlay, slash commands con popup (`/help`, `/model`,
`/skills`, `/permissions`, `/tasks`, `/quit`), @-menciones, `$skill`
picker, Ctrl+T (output completo de la última tool call), backtrack
Esc-Esc, temas (dark/light/high-contrast, ahora con **color `accent`**
de identidad: banner, marcador `>`, borde del composer, spinner y
nombres en popups; overlays de decisión en warning; **takeover de
pantalla al abrir** — banner arriba, composer al fondo, sin alternate
screen), **`/model` picker** para cambiar de backend/modelo a mitad de sesión, **`ask_user`
nativo**
(overlay de opciones 1-4/flechas; Esc = sin respuesta), celdas
`HarnessNote` (J-26) y `◈ skill cargada`, y barra de estado rica
(skills cargadas + tokens, con degradación `fit_right` en terminal
angosta). J-12 cerrado en el engine: las skills cargadas se rehidratan
del rollout log en `--resume` y tras el rebuild de `/model`.

**Palancas de confiabilidad para modelos chicos** (backlog 1-7,
2026-07-06 — cada una con su sección en PLAN.md): rescate textual de
tool calls por familia (`<tool_call>{json}` de qwen2.5, gramática XML
`<function=...>` de qwen3-coder, JSON desnudo); colapso ACI de
observaciones viejas a 1 línea salvo las últimas 5; guardrail `cargo
check` post-edit (errores de compilación de vuelta al modelo en el
mismo tool result; opt-out `disable_post_edit_check`); escalación
reactiva lead/worker estilo Goose (`--lead <backend>[:modelo]`,
decorator `EscalatingBackend` — compone con `--planner`); knobs de
sampling Ollama (`--top-p/--top-k/--repeat-penalty` en braze-bench);
hardening del ensamblaje de tool calls en streaming (escalera de
reparación de args compartida, remap de colisiones index/id, fragmentos
sin index — los wires nunca dropean una call en silencio); y
`--ollama-url` en chat/run para apuntar a un nodo de inferencia LAN.

## Arquitectura

Workspace de 15 crates (`crates/braze-*`, incluye `braze-tui`,
`braze-bench`, `braze-skills` — D′ del estudio consolidado: skills
locales explicit-only — y `braze-memory`, la memoria de proyecto
cross-sesión del Paper 2, agregado 2026-07-16), grafo de
dependencias en niveles — ver `PLAN.md` para el diagrama y las firmas de
los tres traits que actúan de contrato congelado: `ToolProvider`
(`braze-tools-core`), `ModelBackend` (`braze-model`),
`SessionStore`/`ContextCompactor` (`braze-session`).

**Desviaciones deliberadas de la convención del resto del ecosistema Rust
del autor** (datacube-rs, geostat-rs, swarm-abm son 100% sync + rayon):
- `tokio` en todo el workspace (dominio de I/O concurrente: streaming del
  modelo, cliente MCP, tareas en background).
- `tracing`/`tracing-subscriber` en los crates del loop agéntico (el modo
  de falla central — "la secuencia de tool-calls hizo algo inesperado" —
  es invisible sin trazas estructuradas, a diferencia de las librerías
  numéricas donde la corrección se valida con tests).

Todo lo demás sigue la convención habitual: `thiserror` v2, `clap` v4,
`serde`+`serde_json`, licencia dual `MIT OR Apache-2.0`, archivo-por-módulo.

## Estado del código (2026-07-18)

Los 15 crates tienen lógica real y verificada. `cargo build/test/clippy
--workspace` verdes: **~1.000 tests**, clippy `-D warnings` limpio.
**P1.1 ejecutado** (4 commits, 2026-07-18): `engine.rs` vive ahora en
`engine/` (mod.rs solo struct+builders+tests, más `context`, `turn`,
`round`, `dispatch`, `planner`, `fallback`, `hooks_dispatch`) y la
escalera de rescate en `src/rescue.rs` — extracción verbatim, tests
verdes tras cada paso. Reparto del `mod tests` en curso (pasos 4-7,
2026-07-21): fixtures compartidas en `engine/test_support.rs` y los
clusters de context/planner/compactación/best-of-n/dispatch ya en sus
módulos (`mod.rs` 8.704→6.680 líneas); queda el cluster grande de
`run_turn_*`/summary-round (~50 tests) → `turn.rs`. J-20 (symlinks): aceptación MVP
**ratificada** por el autor el 2026-07-18. La
convención de verificación del proyecto: cada incremento se prueba
también **en vivo** (pty scripteado contra el binario real para la TUI,
sweeps de braze-bench contra modelos reales, smoke contra APIs reales) —
compilar ≠ funcionar. Técnica pty reusable: responder `ESC[6n`,
`wait_for` con offsets, asserts sobre celdas commiteadas al scrollback
(no sobre el render diffeado), y detectar salida con `waitpid` (no
`kill(pid, 0)` — el zombie responde como vivo).

## Benchmarking: en Nitro, no en la máquina de trabajo

Regla operativa (2026-07-06, evidencia en PLAN.md): los sweeps de
`braze-bench` corren contra el nodo LAN **Nitro**
(`BRAZE_OLLAMA_BASE_URL=http://192.168.1.8:11434` + `--no-ollama-stop`;
la IP es DHCP — fijarla en el router sigue pendiente). Benchear en la
máquina de trabajo con builds/tests concurrentes contamina los números
(misma config+seed: 2/6 vs 0/6), y Nitro corre qwen2.5:3b ~50× más
rápido (~2s vs ~90-100s por tarea). `RUST_LOG=braze_engine=info` sobre
un sweep muestra las activaciones de palancas del engine (rescates,
compactación) — braze-bench instala subscriber de tracing.

## Palancas nuevas del 2026-07-18 (auditoría v8, ver su § por detalle)

- **Circuit breaker por destino+modelo** en braze-model: fallos de
  transporte consecutivos abren el breaker (4xx/429/Decode neutrales,
  stream-aware, probe 600s); kill-switch `BRAZE_CIRCUIT_BREAKER=off`;
  `CircuitOpen` clasifica `HarnessError` en el bench (fuera del
  denominador).
- **Reporte del bench**: pass^k de tau-bench (la métrica de
  confiabilidad de la tesis — reveló que gpt-oss:20b es pass^5=100% y
  los fallos de gemma4:e4b son sistemáticos, no flakiness) y
  comparación pareada McNemar exacto + Holm vs el primer brazo (K-19).
- **Grading semántico**: `expect_cargo_check = true` en la suite corre
  cargo check real post-run; needles fijados contra fixtures canónicos.
- **Prompt caching Anthropic directo** (3 breakpoints, como OpenRouter;
  `+ablate:no-caching` aplica).
- **Summary-por-lead** (`enable_lead_summary` / `+ablate:lead-summary`):
  la compactación le pide el summary al modelo del `--lead`, fallback
  al digest determinístico.
- **TTC local** (`+ablate:ttc=N`): N rollouts + selección por
  auto-consistencia sobre `outcome_fingerprint`, costos sumados.
- **`braze run --output-format json`** para CI/scripting; `ollama stop`
  del bench ahora apunta al nodo remoto (`OLLAMA_HOST`, K-11).
- Seguridad (v8 Paquete 2): `git diff/log/show` path-checked, `.braze/`
  Irreversible para escrituras del modelo, `ask_user` sanitizado,
  ProjectMemory sin `objective`/`notes` en el render y con campos
  sanitizados, reparación N-5 bajo el lock N-27.

## LocalBackend (2026-07-20/21) — quinto `ModelBackend`, Fases 1-3 completas

Inferencia **in-process** sobre llama.cpp (`llama-cpp-2`, feature
`local` / `local-cuda`), sin server — el harness dueño desde los tokens:
la clase de bug #1/#17 (parser server-side de Ollama) es imposible por
construcción. Detalle completo y recetas en
`docs/local-backend-design-2026-07-20.md`.

**Resultado cumbre (2026-07-21): gpt-oss:20b por el camino Harmony =
57/57 (100%), pass^3=100%** en `default.toml` — la primera corrida de
suite completa del LocalBackend Harmony, cero flakiness, `rescues=0`
por diseño (el parser del backend emite las calls directo), y el mejor
número que el proyecto ha producido en su suite (el mismo modelo vía
Ollama: 98.9%, n=95, 13-jul). CPU puro, 41s/tarea promedio.
Datos: `nitro:~/sweep-rank-oss.json`.

**Lectura estratégica de la independencia de Ollama** (2026-07-21):
importa en tres ejes — (1) elimina la clase de fallo server-side por
estructura (evidenciado por el 100% de arriba); (2) es la capacidad
experimental que ningún harness API-bound tiene (sampler access →
stencil, ablaciones por token); (3) el ecosistema se bifurcó: Ollama
convirtió gpt-oss y TODA la familia Gemma para su engine propio
(blobs incompatibles con llama.cpp), así que el LocalBackend alinea a
braze con el ecosistema GGUF abierto en vez de un runtime amurallado.
El precio: mantener plantillas por familia (3: ChatML, Harmony,
Gemma), bajar GGUFs canónicos, y la fragilidad del build FFI. Para
braze-como-herramienta es un seguro; para braze-como-laboratorio, el
activo central.

**Familias soportadas** (plantilla + detección por arch GGUF/label/
`BRAZE_LOCAL_FAMILY`): ChatML/qwen (reusa blobs de Ollama), Harmony/
gpt-oss y Gemma 2/3/4 (requieren GGUF canónico — en `~/models/` de
ambos nodos: `gpt-oss-20b-MXFP4`, `gemma-4-E4B-it-qat`,
`gemma-4-12B-it-qat`). El stencil y la escalera de rescate funcionan
para las tres (operan sobre la convención textual o el parser propio,
no sobre la plantilla).

- **Fase 1 (CPU + ChatML/qwen)**: paridad medida vs OllamaBackend
  (McNemar p=0.22 n.s.; schema en preámbulo dejó schema_fail 17→0).
- **Fase 2 (GPU)**: build CUDA en Nitro (receta en el design doc —
  libclang-18, `LLAMA_BUILD_SHARED_LIBS=1`); `BRAZE_LOCAL_GPU_LAYERS`.
  qwen2.5:3b entero en la RTX 3050 (37/37 capas).
- **Fase 2b (Harmony/gpt-oss, 2026-07-21)**: `harmony.rs` en braze-model
  (plantilla canónica + `HarmonyParser`; módulo puro, 17 tests corren
  sin el feature). Los marcadores Harmony son tokens especiales → el
  parsing vive en el backend (emite `ToolCallRequested` directo), la
  escalera del engine queda de red de seguridad. Familia autodetectada
  (arch GGUF/label/`BRAZE_LOCAL_FAMILY`); `BRAZE_LOCAL_REASONING`
  (default medium); `stop_reason` honesto (`tool_use`/`stop`/`length`).
  **Verificado en vivo** (2026-07-21): loop agéntico completo de
  gpt-oss:20b en Nitro, CPU (~15s/ronda) y GPU parcial 8/24 capas
  (~10s/ronda), canal analysis suprimido, stdout limpio.
- **Hallazgo**: el blob gpt-oss de Ollama NO es GGUF de llama.cpp (arch
  `gptoss`, tensores `attn_out` vs `attn_output` — es del engine propio
  de Ollama). Para gpt-oss el LocalBackend usa el GGUF canónico
  `ggml-org/gpt-oss-20b-GGUF` en `nitro:~/models/` por ruta directa
  (`--model ~/models/gpt-oss-20b-MXFP4.gguf`). "Reusar GGUF de Ollama"
  vale para qwen, no para gpt-oss.
- Logs de llama.cpp/ggml ruteados a `tracing` (sin eso la TUI se rompe —
  verificado y arreglado 2026-07-21).
- **Fase 3 (stencil GBNF, 2026-07-21)**: constrained decoding con
  laziness manual — `stencil.rs` (gramática envelope qwen con inventario
  de tools + args JSON harmony) y swap de sampler en el loop; kill-switch
  `BRAZE_LOCAL_GRAMMAR=off` (brazo de ablación). Verificado en vivo en
  ambas familias. Bug depurado: double-accept del sampler (latente desde
  Fase 1, fatal solo con gramática). Ver design doc § Fase 3.
- **A/B del stencil: 3 pasadas, veredicto estable** (2026-07-21,
  `docs/sweep-stencil-ab-2026-07-21.md`): pass rate empatado las tres
  veces (envelope y schema-derivada) — **sin ganancia, sin constraint
  tax**. Insight clave: el loop de reparación del engine (repair
  message + retry) ya absorbe los schema_fail (todas las corridas con
  schema_fail pasaron igual) — el stencil compite contra un harness que
  ya cubre la clase río abajo. Su A/B con señal esperable: modelos no
  saturados (gemma4:e4b) o suites adversariales. El proceso destapó 3
  bugs latentes de Fase 1, corregidos (double-accept, prompt>n_batch=
  abort, token de control espurio) + dispatch ahora traza fallos de
  validación con args.
- **Familia Gemma (2026-07-21)**: tercera plantilla del LocalBackend
  (`gemma.rs`: `<start_of_turn>`, system plegado al primer turno user,
  convención textual compartida → escalera de rescate y stencil
  funcionan gratis). **Ningún blob Gemma de Ollama carga en llama.cpp**
  (gemma3: metadata faltante; gemma4:e4b: layout de tensores del engine
  propio — 720 vs 2131) — regla: qwen reusa blobs de Ollama; gpt-oss y
  Gemma necesitan GGUF canónico (gemma4:e4b QAT de unsloth en
  `~/models/`, verificado en vivo con tool call estencilada).
- **gemma-4-12B por LocalBackend** (2026-07-21): capacidad
  prometedora, throughput limitante — primer sweep censurado por
  timeout (29/57 timeouts a 180s con 12 capas GPU; **93% condicional
  entre las corridas que terminaron**, 26/28, con schema_fail=0).
  Re-run con 14 capas + 360s en curso al cierre de esta nota. Mapa de
  la línea Gemma 4 en Nitro (16GB): 12B cabe cómodo (6.7GB QAT),
  26B-A4B (MoE 4B activos, rival directo de gpt-oss:20b) solo en
  IQ4_XS 13.6GB apretado — ES el argumento para subir Nitro a 32GB;
  31B fuera de alcance.
- Pendiente de la línea: veredicto del ranking 12B vs gpt-oss (re-run
  en curso), A/B stencil sobre gemma4:e4b (sus 3 fallos sistemáticos
  de single_tool son la clase donde el envelope muerde), instalar el
  binario CUDA estable en Nitro (dos tropiezos por binarios
  desincronizados el 21-jul — subió de prioridad), fail-fast de brazo
  en el bench (57 fallos instantáneos de carga no deben quemar un
  brazo en silencio), y paridad GPU vs Ollama sobre qwen2.5:3b.

## Próximos pasos al retomar

(Actualizado 2026-07-18 tras ejecutar la auditoría v8 — Paquetes 0-3
completos y el top-6 S/M del Paquete 4 en main; el sweep BFCL corrió el
mismo día. El arco LocalBackend de 2026-07-20/21 está en su § arriba.)

- **Ancla BFCL**: análisis post-sweep (transporte 2% → grader → E1-E4,
  `docs/bfcl-anchor-RESUME.md`) e integración al paper; luego re-runs
  bloques 1-2 y probe Parte B (diseños pre-registrados).
- **A/B Gemma4**: Ollama de Nitro **ya actualizado a 0.32.1** (2026-07-20,
  era 0.30.7 — el fix de tool calling del 16-jul ya está en el nodo);
  falta re-correr e4b vs gpt-oss:20b con el MISMO digest (`c6eb396dbd59`
  — el stealth refresh del 15-jul aún no llega al registry); vigilar el
  cambio de digest para el A/B de pesos.
- **A/Bs nuevos de la cola de Nitro**: lead-summary (con `num_ctx`
  chico para que compacte de verdad) y TTC (`qwen2.5:3b` vs
  `+ablate:ttc=3`, cruzado con pass^k).
- **Manuscrito** (`paper/`): prosa de los TODOs, `/verify-refs`, venue,
  `/zenodo`; anotar en Threats el complemento McNemar/Holm y las citas
  nuevas de v8 § 6 (Constraint Tax, Trivedy, MemCoder, pass^k).
- **TUI**: verificación en vivo del overlay `ask_user` con un modelo
  que llame la tool (resto de la fase 3 verificado en vivo el
  2026-07-19); celdas de compactación (diferido explícito).
- **Modo doc-QA (RAG léxico offline sobre LocalBackend)** — propuesta
  del 2026-07-23 en `docs/docs-qa-mode-design-2026-07-23.md`. Nace de un
  caso externo (Claudio Álvarez): chatbot liviano offline sobre
  documentación (dominio SPE/Posesiones Efectivas), hardware modesto.
  Decisión de arquitectura: **pipeline RAG (retrieve-then-answer, sin
  tool call) como default**, agéntico (`DocsProvider`) como escalón;
  retriever léxico reusa `tool_search::search_stubs`. Anclado a evidencia
  (2 capturas de Claudio: su app RAG con embeddings+híbrido degrada a
  90s/"modo degradado" — síntesis sobrecargada — vs Ollama pelado que
  sintetiza limpio → confirma contexto-chico). Sin código aún; ruta MVP =
  crate `braze-docs` (chunker + `LexicalIndex`) + modo `braze docs`.
  Gaps: UI "cara de GPT" (TUI es terminal), hardware objetivo de Claudio
  (bloquea la recomendación de modelo). Ángulo de publicación aplicado
  ligado al Paper 1. **Pasos 1-2 del MVP EJECUTADOS (2026-07-23)**: crate
  `braze-docs` (chunker + `LexicalIndex` port de `search_stubs`) +
  subcomando `braze docs --dir <wiki> <pregunta>` (pipeline retrieve→
  prompt-mínimo→backend→respuesta citada, despachado como `run_docs` en
  main.rs). 26 tests verdes, clippy limpio, verificado en vivo offline
  (ollama qwen2.5:3b) con la pregunta real de Claudio → 5 pasos limpios +
  fuentes. **`--backend local` (offline TOTAL, llama.cpp in-process sin
  Ollama) también verificado en vivo** — misma respuesta limpia, valida
  el requisito "ninguna llave de servicio" de Claudio. Falta solo la UI.
- **P1.1 resto**: repartir el `mod tests` de `engine/mod.rs`.
- **v8 restantes**: Paquete 4 L (Landlock write-only, subagente
  Viewer/Editor, background trans-ronda), P0.2 (costo USD/walltime por
  turno), K-16 (negative-cache MCP), AGENTS.md interop. J-12 y J-26
  (v7) quedaron cerrados con la fase TUI 3.
- Infra Nitro: IP fija en el router, `OLLAMA_KEEP_ALIVE`.

## Modelos locales recomendados (Ollama)

**El mejor modelo local del proyecto es `gpt-oss:20b` corriendo en
Nitro** — y desde el 2026-07-21, **la mejor forma de correrlo es el
LocalBackend** (camino Harmony: 57/57 y pass^3=100% en `default.toml`,
vs 98.9% del mismo modelo vía Ollama; ver § LocalBackend). La
recomendación original vía Ollama (sweep de capacidad 2026-07-13,
`docs/sweep-capacity-hardware-2026-07-13.md` +
`docs/sweep-g10-weak-skills-gptoss20b-2026-07-13.json`) — reemplaza a
`qwen3.5-coder` en esta recomendación. MoE ~3.6B activos, corre en los
16GB RAM de Nitro sin offloading ni cambios de infraestructura.
**6/6 en `g10-weak-skills`** (satura error_recovery + distractor_selection,
igual que `qwen3.5-coder`), y en `default.toml` (n=95, sweep del mismo
día) **98.9% pass rate a 13.0s promedio por tarea** — supera a
`qwen3.5-coder` en pass rate (+6.3pp, IC Newcombe fuera de cero) y es
~1.9× más rápido (13.0s vs 24.7s), con mecanismo limpio
(`schema_validation_failures=0`). Detalle completo de la decisión —
incluye por qué se descartó construir un `LocalBackend` in-process para
conseguir esta mejora — en `docs/local-backend-stencil-design.md`.

Nota Gemma 4 (2026-07-18): Google publicó un *stealth refresh* el
15-jul (fixes de tool calling, τ²-Airline +8pp en E4B) pero los pesos
NO están en el registry de Ollama todavía (`gemma4:e4b` sigue en digest
`c6eb396dbd59`, el mismo del sweep del 13-jul — re-pullear es no-op).
La palanca accionable era **Ollama ≥0.32.1** (16-jul, fix propio de tool
calling Gemma 4), y Nitro **ya está en 0.32.1** (actualizado 2026-07-20);
el A/B de runtime queda listo para correr, en "Próximos pasos". pass^k
mostró que los 3 fallos de e4b son sistemáticos (una tarea de
`single_tool`), no flakiness — si el fix los repara, e4b salta a
pass^5=100% y desafía a gpt-oss:20b como default por RAM.

Nota infra (2026-07-20): Nitro corre **Ollama 0.32.1** (era 0.30.7). El
upgrade resolvió además la recurrencia del incidente #1 del testbed roam
(HTTP 500 "error parsing tool call": Ollama 0.30.7 parseaba el canal
harmony de gpt-oss server-side y reventaba cuando el razonamiento se
filtraba — aunque el request no declarara tools). A/B confirmatorio: el
mismo guion multi-turno a 8192 daba HTTP 500 en 0.30.7 y **cero** en
0.32.1, y con el 500 fuera la cadena de 3 turnos completó por primera
vez. Detalle en `docs/bitacora-harness-modelo.html`.

`qwen3.5-coder` sigue siendo un modelo local sólido (sweep 2026-07-06,
`docs/sweep-nitro-sampling-2026-07-06/`: 6/6 en `g10-weak-skills` a temp
0.2, ~20-27s por tarea) pero ya no es la primera recomendación. Caveat
vigente para `qwen3.5-coder`: es *thinking model* — Ollama devuelve el
razonamiento en un campo `thinking` separado y con `num_predict` chico
el content puede salir vacío; presupuestar tokens. Para la familia Qwen
**chica** (qwen2.5:3b), el sampling recomendado por Qwen (temp 0.7 /
top_p 0.8 / top_k 20 / repeat_penalty 1.05, flags de braze-bench) rinde
mejor que el 0.2 default del bench (0/6 → 2/6 en g10, direccional en dos
entornos).

El sweep del 2026-07-04 (`default.toml`, `--repetitions 5`, ver
`docs/AUDITORIA-2026-07.md`) dejó además dos hallazgos vigentes sobre qué
modelos priorizar:

- **No todo modelo instruct genérico soporta tool-calling en Ollama.**
  `gemma3:1b` falló 0/50 tareas instantáneamente (~185ms promedio) porque
  Ollama devuelve HTTP 400 "does not support tools" para ese modelo — no es
  un problema de capacidad de razonamiento, es incompatibilidad de API.
  Verificar `ollama show <modelo>` o probar una tarea trivial con tools
  antes de asumir que un modelo nuevo funciona con `braze`.
- **Modelos orientados a function-calling superan claramente a los
  genéricos del mismo tamaño.** `qwen2.5:3b` y `qwen2.5:7b` (ambos con
  soporte de tools nativo) alcanzaron 62-64% de pass rate en el sweep;
  `llama3.2:1b` (soporta tools, pero es un modelo pequeño genérico) quedó
  en 18%. Para evaluar `braze` con modelos locales, priorizar modelos ya
  fine-tuneados para tool-calling (p.ej. la familia Qwen2.5/3.5 orientada a
  function-calling, xLAM-2-3B, Hammer, Nemotron Nano) por sobre variantes
  instruct genéricas del mismo tamaño (Llama3.2, Gemma3) — técnica G6 del
  roadmap de la auditoría 2026-07.

## Modelo recomendado vía OpenRouter (`deepseek/deepseek-v4-flash`)

Sweep de `braze-bench` del 2026-07-05 (5 repeticiones, mismo suite
`default.toml`) contra `openrouter:deepseek/deepseek-v4-flash` — datos
crudos en `docs/sweep-deepseek-v4-flash.json`/`.log`:

- **49/50 pass rate** (±5pp), 2.4 rondas promedio, ~6.6s de latencia
  promedio por tarea, 0 fallos de validación de schema, 0 fallos de
  ejecución, 0 denegaciones de permiso. Perfecto en 4 de 5 skills
  (`no_tool`, `multi_step`, `error_recovery`, `distractor_selection`);
  29/30 en `single_tool`.
- **Verificado también en vivo con la TUI** (`braze chat --tui --backend
  openrouter --model deepseek/deepseek-v4-flash`, 2026-07-05): streaming
  fluido, tool calls reales (`write_file`) renderizando correctamente.
  Notablemente más rápido en la práctica que Ollama local en esta
  máquina — inferencia en la nube vs. CPU local, no una diferencia de
  arquitectura de `braze`.
- Buen default de bajo costo para probar `braze` sin la latencia CPU-bound
  de un modelo local ni el costo de un modelo de frontera — recomendado
  como primera opción al evaluar cambios rápidos o hacer demos.

Esto no cambia el default de `braze-config` (`ollama_model = "llama3.1"`,
`crates/braze-config/src/config.rs`) — queda como criterio para elegir qué
modelo configurar/tener disponible localmente, no como un cambio de código.
