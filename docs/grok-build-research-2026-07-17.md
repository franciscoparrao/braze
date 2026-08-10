# Investigación: xai-org/grok-build — ideas aplicables a braze

Fecha: 2026-07-17
Metodología: 2 agentes de investigación en paralelo vía WebFetch sobre
`github.com/xai-org/grok-build` (arquitectura central uno, UX/protocolo el otro),
cruzado contra el conocimiento de la arquitectura actual de braze. El repo tiene solo
3 commits y no acepta contribuciones externas — es casi seguro un espejo periódico del
monorepo interno de xAI, no el desarrollo primario; el historial de commits no es
informativo, pero el código/docs actuales sí lo son. Los nombres de crates son
autodescriptivos (`xai-grok-sandbox`, `xai-grok-compaction`, `xai-circuit-breaker`,
...), lo que permitió ir más profundo de lo típico en un espejo delgado. Cobertura:
~25 fetches dirigidos por relevancia de nombre sobre un árbol de ~950 rutas bajo
`crates/` — no exhaustivo. Un fetch tempranero confundió "xAI" con "SpaceXAI"; no se
repitió ese error. `docs.x.ai/build/sandboxing` devolvió 404 — el entendimiento de
sandboxing es solo de código fuente, sin confirmación de docs oficiales.

## Ganancias baratas — hacer pronto, riesgo bajo

1. **Protección SSRF en cualquier tool HTTP** (`web_fetch/ssrf.rs`): resuelve el
   hostname y rechaza rangos privados (RFC 1918), link-local (RFC 3927, incluye
   explícitamente `169.254.169.254`, el endpoint de metadata cloud de AWS/GCP/Azure),
   CGNAT (RFC 6598) e IPv6 link-local/ULA, permitiendo loopback para dev local. Si
   braze tiene o llega a tener una tool que resuelve URLs provistas por usuario o
   modelo, este patrón (resolver DNS, chequear el rango de IP) es barato y sin
   contrapartida real.
2. **Clasificación de errores HTTP + circuit breaker genérico para `ModelBackend`**
   (`retry_policy.rs` + `xai-circuit-breaker/src/breaker.rs`): Retryable (429 + todo
   5xx), AuthRefresh (401, un refresh y se rinde), Terminal (400/403/404, descarta
   inmediato). El circuit breaker es de 3 estados (Closed/Open/HalfOpen) con ventana
   deslizante + mínimo de muestras, y un detalle fino: un probe half-open abandonado
   se trata como huérfano tras `open_duration` para que un probe perdido no demore la
   recuperación más de un cooldown. Patrón bien trillado, bajo riesgo — vale comparar
   directo contra el manejo de errores HTTP actual de `crates/braze-model`.
3. **Exit codes documentados + `--output-format json|streaming-json` para `braze
   run`** (`14-headless-mode.md`): 0 éxito, 1 error, 130 SIGINT, 143 SIGTERM; salida
   `plain` (default), `json` (un objeto al final: text/stopReason/sessionId/
   usage/total_cost_usd), o `streaming-json` (NDJSON de eventos). Ingeniería de CLI
   cuidadosa, no protocolo nuevo — checklist barato para que `braze run` sea
   genuinamente usable en CI.
4. **Capa de "perfiles de sandbox" nombrados, solo config, sobre el allowlist
   actual** (`profiles.rs`): `workspace`/`devbox`/`read-only`/`strict`/`off`,
   extensibles por el usuario, con una regla de seguridad simple: la config de
   proyecto puede *agregar* nombres de perfil nuevos pero no puede redefinir uno que
   ya exista en la config global (evita que un repo malicioso afloje silenciosamente
   un perfil de confianza). No requiere sandboxing real a nivel de kernel — se puede
   agregar hoy como capa de nombres/config sobre el `WorkdirAllowlist` existente, y
   deja el terreno preparado para cuando llegue Landlock.

