# Survey de agentes de referencia — qué tomar para braze

Fecha: 2026-08-10
Método: clonado + revisión enfocada (un subagente Explore por repo), cada
uno briefeado con las características de braze para juzgar novel-vs-cubierto.
Repos: **magnitude** (Apache-2.0, agente TS + motor inferencia Rust),
**aider** (Python, referencia de formatos de edición), **SWE-agent**
(Python, ancla académica del ACI), **codex** (Rust, primo de diseño de
braze). Todo con licencia permisiva → tomar con atribución.

Regla de lectura: braze apuesta a **harness-compensa-modelo-chico**. Varias
features de estos repos se construyeron para modelos frontier y sus propios
autores las deprecaron al mejorar los modelos (SWE-agent → mini-swe-agent);
para modelos locales chicos **vuelven a ser esenciales**, que es justo la
tesis de braze. Eso reordena las prioridades respecto de lo que un proyecto
frontier elegiría.

## Tier 1 — construir ahora (alto ROI, barato, on-thesis)

1. **[HECHO 2026-08-10, commit 2e9a3e5]** **Gate sintáctico ANTES de
   aplicar la edición** (SWE-agent `tools/windowed_edit_linting/bin/edit`).
   braze corría `cargo check` DESPUÉS de que la edición aterriza
   (apply-then-warn); ahora un `syn`-parse instantáneo rechaza la edición
   ANTES de escribir si introduce un error de sintaxis nuevo, dejando el
   archivo SIEMPRE válido. `braze_tools_local::syntactic_gate`, on by
   default (`disable_syntactic_edit_gate` / `+ablate:no-syntactic-gate`),
   complementa el `cargo check` de después. Verificado en vivo.
2. **Diffear errores pre/post del check → mostrar solo los NUEVOS**
   (SWE-agent `flake8_utils.py`). Hoy braze le pasa al modelo toda la
   salida del `cargo check`, incluidos warnings preexistentes que el
   modelo no causó — derraila a un modelo chico. Aplicable al post-edit
   check ya, sin revert.
3. **seccomp: denegar `io_uring_*` + `ptrace` incondicional, y sockets
   solo `AF_UNIX` cuando la red está off** (codex
   `linux-sandbox/src/landlock.rs:169`). El Landlock write-only de braze
   deja estas clases de bypass completamente abiertas. ~30 líneas con
   `seccompiler`. Máxima seguridad por línea.
4. **Read-denial de rutas secretas** (codex, vía Landlock ABI≥1 read
   scoping). El sandbox write-only de braze es **exfiltración-abierto**:
   el agente puede leer `~/.ssh`, `.env`, credenciales cloud. Aunque sea
   una lista chica de globs ilegibles, es un upgrade real.
5. **Subpaths read-only dentro de raíces escribibles** (`.git/hooks`,
   `.braze/`) (codex `protocol.rs:1060`). Hueco de escalación: el agente
   reescribe un git hook y corre código arbitrario en el próximo commit.
   Cambio chico.
6. **Métricas de harness de primera clase en el bench** (aider
   `benchmark.py:942`, `notes.md:21`): "% que usó el formato de edición
   correcto", tasa de tool-call malformado, tasa de match fallido de
   `edit_file`, conteo de elisión perezosa, y **pass@try-1 vs pass@try-2**
   (cuánto recupera el guardrail/interlock). Aísla fallo-de-harness de
   fallo-de-modelo — el KPI central de la disciplina de braze.
7. **NO agregar fuzzy-match a `edit_file`** (aider lo construyó y lo
   **desactivó** deliberadamente, `editblock_coder.py:183`: la aplicación
   aproximada corrompe código en silencio). Valida el diseño de braze
   (fallar y caer a write_file). SÍ mantener match tolerante a
   whitespace. Add barato: en match fallido, devolver las **líneas reales
   más parecidas** (difflib-style) antes del fallback.

## Tier 2 — construir pronto (buen ROI, algo más de trabajo)

8. **Harness de paridad POR-PRIMITIVA** (magnitude `parity/`). El McNemar
   actual dice *si* braze diverge de Ollama; esto dice *dónde*: freezear
   y diffear las representaciones intermedias por etapa — token-IDs,
   prompt renderizado, string de gramática GBNF (¡el stencil!), orden
   top-k — contra Ollama/llama.cpp. No necesita su fork ni su maquinaria
   JSONL; la idea transferible es diffear las etapas, no solo el texto
   final. Empezar por token-IDs + template + GBNF (C5).