## Candidatos concretos de palanca de confiabilidad — prototipar, no slam-dunk

5. **"Laziness classifier"** (`laziness_classifier.rs`): un clasificador pasivo que
   corre en tiempo ocioso del agente, auditando la transcripción por señales de que
   el modelo se estancó o fabricó progreso — narración de una acción sin tool call
   que la respalde, pedir permiso a mitad de un paso obvio, afirmar que terminó sin
   evidencia de tool call, o TODOs visibles sin avance. Cruza esas afirmaciones contra
   hechos de runtime "a prueba de manipulación" (tareas en background pendientes,
   segundos transcurridos del turno) en vez de confiar en la prosa del modelo. Es un
   modo de falla real y conocido (afirmar con confianza que el trabajo está hecho
   cuando no lo está) que ninguna palanca actual de braze detecta específicamente —
   candidata genuina a nueva palanca, testeable con el mismo estilo de ablation que
   el resto (`+ablate:no-laziness-check` o similar).
6. **Edits ancladas por hash de contenido en vez de número de línea o
   búsqueda/reemplazo literal** (`grok_build_hashline/`), con "recuperación acotada
   para anclas desplazadas o desactualizadas". Más robusto que números de línea (que
   se desincronizan tras cada edit previo del mismo archivo) y menos frágil que
   texto exacto (que se rompe con reformateos triviales). Encaja directo con la
   propia historia de confiabilidad de braze (rescate textual, guardrail post-edit) —
   vale prototipar, no es una victoria automática (agrega complejidad de cómputo de
   anclas y necesita su propio fallback de recuperación).
7. **Modo plan con bloqueo de edición a nivel de kernel del harness, no solo por
   prompt** (`19-plan-mode.md`): en modo plan, cualquier tool call de edición fuera
   de `plan.md` se rechaza directamente sin importar la config de permisos, hasta que
   el agente llama `exit_plan_mode` y el usuario aprueba vía una vista dedicada
   (preview scrolleable + comentarios inline por línea). Más fuerte que un overlay de
   aprobación por tool-call individual — un "modo lectura/planificación" mecánicamente
   forzado, no solo instruido.
8. **Rewind a nivel de archivo, no solo de conversación** (`17-sessions.md`):
   `/rewind` restaura archivos desde snapshots (`rewind_points.jsonl`) tomados por
   cada prompt del usuario, y *además* trunca el historial de conversación para
   calzar. Vale verificar si el backtrack Esc-Esc de braze (según CLAUDE.md, rescata
   la conversación) también restaura el estado real de archivos — si no, es un hueco
   real: un "deshacer" completo es más útil que uno solo-conversación.

## La apuesta grande: Agent Client Protocol (ACP)

**Es el estándar abierto de Zed Industries, no propietario de xAI** (JSON-RPC 2.0
sobre stdio, "LSP para agentes de IA", open-sourced agosto 2025; JetBrains se está
sumando). grok-build es un *implementador cliente* de un estándar ajeno, no lo
inventó — para braze esto es una decisión de adoptar-vs-inventar, no un mecanismo
nuevo que diseñar desde cero.

- **3 transportes**: stdio JSON-RPC (`grok agent stdio`), modo servidor persistente
  (`grok agent serve --bind ... --secret ...`), y relay WebSocket para clientes
  remotos/browser.
- **Ciclo de sesión**: `initialize` (negociación de capacidades) → `session/new` →
  `session/prompt` → notificaciones `session/update` en streaming → pedidos de
  aprobación de tools. Es, en espíritu, muy parecido al loop interno de
  `braze-engine` — solo que nunca externalizado como superficie RPC para un tercero.
- **Taxonomía de eventos de streaming**: `agent_message_chunk`, `agent_thought_chunk`,
  `tool_call`, `tool_call_update`, `plan` — el equivalente estructurado de lo que hoy
  las tool-call cells de `braze-tui` renderizan directamente en el mismo proceso.