9. **Formato de edición del `editor` child según capacidad del modelo**
   (aider `models.py:131`, `notes.md:36`). Dato de aider: modelos
   chicos/débiles son más confiables con **whole-file** (sin fallos de
   anchor/indentación) a costa de tokens; `diff`/anchor solo paga en
   modelos capaces. El interlock de braze (2 edits fallidos → write_file)
   ya es una versión *reactiva* de esto — hacerlo *proactivo* para
   modelos chicos conocidos. Caveat: el resultado "udiff 3× menos
   perezoso" es de modelos frontier; NO generalizar a chicos.
10. **A/B del "impuesto JSON"** (aider `2024-08-14-code-in-json.md`):
    aider midió que envolver código en un arg JSON de tool-call **degrada
    la calidad del código** vs texto en fence. braze entrega ediciones
    como tool calls JSON — probable que el impuesto sea peor en modelos
    chicos. Ablación natural en el bench de braze: misma edición como
    JSON-tool-arg vs bloque de texto parseado por el harness.
11. **Vista esqueleto tree-sitter** (SWE-agent `tools/filemap`):
    signaturas + headers de struct/enum/impl, cuerpos elididos. Deja a un
    modelo chico entender un archivo grande gastando pocos tokens —
    economía de contexto es *el* cuello de botella del modelo chico.
    Buildable en Rust (tree-sitter). Candidato a ablación de bench.
12. **execpolicy: reglas de comando self-testing con justificación**
    (codex `execpolicy/`). Upgrade del clasificador de shell hand-rolled
    de braze a datos validados: `match`/`not_match` como tests de la
    política al cargar (no shippear una regla rota), y `justification`
    surfaceada ("denegado, usá X en vez") que mejora la recuperación del
    agente medblemente.
13. **Auto-aprobación contingente a que el sandbox esté realmente activo**
    (codex `safety.rs:72`). braze acaba de hacer fail-closed en fallo de
    aplicación; el ángulo nuevo es que si Landlock está off/falló, la
    política de confirmación debe *apretarse*, no quedar permisiva.

## Tier 3 — nudges baratos de modelo chico (bajo esfuerzo, on-thesis)

14. **Mensajes de requery específicos por código de error** (SWE-agent
    `parsing.py:374`): distinto mensaje para {sin tool call, múltiples,
    arg faltante, JSON malo}. La escalera de rescate textual de braze
    debería decir *exactamente* qué salió mal.
15. **Coaching de comportamiento dentro del docstring de la tool**
    (SWE-agent: la advertencia de indentación en la descripción de
    `edit`). Poner el modo de falla más común del modelo en la
    descripción misma.
16. **Blocklist de comandos interactivos/colgones + env que neutraliza
    pagers** (SWE-agent `tools/tools.py`: `vim`/`python` REPL/`tail -f`;
    `PAGER=cat GIT_PAGER=cat`). Previene una clase entera de cuelgues del
    sandbox. Trivial.
17. **Dedup de vista de archivo obsoleta** (SWE-agent
    `ClosedWindowHistoryProcessor`): colapso *file-aware* — sabe que dos
    observaciones muestran la misma región y guarda solo la fresca.
    Complemento del lado-observación a los nudges de relectura
    improductiva que braze ya tiene.
18. **Cap refuse-on-flood + modo count-only en grep/glob** (SWE-agent
    search): rehusar y pedir acotar cuando el match es enorme, en vez de
    inundar el contexto. ripgrep da `--count`/`--files-with-matches`
    gratis.
19. **Self-review escalonado en submit** (SWE-agent
    `review_on_submit_m`): el primer "listo" devuelve un checklist de
    verificación y solo el segundo cierra. Gate determinístico de "¿de
    verdad verificaste?", sin capacidad extra del modelo.
20. **Catálogo offline con headers GGUF compactados** (magnitude
    `catalog/` + `planner_stub.rs`): recomendar el mejor modelo que el
    hardware corre **sin descargar nada**, corriendo el estimador de fit
    sobre headers reales compactados. Barato, sin fork; upgrade de
    `tune_model`. Más disciplina de procedencia (`measured` vs
    `estimated` con URL de evidencia).

## Tier 4 — inferencia/latencia (LocalBackend; magnitude la referencia)