- **Métodos de extensión propios de xAI** bajo el namespace `x.ai/*` (filesystem,
  git, búsqueda, terminal, fork/resume de sesión, auth) — básicamente la superficie
  de tools que braze ya tiene, re-expuesta como RPC invocable por un editor externo
  en vez de solo por el engine in-process.
- **Clientes ya compatibles**: Zed, Neovim (plugins comunitarios), Emacs, marimo;
  JetBrains "próximamente". SDKs oficiales en TypeScript/Rust/Python/Go/Kotlin.
- No se encontró copia del schema JSON de ACP dentro del repo — probablemente viene
  de un SDK externo vendored que no fue enumerado en lo que se pudo listar.

**Por qué importa para braze específicamente**: el propio paper del Proyecto 1
posiciona a braze contra Claude Code/Codex/OpenCode/Gemini CLI como harnesses que
"documentan principios pero no publican resultados medibles/ablatables". "¿Este
harness habla un protocolo abierto de embedding en editores?" es un eje concreto y
verificable donde braze hoy pierde frente a ese conjunto de pares. Adoptar ACP (no
inventar un protocolo propio) probablemente cuesta bastante menos que el trabajo de
palancas de confiabilidad ya hecho — un crate nuevo (`braze-acp`) envolviendo
`braze-engine`, mapeando el `ToolProvider`/las tool-call cells existentes al stream
`session/update` de ACP.

## Confirma decisiones de braze ya buenas — no copiar a ciegas

9. **Multi-provider (`ModelBackend`) — braze está adelante, no atrás.** grok-build
   parece construido en torno a un solo modelo de primera parte (Grok):
   `default_models.json` tiene una sola entrada, y no se encontró un trait de
   backend con múltiples implementadores reales como el `Anthropic`/`Ollama`/
   `OpenRouter` de braze. La apuesta de grok-build es "un modelo de frontera,
   integrado a fondo" (detección de doom-loop del lado del servidor, acoplamiento
   fuerte al stack de inferencia propio); la de braze es "la calidad del harness
   compensa modelos intercambiables/chicos". Nada que portar acá — si algo, esto
   refuerza que la inversión en abstracción de proveedor de braze es un
   diferenciador real, no una deuda.
10. **Fork filosófico en permisos, no un hueco.** grok-build no tiene un clasificador
    Safe/Irreversible visible — en su lugar, `should_auto_allow_bash()` aprueba
    automáticamente cuando el sandbox del SO está activo: la apuesta es "el sandbox
    hace imposibles los malos resultados, no molestes al usuario". La de braze es
    "predecir qué acciones son peligrosas y preguntar antes" — funciona sin depender
    de Landlock/bwrap (portable), pero es probabilística (un clasificador puede
    equivocarse) donde la de grok-build es determinística una vez el sandbox está
    verificado activo. Apuestas distintas, no un "braze debería copiar esto".
11. **Cero documentación de "por qué" en grok-build** — se revisó README,
    getting-started y CONTRIBUTING específicamente buscando razonamiento de diseño
    (por qué 3 modos, por qué Rust, por qué ACP) y solo hay descripción funcional
    enumerativa, sin nada parecido al estilo CLAUDE.md/PLAN.md de braze. Esto es
    evidencia a favor del propio framing del paper (que critica a los harnesses
    pares por exactamente este patrón) — nada que portar, pero refuerza la
    contribución que el paper ya reclama.
12. **Rescate de tool-calls con JSON malformado: no se encontró equivalente en
    grok-build**, pese a cobertura razonablemente profunda de `xai-tool-runtime` y
    `xai-tool-protocol`. Podría deberse a que xAI empuja la prevención al decoding
    restringido del lado del servidor (control de ambos extremos: modelo + harness).
    No es una confirmación fuerte de que no exista — pero sugiere que la escalera de
    rescate textual de braze sigue siendo un diferenciador real frente a este par.

## Apuestas arquitectónicas grandes — para fases futuras, no ahora

13. **Un daemon "leader" único compartido por todas las superficies de cliente**
    (`xai-grok-shell/src/leader/`): un proceso persistente mantiene el estado del
    agente bajo `~/.grok/`, con TUI/extensiones de IDE/modo headless conectándose vía
    sockets Unix domain, `flock` para instancia única, y "eviction por versión"
    (clientes nuevos reemplazan leaders obsoletos). Esto habilita, por construcción,
    que una tarea en background lanzada desde la TUI se observe o continúe desde un
    cliente headless/ACP separado — algo que braze no puede hacer hoy (un proceso
    por invocación). Encaja directo con el propio principio de diseño de braze
    ("ejecución en background con notificación push, no polling") pero es una
    inversión grande (protocolo IPC, lógica de lock/eviction, reconexión) — no
    incremental.
14. **Subagentes paralelos con coordinador** (`subagent_coordinator.rs`): delegación
    N-way con contextos aislados (worktrees separados, canales de espera con
    timeout), cualitativamente distinto del par lead/executor de braze. Como el
    multi-agente ya está diferido a Fase 2 en el propio roadmap de braze, esto es un
    diseño de referencia útil para cuando esa fase arranque, no algo a portar ahora.
15. **Compactación de contexto en 3 niveles** (`xai-grok-compaction`): tail-keep
    por-paso (`intra_compaction`), chunked entre-turnos (`inter_compaction`), y
    regeneración completa vía resumen-LLM (`code_compaction`, propio de grok-build).
    El compactor único de braze (`ContextCompactor`) es una sola estrategia; separar
    "encoger por paso" de "encoger entre turnos" de "regenerar del todo" es un
    espacio de tradeoffs legítimo que braze no expresa hoy — no es un port directo
    (subsistema grande con sus propios templates de prompt), pero la idea de
    regeneración completa vía resumen podría valer para sesiones muy largas donde
    tail-keep pierde demasiado contexto temprano.
16. **Esquemas de tools versionados** (`read_file/versions/legacy_0_4_10.rs`, etc.):
    patrón para cambiar la forma de argumentos de una tool sin romper el replay de
    sesiones viejas. No urgente, pero vale recordarlo la próxima vez que cambie un
    schema de `braze-tools-local`.

## Qué no se pudo verificar

- No se encontró copia del schema/spec JSON de ACP en el repo (`xai-acp-lib`) —
  probablemente depende de un SDK externo no enumerado.
- No se confirmó integración con VS Code en ningún lugar fetcheado.
- No se pudo confirmar si existe un preview de diff como feature nombrada distinta
  (solo se infiere implícitamente de las tool calls de git/archivo).
- El algoritmo de backoff exacto en `retry_policy.rs` no apareció en lo fetcheado
  (el comentario lo menciona, la implementación no se vio).
- No se confirmó si `network_policy.rs` (política de red por-child-process) está
  realmente conectada a algo — el propio código dice explícitamente que no lo está
  todavía ("not selected by sandbox profiles or enforced by the current runtime").
- Todo se obtuvo vía el paso de resumen de WebFetch (no bytes crudos de GitHub) —
  tratar nombres de campos exactos (especialmente la lista de métodos de extensión
  ACP y el schema JSON de hooks) como paráfrasis a re-verificar contra la fuente
  primaria antes de implementar nada basado en ellos.

## Recomendación de priorización, si se decide actuar

Orden sugerido por costo/beneficio, no por importancia narrativa:

1. SSRF + retry/circuit-breaker (bajo costo, sin narrativa de paper, pura robustez).
2. Exit codes + `--output-format` de `braze run` (bajo costo, mejora DX de CI).
3. Verificar si el backtrack de braze restaura archivos o solo conversación — si no,
   cerrarlo es barato y valioso.