21. **KV prefix-reuse + checkpoint de `LlamaSequenceState`** (magnitude
    `scheduler.rs:153`). Cachear el estado de secuencia en el borde del
    durable-summary del compactor evita re-prefillear el prefijo estable.
    Ganancia de latencia single-request, **sin fork**. El de mayor ROI de
    esta familia.
22. **MTP / decoding especulativo** (magnitude `icn-mtp/`): 2-3× en
    decode sobre tokens aceptados — lo que haría tolerable un 20B en la
    CPU de Nitro. Era el "instrumento A" de round-economics. **GATED en
    forkear `llama-cpp-2`** (magnitude usa `magnitudedev/llama-cpp-rs`);
    copiar ahora la *forma de la política* (preflight → bundled → single
    draft → disable, rechazar ambigüedad), construir cuando braze forkee
    o upstream exponga spec-decoding.
23. **Descomposición exacta del fit** (magnitude `icn-hardware`): modelar
    por-capa el KV incl. sliding-window y ratio de compresión KV-quant.
    Portar el *desglose* (modelo vs KV-por-capa vs compute vs draft) aun
    sin la superficie nativa forkeada.

## Explícitamente NO tomar (con razón)

- **Flota multi-agente genérica** (magnitude: 8 roles + coordinador +
  task graph; su control de profundidad es por prompt "no anides", más
  débil que el depth-1 *estructural* de braze). Es la orquestación que la
  auditoría prohíbe.
- **Compactor por LLM-turn** (magnitude): no-determinista y cuesta una
  inferencia. Su propio *fallback determinístico* valida el compactor
  determinístico de braze.
- **Viewer stateful con cursor** (SWE-agent): agrega estado oculto que un
  modelo chico debe trackear — pasivo. Los reads stateless de braze son
  mejores. Robar solo el afford "(N more lines above/below)".
- **execve-interception escalation server** (codex): potente pero pesado
  (shell parcheado, socket, FD passing); resuelve un problema de escala
  que braze no tiene.
- **Backends macOS Seatbelt / Windows** (codex), **Responses-over-WS**
  (codex), **índice SQLite de sesiones** (codex/magnitude), **benchmark
  de serving/goodput** (magnitude): inversiones de plataforma/escala, no
  ideas que a braze le falten. Refactorizar el `ModelBackend` trait hacia
  el modelo data-struct de codex **no aplica** (los backends de braze son
  conductualmente distintos, no solo variantes de wire format).

## Citas para el paper (Related Work / harness engineering)

- **SWE-agent ACI** (`docs/background/aci.md` + arXiv 2405.15793) — el
  ancla más limpia de "la interfaz es parte del harness".
- **aider** — resultados empíricos de formato (whole vs diff según
  capacidad; impuesto código-en-JSON).
- **Embedding limits** (arXiv 2508.21038, ICLR 2026) — respaldo teórico
  de recuperar código léxicamente (grep), no por embeddings; aider es el
  co-ejemplo (repo-map por PageRank, sin embeddings).
- **magnitude / Kimi Code / SWE-Edit** — la industria converge a
  subagentes de contexto angosto sobre modelos locales, *sin medir* la
  ganancia; la contribución de braze es medirla con A/B.
- **Diferenciación**: el compactor determinístico-que-resume de braze es
  un *avance* sobre el ACI de SWE-agent, que solo elide/omite y sale por
  overflow, nunca resume.

## Convergencias (señales fuertes: coinciden ≥2 repos)

1. **Mantener el archivo siempre-válido para el modelo chico**: SWE-agent
   (revert-before-apply) + aider (fuzzy-match desactivado) → fallar
   limpio antes que aplicar algo aproximado.
2. **Envolver código en JSON/tool-call es un impuesto**: aider
   (code-in-JSON) + SWE-agent (parser text-based `bash_only` para modelos
   sin tool-calling) → A/B fenced-text vs JSON-arg.
3. **Salida terse/acotada > inundar** para modelos chicos: SWE-agent
   (search file-list + refuse-on-flood, dedup de observaciones).
4. **Los modelos chicos necesitan coaching de formato + errores
   específicos**: SWE-agent (docstring coaching + requery por código) +
   aider (tabla de formato por modelo).
5. **Determinístico > LLM-turn para compaction**: el fallback de
   magnitude valida el default de braze.

## Anexo (2026-08-10): gemini-cli — el quinto repo (segundo primo declarado)