4. Decidir sobre ACP: es la única idea de esta lista con narrativa directa para el
   Paper 1 (un eje nuevo de comparación contra los pares) — vale una discusión
   explícita de alcance/costo antes de comprometerse, no una decisión de pasada.

## Estado de implementación (2026-07-17)

Decisión explícita del usuario: **ACP descartado** ("no es de mi interés" — ver
memoria de feedback, no re-proponer sin que lo pida). Al revisar el código real, 2 de
los 4 puntos de "ganancias baratas" resultaron distintos de lo esperado:

- **SSRF**: no aplica — braze no tiene ninguna tool que resuelva URLs (no hay
  `web_fetch` ni equivalente en `crates/braze-tools-local`). Nada que proteger todavía;
  recordar este ítem si alguna vez se agrega una tool HTTP.
- **Retry/clasificación de errores HTTP**: ya estaba a la par de grok-build antes de
  tocar nada — `crates/braze-model/src/retry.rs` (H-19) ya distingue transitorio
  (429+5xx, con backoff exponencial + jitter + `Retry-After`) de terminal (401/403/404,
  sin reintento), con tests que cubren exactamente esos casos. No había nada que
  arreglar ahí.
- **Circuit breaker — implementado** (`crates/braze-model/src/circuit_breaker.rs`):
  3 estados (Closed/Open/HalfOpen), ventana de conteo con `min_samples=5` y
  `error_rate_threshold=0.5`, `open_duration=30s`, reclamo de probe half-open
  abandonado. Registro global por clave `"{provider}:{url}"` (`OnceLock<Mutex<HashMap<...>>>`)
  para que una instancia de backend *nueva* (`braze-bench` construye una por tarea,
  ver `runner.rs`) siga viendo el estado de fallos de instancias anteriores apuntando
  al mismo destino, sin tocar la API de `BackendSpec::build()`. Envuelto en las 3
  implementaciones (`AnthropicBackend`/`OllamaBackend`/`OpenRouterBackend`) — Ollama
  también lo usa pese a no tener retry propio (el fallo ahí es agotamiento de
  recursos, no un blip transitorio, pero rastrear la tasa de fallos entre llamadas
  igual vale). Nueva variante `ModelError::CircuitOpen`. 8 tests nuevos + los 154
  existentes de `braze-model` verdes, clippy limpio en todo el workspace.
  **Limitación real, no un bug**: el estado es `static` (por proceso) — se comparte
  entre tareas dentro de una misma corrida de `braze-bench` (el caso de uso que lo
  motivó), pero NO entre invocaciones separadas de `braze run`/`braze chat` desde la
  shell (cada una es un proceso nuevo). Extenderlo a persistencia entre procesos sería
  una feature bastante más grande, no una "ganancia barata".
- **`--output-format json` de `braze run` — implementado** (`crates/braze-cli/src/
  cli_args.rs`, `main.rs`): flag `--output-format plain|json` (default `plain`, cero
  cambio de comportamiento para scripts existentes). En `json`, un
  `JsonSummaryObserver` acumula el texto (mismo contenido que `TextDeltaObserver`
  habría impreso) y suma tokens/ronda a través de los eventos `Usage`, imprimiendo un
  solo objeto al final (`text`, `session_id`, `input_tokens`, `output_tokens`,
  `rounds`, `stop_reason`) en vez de mezclar el stream con la línea humana
  `session: <id>`. Verificado en vivo contra Nitro. Exit codes (0/1) no se tocaron —
  ya coinciden con lo documentado para SIGINT/SIGTERM (130/143) vía la semántica por
  defecto del SO/shell cuando un proceso muere por señal, sin necesitar código nuevo;
  no verificado empíricamente todavía.
- **Rewind de archivos**: pendiente de verificar/implementar, no atacado en esta
  sesión.
5. Laziness classifier y hashline edits como candidatas a nueva palanca de
   confiabilidad — encajan en el patrón existente de "diseño pre-registrado, A/B,
   veredicto" que el propio framework de disciplina científica de braze ya exige.