Clonado y revisado aparte, enfocado en lo que codex no cubrió. Es
TypeScript (ideas, no código portable). Hallazgo central: gemini-cli está
a mitad de migración de un sandbox de proceso-completo (contenedor) a un
**sandbox por-tool con Bubblewrap** — que es EXACTAMENTE el "trabajo
futuro / out-of-process" que el module doc de `braze-permissions::sandbox`
declaró como el fix real para lo que Landlock no puede.

**Worth building (nuevo, alto valor):**
- **Sandbox por-tool con Bubblewrap** (`packages/core/src/sandbox/linux/
  bwrapArgsBuilder.ts` + `LinuxSandboxManager.ts`): `--ro-bind / /`
  (todo el FS read-only) + `--bind`/`--ro-bind` selectivos del workspace
  por-comando, seccomp BPF adjunto vía `--seccomp`, `--die-with-parent`.
  Logra las DOS cosas que el Landlock write-only de braze provablemente
  NO puede: (a) **read-denial de secretos** — `.env`/`.env.*` se
  descubren y se **bind-montan con una máscara `chmod 0`** (o
  `--ro-bind /dev/null`), así el tool literalmente no los puede leer;
  (b) **política de FS por-comando** (`.git` read-only salvo para git).
  Es el blueprint concreto del ítem futuro de braze; implementable en
  Rust shelleando a `bwrap` con args-file (pasan args por fd 8 para
  esquivar ARG_MAX). Detalles a robar: ordenar mounts por largo de dest
  (padres antes que hijos), pre-crear los governance files para poder
  protegerlos. **Esto reemplaza al "trabajo futuro" difuso del module
  doc de sandbox.rs por un diseño de referencia real.**
- **Carga JIT de AGENTS.md por subdirectorio** (`memoryDiscovery.ts
  loadJitSubdirectoryMemory`): cuando un tool toca un subdir, carga el
  GEMINI.md más cercano hacia arriba (techo = git root) on-demand, en vez
  de todo el árbol al inicio. Mantiene el system prompt chico — justo la
  restricción del modelo chico. Sobre el AGENTS.md que braze ya tiene.
  `@import` con guardas de ciclo/profundidad es un nice-to-have.
- **Versiones determinísticas de**: (a) truncado de tool-output por
  presupuesto-de-tokens-inverso con spill-to-file (ataca el "un grep
  gigante domina el contexto"); (b) el schema de slots `<state_snapshot>`
  (goal/constraints/knowledge/artifact_trail/task_state con
  [DONE]/[IN PROGRESS]/[TODO]) como PLANTILLA que braze llena
  determinístico, no pidiéndole prosa al modelo.
- **`omissionPlaceholderDetector`** (rechaza `// ... rest unchanged ...`
  en ediciones): braze YA tiene su análogo (`elision_marker` en
  edit_file), así que acá braze está a la par — confirmarlo, no
  reconstruirlo.
- **Bench**: Dynamic Baseline Verification (ante un fallo de A/B,
  re-corre el baseline para distinguir regresión de fallo preexistente),
  la escalera ALWAYS/USUALLY para checks flaky, e IQR+mediana+warmup para
  métricas de walltime/costo. La estadística de braze (pass^k, McNemar)
  es MÁS rigurosa; tomar solo su manejo de no-determinismo, no sus
  umbrales.

**Cite, don't build:** el sandbox por contenedor (Docker/Podman/Seatbelt/
gVisor — elección de escala/consumidor, no encaja con el binario chico de
braze; robar solo el truco "montar el workspace en la MISMA ruta absoluta
host↔contenedor"); el policy engine TOML declarativo + **hashing de
integridad de la política** (análogo a execpolicy de codex; el hashing es
una frase linda para la sección de seguridad del paper); el compactor
LLM-summarizing con probe de auto-verificación (contraste: braze es
determinístico; un modelo chico produce snapshots débiles). Contraste
para el paper: gemini usa tool-calling nativo de la API → NO hace rescate
textual; braze DEBE hacerlo para modelos locales — motiva la escalera de
rescate de braze.

**Skip:** `packages/a2a-server` (protocolo Agent2Agent sobre HTTP,
"experimental") y la capa de subagentes remotos/anidados
(`remote-subagent-protocol.ts`, sin cap de profundidad) — es la
orquestación multi-agente genérica que braze rechaza. Sus subagentes
investigadores read-only nombrados (`codebase-investigator`, `generalist`)
mapean 1:1 al `explore` de braze — valida el diseño depth-1 (cita).
