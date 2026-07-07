# Auditoría de capacidad SLM-first de `braze` — julio 2026 (v3)

> **Objetivo distinto al de v1/v2.** Las dos auditorías previas
> (`AUDITORIA-2026-07.md`, `-v2.md`) fueron *bug-hunts*: corrupción
> permanente de sesión (400 de Anthropic), escapes del allowlist, deuda de
> la TUI. Todos esos grupos (A–N) están cerrados. Esta tercera pasada
> pregunta otra cosa: **dado el objetivo declarado del proyecto — "ser el
> mejor harness agéntico para modelos pequeños" — ¿dónde subrinde braze
> *para un modelo chico específicamente*, medido contra las palancas SOTA
> que el propio proyecto dice implementar?** No es "¿hay bugs?" sino "¿un
> qwen2.5:3b / qwen3.5-coder puede realmente completar una tarea real, o
> el harness lo deja atrapado?".
>
> **Fecha:** 2026-07-06 · **Commit base:** `c7fc276` · **Cobertura:** los 13
> crates, con foco en la superficie que un modelo local toca (lectura/
> edición, contexto bajo `num_ctx=8192`, formato de tool-calls por familia,
> escalación/prompting, y el bench como instrumento del paper).
> **Método:** 5 agentes adversariales en paralelo, uno por dominio, cada uno
> briefeado con lo ya cerrado en v1/v2 para reportar solo gaps *nuevos*. Los
> hallazgos de severidad más alta se re-verificaron leyendo el código a mano
> (marcados ✔ verificado).

---

## 1. La tesis de esta auditoría, en una frase

**braze tiene una plomería *defensiva* SLM-first excelente y una superficie
*ofensiva* SLM-first con huecos sistemáticos.** Todo lo que evita que una
sesión se rompa —validez de protocolo, rescate de tool-calls malformadas,
reparación de args, ids únicos, idempotencia de compactación— está
genuinamente resuelto y es mejor que el de los pares OSS revisados. Pero lo
que hace que un modelo chico efectivamente *complete una tarea* —poder leer
un archivo mediano entero, tener contexto que quepa en 8192 tokens, que se le
hable en su plantilla de entrenamiento, y poder *medir* si cada palanca
ayuda— tiene gaps que se agrupan en tres patrones:

1. **La superficie de lectura/edición no soporta el archivo de tamaño
   típico.** `read_file` trunca a 8 KB (~200 líneas) sin paginación, y el
   steering "usa write_file con el contenido completo" presupone un contenido
   que el modelo nunca pudo obtener. El archivo mediano (>200 líneas) es un
   callejón sin salida, no el archivo enorme.
2. **La aritmética de contexto no está calibrada para `num_ctx=8192`.** Las
   constantes vienen literales de SWE-agent/ACI (GPT-4, contexto grande): 5
   observaciones completas de 8 KB ≈ 10k tokens no caben en 8192, y
   `max_tokens=4096` regala la mitad de la ventana a la salida.
3. **La inteligencia por-familia está toda del lado de la *salida*, ninguna
   del lado de la *entrada*.** braze parsea `<tool_call>` de Qwen y la
   gramática XML de qwen3-coder (reactivo, tras el fallo), pero le manda a
   *todos* los modelos el mismo system prompt genérico y el mismo formato de
   tools. Es exactamente al revés de donde la tesis del propio proyecto
   (Qwen3-Coder TR: los modelos chicos sobreajustan a su tool-template)
   predice el mayor retorno.

Y el meta-hallazgo que envuelve a los tres: **el bench, el instrumento que
debería demostrar que cada una de estas palancas importa, no puede correr
ablaciones** — las dos palancas que la literatura señala como de mayor efecto
en modelos chicos (matching de edición de Aider, colapso de observaciones de
SWE-agent) son precisamente las que están hardcodeadas siempre-on sin knob. La
"curva harness-vs-escala por skill" que es la contribución publicable no se
puede producir con el bench actual.

### Conteo de hallazgos

| Cluster | Hallazgos | Severidad máxima |
|---------|:---:|---|
| **A · Superficie lectura/edición** | 5 | CRÍTICA |
| **B · Contexto bajo `num_ctx` chico** | 5 | ALTA |
| **C · Tool-calling por familia** | 6 | ALTA |
| **D · Prompting y escalación** | 7 | ALTA |
| **E · Validez del bench (el paper)** | 6 | CRÍTICA (para el paper) |
| **F · Profundidad en wires/engine** | 10 | ALTA |

Ninguno solapa con los grupos A–N ya cerrados; son gaps de capacidad, no
regresiones de los fixes previos (los fixes de v2 se sostienen). El Cluster F
proviene de dos pasadas de mayor profundidad sobre `braze-model` y
`braze-engine` que extienden los clusters A–D con hallazgos más finos en los
wire parsers y el runtime del loop; varios **corroboran de forma
independiente** hallazgos de los clusters de dominio (notablemente D4, que
tres agentes distintos encontraron por separado).

---

## 2. Cluster A — La superficie de lectura/edición: la trampa del archivo mediano

> El cluster de mayor severidad, y el más corroborado: dos agentes
> independientes (edición y contexto) convergieron en el mismo hallazgo raíz.

### A1 · [CRÍTICA] `read_file` trunca a 8 KB sin paginación → whole-file imposible y `edit_file` limitado a los primeros 8 KB — ✔ verificado

- **Ubicación:** `braze-tools-local/src/read_file.rs:20` (lee el archivo
  entero con `read_to_string`), `provider.rs:261,283` (`wrap` aplica
  `truncate_output`, `MAX_TOOL_OUTPUT_BYTES = 8_000`), `schema.rs:63-73`
  (`read_file` solo acepta `{path}`, sin `offset`/`limit`).
- **Escenario SLM:** un ejecutor 3-7B necesita editar un archivo de 30 KB
  (tamaño típico — el propio `edit_file.rs` tiene 19 KB, `provider.rs` 30 KB).
  Llama `read_file` → recibe **solo los primeros 8 KB** (~200 líneas) + un
  trailer. Ahora está atrapado: `edit_file` solo puede copiar `old_string`
  exacto de lo que *vio* → únicamente puede editar dentro de los primeros
  8 KB; cualquier edición más profunda falla el matching. Y `write_file`
  exige "el contenido COMPLETO" (`schema.rs:40-42`) — que el modelo **jamás
  recibió**. No es una cuestión de tamaño de ventana: es estructuralmente
  imposible. La trampa se dispara en el archivo **mediano típico**, no en el
  enorme.
- **Por qué el steering whole-file (ya implementado, correcto según Aider)
  no basta:** presupone que el modelo *tiene* el contenido completo. Sin
  lectura paginada, ni whole-file ni search/replace tienen salida en un
  archivo >200 líneas.
- **Fix:** añadir `offset`/`limit` (por líneas) a `read_file`, estilo viewer
  paginado de SWE-agent (100 líneas con scroll), y que el trailer de
  truncación indique el offset para continuar (`"archivo de N líneas,
  mostrando 1-200; relee con offset=200"`). **Es el prerequisito faltante
  para que toda la superficie de edición sea usable con contexto chico.**

### A2 · [ALTA] `write_file` sobre un archivo truncado destruye el resto sin warning — seguir el propio steering corrompe datos

- **Ubicación:** `braze-tools-local/src/write_file.rs:23` (`tokio::fs::write`
  sobrescribe entero, cero verificación previa), interactúa con A1 y con el
  steering de `edit_file.rs:46` / `schema.rs:40`.
- **Escenario SLM:** el modelo leyó 8 KB de un archivo de 30 KB, obedece el
  steering ("usa write_file con el contenido completo"), reconstruye "el
  archivo completo" a partir de lo que vio (8 KB) y llama `write_file`. Se
  sobrescriben 30 KB con 8 KB; los otros 22 KB desaparecen, `is_error:
  false`. El guardrail post-edit no lo atrapa salvo que el borrado rompa la
  compilación Rust; en `.md`/`.json`/`.py`/config es invisible.
- **Fix:** `write_file` sobre un archivo existente cuyo tamaño previo es
  mucho mayor que `content.len()` debe advertir en el tool result (o requerir
  un flag explícito de overwrite-shrink); y no emitir el steering whole-file
  cuando `read_file` truncó.

### A3 · [MEDIA-ALTA] El error de matching fallido no muestra las líneas cercanas que sí existen

- **Ubicación:** `braze-tools-local/src/edit_file.rs:101-119`
  (`MatchFailure::into_message` para `NotFound` devuelve "no encontrado" +
  steering, sin ninguna línea real del archivo).
- **Escenario SLM:** Aider midió 9× menos fallas con aplicación difusa +
  *buenos errores*; el "buen error" es mostrar el fragmento más cercano que
  sí existe ("did you mean"). El código ya calcula ventanas de líneas
  (`find_line_window`, `edit_file.rs:171-199`) pero no las usa en el error.
  Un modelo chico recibe "no está" y queda sin material para autocorregirse
  (y el steering a write_file es un callejón sin salida por A1).
- **Fix:** en `NotFound`, un segundo pase que encuentre la línea de menor
  distancia a la primera línea de `old_string` y la adjunte con su número.

### A4 · [MEDIA] `edit_file` sobre un archivo inexistente da un error de I/O crudo sin steering a `write_file`

- **Ubicación:** `edit_file.rs:58-60` (`read_to_string` falla con "failed to
  read '{path}': No such file or directory").
- **Escenario SLM:** un modelo chico que quiere *crear* un archivo prueba
  `edit_file` (formato familiar) y recibe un error de OS que no le dice que
  debe usar `write_file`. Contrasta con la riqueza del error de matching.
- **Fix:** cuando `read_to_string` falla con `NotFound`, steerear: *"'{path}'
  no existe. Para crear un archivo nuevo usa write_file con su contenido."*

### A5 · [BAJA] Dos residuos de la edición difusa

- **CRLF→LF silencioso** (`edit_file.rs:176,238`): en un archivo `\r\n`, las
  rungs difusas usan `str::lines()` (descarta `\r`) y reensamblan con
  `join("\n")` → **todo** el archivo pasa a LF, no solo la región editada.
  Relevante por ser harness genérico (archivos de origen Windows).
- **Trailer de truncado copiable** (`provider.rs:299-304`): el trailer
  `"[output truncated…]"` se adjunta al contenido; un modelo chico puede
  copiarlo (o una línea parcial del borde de corte) dentro de `old_string`.
  Cortar en borde de línea, no de byte.
- **Nota (no es un gap):** `read_file` sin numeración de líneas es
  **correcto** — `edit_file` hace matching exacto de substring y un prefijo
  `"42: "` rompería el match. No migrar a numeración ingenua.

---

## 3. Cluster B — El contexto bajo `num_ctx` chico

> Números base verificados: `ollama_num_ctx=8192`, `max_tokens=4096`
> (`config.rs:171,175`) → presupuesto efectivo de prompt = `8192−4096−1024 =
> 3072` tokens (`prompt.rs:55,63-67`). Cap por output = 8000 bytes ≈ 2000
> tokens. Se conservan 5 observaciones completas (`TACTICAL_FULL_OBSERVATIONS`,
> `history.rs:48`).

### B1 · [ALTA] No hay cap AGREGADO sobre la cola de observaciones completas — 5×8 KB revientan `num_ctx=8192`

- **Ubicación:** `history.rs:48` (`TACTICAL_FULL_OBSERVATIONS=5`) +
  `engine.rs:1380` (gate `compaction_would_help = tactical.len() >
  KEEP_RAW_TAIL`).
- **Escenario SLM:** el cap de 8 KB protege *un* output, pero el renderer
  conserva las **últimas 5 observaciones en full** = 5 × 8000 ≈ **10.000
  tokens**, que exceden el presupuesto efectivo (3072) *y* el `num_ctx`
  entero (8192). No hay presupuesto agregado sobre el conjunto retenido. Peor:
  con `tactical.len() ≤ KEEP_RAW_TAIL(6)`, el gate **desactiva** la
  compactación, así que 6 observaciones de 8 KB (~12k tokens) se mandan
  enteras a un modelo de 8192 → truncación silenciosa desde el frente en
  Ollama (pierde system prompt + tools). El "5" viene literal de
  SWE-agent/ACI (GPT-4, contexto grande) sin reescalar a `num_ctx` chico.
- **Fix:** escalar `TACTICAL_FULL_OBSERVATIONS` y/o `MAX_TOOL_OUTPUT_BYTES` en
  función de `context_budget_tokens`, o añadir un cap agregado por bytes sobre
  la cola de observaciones full (colapsar de más viejo a más nuevo hasta
  caber).

### B2 · [MEDIA-ALTA] El estimador mide la táctica en CRUDO, no en la forma colapsada que realmente se envía

- **Ubicación:** `engine.rs:1912-1917` (`estimate_prompt_tokens`) usa
  `estimate_dropped_tokens(tactical)` (`engine.rs:1873-1876`, cuenta
  `result.content.len()` completo por observación).
- **Escenario SLM:** asimetría con el renderer. El lado durable se mide en la
  forma *cleared* (fix N-6, correcto), pero el táctico se mide en crudo aunque
  `build_messages` lo colapse a 1 línea. Con 10 observaciones de 8 KB crudas
  ≈ 20k estimados dispara compactación, pero el prompt renderizado (5 full +
  5 colapsadas) es ~10k → **sobre-compacta**, tirando contexto crudo
  prematuramente en la ventana ya diminuta. Es el espejo de B1.
- **Fix:** estimar la táctica a través de la misma lógica de colapso (como ya
  se hace para el durable vía `render_durable_events`).

### B3 · [MEDIA] No existe restauración post-compactación de los N archivos recientes

- **Ubicación (ausencia):** sin lógica de restore/re-read en `engine.rs` ni
  `braze-session/`; en compactación un `read_file` se reduce a su path
  (`simple_compactor.rs:131-151`) y el contenido se reemplaza por el
  placeholder "cleared" (`history.rs:324-328`). `NEVER_CLEAR_TOOLS` está vacío
  (`history.rs:37`).
- **Escenario SLM:** el préstamo de Qwen Code ("restauración de los 5 archivos
  más recientes, muy valioso con num_ctx=8192", SOTA doc) sigue pendiente. Con
  contexto chico la compactación llega temprano y seguido; cada vez que pliega
  un `read_file` el modelo debe re-leerlo — y re-leer vuelve a truncar a 8 KB
  (A1). Pierde el archivo en el que estaba trabajando justo cuando el
  presupuesto lo obliga a compactar.
- **Fix:** re-inyectar verbatim (hasta el cap) el contenido de los últimos N
  `read_file`/`edit_file` distintos por path como bloque durable exento del
  clearing (usar el gancho `NEVER_CLEAR_TOOLS` existente).

### B4 · [MEDIA] El margen fijo de 1024 tokens no crece con las tools MCP, y el estimador ignora system prompt + definiciones de tools

- **Ubicación:** `prompt.rs:55` (`CONTEXT_BUDGET_MARGIN_TOKENS=1024`,
  constante) + `engine.rs:1912-1917` (el estimador mide solo
  `summary+durable+tactical`, no el system prompt ni las definiciones de
  tools).
- **Escenario SLM:** el system prompt (~250 tokens) y las definiciones de
  tools se cubren con un margen plano de 1024. Con las 6 tools locales alcanza
  raspando, pero `mcp_servers` agrega una definición por cada tool MCP, **sin
  cota y sin contarse**. Suficientes tools MCP → overhead real > 1024 → el
  prompt excede `num_ctx` aunque el estimador diga "bajo presupuesto" →
  truncación silenciosa. Además el estimador es `chars/4` sobre bytes
  (`engine.rs:1934`), y para JSON/código los tokens son más densos (~3.3
  chars/token) → **subestima ~20%**, la dirección peligrosa (overflow).
- **Fix:** derivar el margen del largo real del system prompt + suma de bytes
  de los stubs de tools resueltos; usar un factor ~3.5 chars/token para el
  lado JSON.

### B5 · [MEDIA, solo config] `max_tokens=4096` sobre `num_ctx=8192` regala la mitad de la ventana a la salida

- **Ubicación:** `config.rs:171,175` (defaults).
- **Escenario SLM:** reservar 4096 de 8192 para salida deja el presupuesto de
  prompt en 3072. Un ejecutor que emite JSON de tool-call rara vez necesita
  >500 tokens de salida; bajar `max_tokens` a ~1024 casi duplica el
  presupuesto de prompt (`8192−1024−1024=6144`) y alivia B1/B2 **sin tocar
  código**. El binding real en Ollama es el presupuesto de tokens (ya bien
  cableado, `main.rs:389`), no el conteo de eventos — así que `threshold=40`
  es en gran parte inerte para Ollama; el problema de defaults es `max_tokens`.
- **Fix:** revisar el default de `max_tokens` para el perfil SLM, o
  documentar `max_tokens` bajo como recomendado con `num_ctx` chico.

---

## 4. Cluster C — Tool-calling por familia de modelo chico

### C1 · [ALTA] Tools MCP: se muestra el schema permisivo pero se valida contra el estricto — trampa para SLMs — ✔ verificado

- **Ubicación:** `backend.rs:50-55` (`permissive_fallback_schema` =
  `{additionalProperties:true}`), aplicado en `anthropic_wire.rs:162`,
  `ollama_wire.rs:262`, `openrouter_wire.rs:255` para stubs con
  `input_schema:None`; los stubs MCP siempre son `None`
  (`braze-mcp-client/src/provider.rs:262`). La validación en dispatch resuelve
  el schema **real y estricto** (`engine.rs:1072-1075` +
  `jsonschema::validate`).
- **Escenario SLM:** un qwen2.5:3b ve `name + summary +
  {additionalProperties:true}` para una tool MCP → sin señal de `required`/
  nombres exactos, inventa campos (ToolScan: Vicuna-13B 40% de acierto en
  nombres de args). Luego se valida contra `additionalProperties:false` +
  required que nunca vio → falla, quema un turno (recurso escasísimo con
  `num_ctx=8192`), y recién en el reintento ve el schema real. Con presupuesto
  de turnos corto puede no converger. **Acotado a MCP** — las tools locales ya
  envían el schema real up-front (`schema.rs`, correcto).
- **Fix:** cachear el schema resuelto y promoverlo al stub en turnos
  siguientes (una tool ya tocada tiene schema conocido y barato de incluir);
  o, mínimo, incrustar los nombres de parámetros `required` en el `summary`
  del stub MCP. Rompe la carga diferida solo para tools ya usadas, no para el
  set completo.

### C2 · [ALTA] El rescate textual no cubre el formato "pythonic" de la familia Llama

- **Ubicación:** el rescate cubre taggeado `<tool_call>` (qwen2.5/Hermes),
  XML `<function=>` (qwen3-coder) y JSON desnudo (`engine.rs:1642-1773`); no
  hay rama para el formato pythonic `[func(arg=val)]` / `func(arg="x")`, que
  es el formato **nativo** de tool-calling de Llama 3.1/3.2 — uno de los SLMs
  más instalados vía Ollama.
- **Escenario SLM:** un usuario corre `llama3.2:3b`; cuando el template no se
  honra end-to-end, el modelo emite `[read_file(path="informe.txt")]` como
  texto. Ninguno de los tres extractores matchea → el "tool call" se persiste
  como respuesta final de texto y el turno termina ignorando silenciosamente
  la llamada. Es el hallazgo B5 (rescate) reintroducido para la familia Llama.
- **Fix:** `extract_pythonic_tool_calls` en la escalera (entre XML y JSON
  desnudo): parsear `[nombre(k=v,…)]`, mismo contrato que los otros
  (bloques removidos, prosa preservada, malformados visibles).

### C3 · [MEDIA] El rescate de JSON desnudo rechaza tres variantes comunes en SLMs

- **Ubicación:** `parse_tool_call_json` (`engine.rs:1666-1681`) exige clave
  exactamente `name` top-level y `arguments`/`parameters` que sea objeto
  (`is_object()`, :1673). Se pierden: (1) arguments doble-codificados como
  string `{"name":"read_file","arguments":"{\"path\":\"x\"}"}`; (2) claves
  alternativas `{"tool":...,"tool_input":{}}` (ReAct/LangChain) o el anidado
  OpenAI `{"function":{"name":...,"arguments":{}}}`; (3) array de varias calls.
- **Fix:** aceptar `arguments` string (vía `parse_arguments_with_repair`),
  probar claves `tool`/`tool_name`/`function.name`, y mapear arrays top-level.

### C4 · [MEDIA] El "contexto de reparación" del reintento no incluye un ejemplo, y el 2º intento pierde el schema — ✔ verificado

- **Ubicación:** `engine.rs:1089-1103`. El intento 1 **sí** incluye el schema
  real (bien — no es "solo el error crudo"), pero (a) sin un ejemplo de
  invocación válida (un 3B interpreta mejor `Ejemplo: {"path":"..."}` que un
  JSON Schema serializado), y (b) el intento 2 **elimina el schema** y solo
  dice "no more hints" — si el modelo casi acertó, se queda sin la referencia
  justo cuando más la necesita. Además el contador va por *nombre* de tool, no
  por call (`engine.rs:1085`): un modelo que llama la misma tool dos veces con
  args distintos se auto-castiga.
- **Fix:** sintetizar un ejemplo mínimo desde `required`+`properties` y
  adjuntarlo en el intento 1; mantener el schema en el intento 2.

### C5 · [MEDIA/BAJA] Ollama —el backend primario de SLMs— no pasa los args stringificados por la escalera `args_repair`; colapsa a `{}`

- **Ubicación:** `ollama_wire.rs:414-420` — cuando `arguments` llega como
  `Value::String(s)` usa `serde_json::from_str(s).ok()` y ante fallo cae a
  `json!({})`. Anthropic/OpenRouter enrutan por `parse_arguments_with_repair`
  (rung de reparación de truncamiento antes del colapso). El SOTA doc dice que
  la escalera se aplicó "en ambos wires (Anthropic y OpenRouter)" — **Ollama
  quedó fuera**, y es justo el backend de los modelos chicos objetivo.
- **Fix:** reemplazar `from_str(s).ok()` por `parse_arguments_with_repair(s)`
  (exponerlo `pub(crate)` cross-módulo), unificando la escalera en los tres
  wires.

### C6 · [BAJA] Drop silencioso de tool call sin `name` en Ollama

- **Ubicación:** `ollama_wire.rs:405-407` — `tool_call_from_json` devuelve
  `None` si falta `function.name`, descartado silenciosamente aguas arriba.
  Distinto de los extractores textuales, que preservan malformados como texto
  visible.
- **Fix:** emitir la call con un name placeholder inválido para que caiga en
  la rama `NotFound` (`engine.rs:1129`, que sí retroalimenta "Unknown tool,
  available: …"), o al menos loguear `warn`.

---

## 5. Cluster D — Prompting y escalación (el corazón de la tesis del harness)

### D1 · [ALTA — el de mayor valor conceptual] Cero adaptación del prompt/formato POR FAMILIA; la única lógica family-aware es reactiva (parsing), nunca proactiva (shaping) — ✔ verificado

- **Ubicación:** `braze-config/src/prompt.rs:30` — `default_system_prompt(cwd:
  &Path)` **no recibe el modelo**. Un solo string genérico para qwen2.5,
  qwen3-coder, llama3.x, deepseek, Anthropic. La serialización de tools es por
  wire-protocol, no por familia. La ÚNICA lógica family-specific del repo es de
  *salida* (rescate `<tool_call>`/`<function=>`, `engine.rs:418`), y encima
  aplicada uniforme a todos (a un Llama se le intentan los tags de Qwen).
- **Por qué importa:** la tesis del propio proyecto (Qwen3-Coder TR: los
  modelos chicos "sobreajustan MÁS duro a su tool-template de entrenamiento";
  format-following de GPT-5-2 oscilando 84.0→14.0 según scaffold; SOTA doc
  etiqueta "matchear prompt/formato a la familia" como *máxima palanca*) dice
  que un 3B rinde radicalmente distinto según se le hable en SU plantilla.
  braze le da a un qwen2.5:3b el mismo prompt/formato que a Claude. La palanca
  de mayor retorno está implementada solo del lado del parser, **al revés** de
  donde la tesis la ubica.
- **Fix:** `fn system_prompt_for(model: &str, cwd)` + selección de estilo de
  tool-hint por familia, derivado del nombre del modelo Ollama. Mínimo: para
  Qwen, mover el ejemplo de `<tool_call>` del rescate reactivo a una pista
  proactiva en el system prompt. Cambio de `braze-config`/`braze-model`, sin
  tocar el grafo de dependencias del engine.

### D2 · [ALTA] Los knobs de sampling están cableados SOLO al bench, no a la CLI de producción — ✔ verificado

- **Ubicación:** `braze-cli/src/main.rs:143-147` — la rama Ollama de
  `build_model_backend` solo llama `with_base_url(...).with_num_ctx(...)`;
  ningún `with_temperature/top_p/top_k/repeat_penalty`. `config.rs` **no tiene
  campos de sampling** (grep: cero). El bench sí los aplica
  (`backend_spec.rs:207`).
- **Escenario SLM:** el sweep A/B de sampling puede encontrar el óptimo de
  Qwen (temp 0.7/top_p 0.8/top_k 20/rep 1.05) pero ese óptimo **es
  inaplicable en `braze chat`/`braze run`**: producción queda clavada en
  `DEFAULT_TEMPERATURE=0.2` (`ollama.rs:33`) y el resto en `None`. Es el
  equivalente de N-36 (system prompt) pero para sampling — el proyecto ya
  invirtió en construir los `with_*` y no los conectó a producción.
- **Fix:** agregar `ollama_temperature/top_p/top_k/repeat_penalty` a `Config`
  + flags, cablearlos en `build_model_backend`. Trivial, sin dependencias.

### D3 · [ALTA] El detector de escalación no distingue falla-del-modelo de falla-del-entorno

- **Ubicación:** `escalation.rs:175-199` (`trailing_failed_observations`
  cuenta cualquier `ToolResult{is_error:true}`) + `tools-local/provider.rs:261`
  (`wrap` marca `is_error:true` para *todo* `Err`, incluyendo causas
  ambientales: `read_file` a ruta inexistente, `shell_exec` exit≠0, `grep` sin
  match). `DEFAULT_FAILURE_THRESHOLD=2`.
- **Escenario SLM:** un worker que explora legítimamente y encadena dos
  resultados negativos del entorno (leer un archivo que no existe, luego un
  grep sin match) cruza el umbral y escala al modelo caro — gasto provocado
  por el entorno, no por incapacidad del worker. Peor: el nudge A5, la
  reparación de schema y "already called" también marcan `is_error:true`, así
  que el detector mezcla señales de naturaleza distinta.
- **Fix:** propagar la causa hasta el `ToolResult` (variante/campo que
  distinga `EnvironmentError` de `MalformedCall`/`SchemaValidation`) y contar
  solo las atribuibles al modelo. Alternativa barata: excluir de la cuenta los
  `is_error` de entorno (exit codes, not-found).

### D4 · [MEDIA] G10 (best-of-n) × EscalatingBackend interactúan mal: cada candidato cuenta como un turno de escalación

- **Ubicación:** `engine.rs:502` (`complete_with_best_of_n` llama
  `complete_once` N veces) → cada una entra a `EscalatingBackend` →
  `state.calls += 1` (`escalation.rs:106`).
- **Escenario SLM:** con `best_of_n=5` y `lead_turns=3` (default), en el
  *primer* round los candidatos 1-3 van al lead y 4-5 al worker — el "lead abre
  la sesión" se agota dentro de un solo round de votación, y el voto de
  plurality mezcla candidatos de **modelos distintos** como intercambiables.
  Es precisamente la combinación "todas las palancas para el modelo chico".
- **Fix:** contar turnos de escalación a nivel de round, no de candidato (una
  sola decisión de `route` por round). Documentar que hoy no son composables.

### D5 · [MEDIA] El nudge A5 es intra-turno; el loop de narración cross-turn —el modo de falla nominal del proyecto— no lo detecta ninguna palanca de código

- **Ubicación:** `engine.rs:1031` (`seen_calls` local a `dispatch_tool_calls`,
  se reinicia cada `run_turn`). El modo de falla que motivó todo el prompt
  anti-loop (`prompt.rs:26-29`: "kept restating the plan across several turns…
  without ever emitting the write_file call") es explícitamente **cross-turn y
  con cero tool calls** — A5 nunca se activa (no hay llamada que repita) y el
  detector de escalación tampoco (no hay `is_error`). La única defensa es el
  bullet en prosa del system prompt, es decir, se depende íntegramente de la
  comprensión del 3B — la debilidad que el harness debía compensar.
- **Fix:** detección de "narración sin acción" a nivel de turno (si N turnos
  consecutivos producen solo `AssistantText` tras una petición imperativa,
  inyectar un nudge estructural o un round forzado). Medible en el bench.

### D6 · [MEDIA] `best_of_n` no está gateado por backend: contra Ollama en CPU multiplica la latencia N×

- **Ubicación:** `main.rs:381` (`.with_best_of_n` incondicional, sin mirar
  `default_backend`) — contrasta con el budget de contexto justo debajo
  (`main.rs:389`, sí gateado `if default_backend=="ollama"`). Default
  `best_of_n:1` (apagado, bien), pero overridable por env sin advertencia.
- **Escenario SLM:** un usuario lo sube a 5 pensando "más robusto" → ×5 sobre
  90-100s/tarea = 450-500s, sesión inservible, sin warning. G10 rinde en cloud
  barato/paralelo, no en un 3B serializado en CPU.
- **Fix:** `warn` (o clamp) cuando `best_of_n>1` y el backend es Ollama.

### D7 · [MEDIA-BAJA] System prompt y feedback del harness en inglés mientras el modelo chico interactúa en español

- **Ubicación:** `prompt.rs:32-47` (system prompt en inglés); nudge A5
  (`engine.rs:1044`), reparación de schema (`engine.rs:1091`) y planning
  prompt (`engine.rs:1850`) también en inglés. El caso real documentado
  (`prompt.rs:27`) es qwen2.5:3b respondiendo en español.
- **Escenario SLM:** los modelos chicos tienen instruction-following
  cross-lingual notablemente más débil; un system prompt + feedback loop en
  otro idioma que la conversación es degradación evitable (no verificada
  cuantitativamente).
- **Fix:** idioma del prompt/mensajes configurable o derivado del locale.

> **Verificado limpio (no son gaps):** la *longitud* del system prompt (~180
> palabras, 5 reglas imperativas, sin few-shot) está bien calibrada para un 3B
> con ICL débil; el planner es limpiamente opt-in (`engine.rs:803`,
> `main.rs:395` — la ruta default no paga nada); el texto del nudge A5 es
> concreto y accionable.

---

## 6. Cluster E — Validez del bench como instrumento del paper

> La contribución publicable es "la curva harness-vs-escala por skill". El
> bench actual no puede producirla. Estos gaps son sobre *evidencia*, no sobre
> el runtime de producción.

### E1 · [CRÍTICA para el paper] No existe infraestructura de ablación de palancas — ✔ verificado

- **Ubicación:** `runner.rs:132-167` — todas las palancas se leen de un
  **único** `Config` global cargado una vez (`main.rs:122`), compartido por
  todos los backends del sweep. El `struct Cli` (`main.rs:30-95`) no expone
  ningún flag de palanca. **Togglables solo vía env global:** `best_of_n`,
  `disable_textual_tool_call_rescue`, `disable_post_edit_check`,
  `tactical_window/threshold`. **No togglables en absoluto (hardcoded
  siempre-on):** el colapso de observaciones (`TACTICAL_FULL_OBSERVATIONS`,
  `const`, `history.rs:48`) y la escalera de matching de `edit_file`
  (`edit_file.rs:14-15,122,341`) — es decir, **las dos palancas que la
  literatura señala como de mayor efecto en modelos chicos** (Aider: formato
  de edición; SWE-agent: colapso de observaciones).
- **Consecuencia:** la curva harness-vs-escala no se puede producir en una
  corrida; las palancas que sí se apagan solo lo hacen globalmente (otro
  sweep, otro env), no lado a lado ni por-backend. El split planner **sí** es
  per-spec (sufijo `+plan:`, `backend_spec.rs:45-65`) — demuestra que el
  patrón correcto ya existe en el código; ninguna otra palanca lo adopta.
- **Fix:** promover cada palanca a un sufijo de spec componible (estilo
  `+plan:`) o a una matriz de ablación en el `Cli` que envuelva el `Config`
  por-run, de modo que un sweep emita `{backend × palanca_on/off × skill}`.
  **Prerequisito de infraestructura #1 para el paper.**

### E2 · [ALTA] No hay baseline externo — el bench solo mide braze contra sí mismo

- **Ubicación:** `backend_spec.rs:22-27` (solo anthropic/ollama/openrouter),
  `runner.rs:132` (siempre envuelto en `braze_engine::Engine`). El SOTA doc
  declara mini-swe-agent como "ancla superior y baseline obligado" — el bench
  no tiene punto de enganche. Toda comparación es braze-vs-braze.
- **Fix:** trait `ExternalHarness` que reciba `(TaskDef, sandbox)` y devuelva
  el mismo `TaskResult` (mismo sandbox, mismo matching), con adaptador
  mini-swe-agent. Sin ancla no hay eje vertical de la curva.

### E3 · [ALTA] Poder estadístico nulo por-skill; tareas de edición sub-representadas

- **Ubicación:** `suites/default.toml` — 10 tareas: `single_tool`:6,
  `no_tool`/`multi_step`/`error_recovery`/`distractor_selection`: **1 cada
  uno**. Cuatro de cinco skills con **n=1**. El reporte por-skill
  (`report.rs:243-256`) imprime `passed/total` crudo sin intervalo. La
  edición (donde Aider dice que el formato importa) está en ~2 tareas de 10.
  Dos single_tool (`grep_basic`, `shell_exec_basic`) solo verifican
  `expect_tool_call` sin verificar el resultado.
- **Fix:** la "suite ampliada de edición" que el SOTA doc ya marca pendiente,
  con ≥5-8 tareas por skill y peso en editing. Sin poder estadístico por-skill
  la contribución no es defendible ante un revisor.

### E4 · [ALTA] F10 sigue abierto: matching de éxito por substring; falso positivo real en `error_recovery` — ✔ verificado

- **Ubicación:** `metrics.rs:260` (`final_text…contains(expected)`),
  `runner.rs:206-209` (archivos, `contents.contains(expected_substring)`).
- **Falso positivo concreto (verificado en la suite):**
  `error_recovery_wrong_filename` (`default.toml`) usa el archivo
  `informe_final_v2.txt` (contenido de 2 líneas) y espera
  `expect_text_contains="2"`. El **nombre del archivo contiene `v2`** → un
  modelo que eche el nombre y cuente **mal** ("el archivo informe_final_v2.txt
  tiene 5 líneas") produce texto con `"2"` y **pasa** pese a la respuesta
  incorrecta. Igual riesgo en `read_file_basic` (`"3"`) y `no_tool_qa`
  (`"4"`). Y `error_recovery` es uno de los skills que sostienen la curva.
- **Fix:** igualdad normalizada o word-boundary para respuestas numéricas
  cortas; comparar el token numérico aislado.

### E5 · [MEDIA] El tradeoff costo/calidad no se reporta por-skill

- **Ubicación:** `TaskResult` sí registra `wall_time_ms`/tokens/`rounds`
  (`metrics.rs:92-106`) y el reporte por-*backend* los muestra, pero el reporte
  por-*skill* (`report.rs:243-256`) imprime solo `passed/total`. La celda que
  el paper necesita (¿el harness sube `multi_step` a costa de 3× rounds?) no se
  emite, aunque el dato crudo está en el JSON. Sin p90/p99 (para el argumento
  de costo la cola importa más que la mediana).
- **Fix:** extender el bloque por-skill con median_ms/avg_rounds/avg_tokens
  (solo agregación del dato ya disponible).

### E6 · [MEDIA→ALTA para reproducibilidad] El JSON no emite metadata de corrida — ✔ verificado

- **Ubicación:** `report.rs:264-269` serializa un `Vec<TaskResult>` desnudo.
  Ningún campo contiene temperatura/seed/sampling, valores de las palancas
  activas, digest del modelo Ollama (solo el tag móvil `latest`), hash del
  suite, timestamp ni commit de braze. Evidencia del daño: en
  `docs/sweep-nitro-sampling-2026-07-06/`, las variantes t02 vs rec se
  distinguen **solo por el nombre del archivo** — el JSON es indistinguible.
- **Fix:** un header `RunMetadata` (sampling completo, palancas activas,
  digest vía `/api/show`, hash del suite, commit). Condición estándar de
  reproducibilidad de software papers; combinar ramas de ablación (E1) lo
  exige.

---

## 6.5. Cluster F — Profundidad en wire parsers y runtime del loop

> Dos pasadas de mayor profundidad sobre `braze-model` (15 archivos) y
> `braze-engine` (runtime completo), verificando cada hallazgo contra el flujo
> real hasta los consumidores. Extienden A–D con precisión adicional. Los ✔
> los re-verifiqué a mano.

### F1 · [ALTA] El rescate tagged/XML ejecuta ejemplos incrustados en prosa y dentro de fences de markdown; y `parameters` acepta *definiciones* de tools estilo OpenAI — ✔ verificado (extiende C3, safety)

- **Ubicación:** `engine.rs:426-450` (escalera), `1700-1739`
  (`extract_tagged_tool_calls`), `1747-1773` (XML), `1666-1681`
  (`parse_tool_call_json`).
- **Escenario:** N-15 (cerrado con flag) cubría solo el JSON desnudo, que
  exige que *toda* la respuesta sea el JSON. Los dos formatos nuevos post-v2
  admiten prosa alrededor y **no son conscientes de fences**: pedirle al modelo
  "explícame cómo Qwen emite tool calls" y que responda un ejemplo
  ` ```<tool_call>{"name":"read_file","arguments":{"path":"/etc/shadow"}}</tool_call>``` `
  hace que `extract_tagged_tool_calls` lo encuentre (`find(OPEN_TAG)` sobre
  texto crudo, el fence no protege) y **lo ejecute** (las tools de lectura no
  confirman). Agravante nuevo: `parse_tool_call_json` acepta `parameters` como
  sinónimo de `arguments`, así que una **definición** de función OpenAI
  (`{"name":"get_weather","parameters":{"type":"object","properties":{…}}}`) —
  la forma más común en documentación de tool-calling — pasa el shape-check y
  se despacha con el schema como argumentos.
- **Fix:** (a) excluir bloques dentro de un fence (el rescate es para *leaks de
  template*, que el modelo nunca fencea); (b) rechazar `parameters` si parece
  JSON-Schema (`type:object` + `properties`); (c) verificar el nombre contra
  los stubs de la ronda antes de convertir texto en llamada.

### F2 · [ALTA] El rescate XML de qwen3-coder no coerciona tipos → fallo de schema sistemático con el mejor modelo local del proyecto — ✔ verificado

- **Ubicación:** `engine.rs:1815-1822` (`parse_function_xml_tool_call`).
- **Escenario (dos direcciones):** (1) `<parameter=limit>50</parameter>` →
  `String("50")`, nunca coercido a número — toda tool con parámetro
  numérico/booleano (comunes en MCP: `limit`, `offset`, `recursive`) falla la
  validación de schema **siempre** que qwen3.5-coder emita por la vía textual
  (y es *thinking model*, propenso a ella), y el modelo no puede corregirlo
  reintentando (la gramática XML no expresa un número JSON) → quema el retry y
  rondas hasta `MAX_TURN_ITERATIONS`. (2) `<parameter=content>{"a":1}</parameter>`
  (escribir un `.json`, caso central de un coding agent) se parsea como objeto
  → rompe un schema `content: string`. Además parte los votos de best-of-n: el
  mismo call por wire (`{"limit":50}`) y por XML (`{"limit":"50"}`) tiene
  firmas distintas.
- **Fix:** coerción guiada por schema — el schema real ya está disponible en
  `dispatch_tool_calls` vía `tools.resolve()`; antes de validar, coercionar
  string↔number/boolean/integer según lo declare el schema.

### F3 · [ALTA] El guardrail post-edit deja `is_error: false`, y la escalación solo cuenta `is_error: true` → el lead nunca vuelve en el flounder de edición, el modo de falla que el harness apunta — ✔ verificado

- **Ubicación:** `tools-local/provider.rs:136-144` (`append_post_edit_feedback`,
  "The result stays `is_error: false`") × `escalation.rs:175-199`
  (`trailing_failed_observations` cuenta solo `is_error: true`).
- **Escenario:** un worker chico encadena 5 ediciones que aplican pero rompen
  la compilación; el ítem 4 devuelve los errores de `cargo check` en el mismo
  tool result, deliberadamente con `is_error: false`. Para la escalación esas
  son 5 observaciones *limpias* → el streak nunca se arma → el lead nunca
  vuelve, precisamente en el flounder característico de un 3-7B en edición. Las
  dos palancas (ítems 4 y 6) **se anulan en su intersección**.
- **Fix:** que la escalación reconozca el marcador del guardrail como falla
  (prefijo estable `[post-edit-check]` + acoplamiento por convención), o que la
  detección mire también la señal de compilación.

### F4 · [ALTA] El remap de colisiones index/id de OpenRouter está keyed solo en `id` → los proveedores sin id (la población de N-21) que reúsan `index:0` corrompen ambas calls

- **Ubicación:** `openrouter_wire.rs:442-457` (`accumulate_tool_call_fragment`,
  el guard de desplazamiento exige `(Some(existing), Some(incoming))`).
- **Escenario:** un upstream que nunca envía `id` (la población que motivó
  N-21) reúsa `index:0` para dos calls secuenciales (lo que motivó el remap del
  ítem 3): con `incoming_id = None` el guard nunca dispara, el slot se mergea
  (`{"path":"a"}{"path":"b"}` → dos raíces JSON → colapsa a `{}`) → se despacha
  **una** call vacía y la primera se pierde. El caso exacto que el remap
  pretendía eliminar, pero solo para upstreams *con* id.
- **Fix:** desplazar también cuando el fragmento anuncia `name`/`id` y el slot
  existente ya tiene `name` + un `arguments_buf` que parsea completo — el mismo
  criterio "announcement sobre call terminada" de la ruta sin index.

### F5 · [ALTA] El colapso ACI colapsa observaciones de la *misma ronda* que el modelo nunca vio completas (extiende B1/B2)

- **Ubicación:** `history.rs:129-150` (conteo global sobre la ventana, sin
  noción de rondas) + `history.rs:48`.
- **Escenario:** una ronda con 8 tool calls paralelas persiste 8
  `ToolCallCompleted`; el **siguiente** request — el primero en que el modelo
  podría leerlas — ya colapsa las 3 más viejas a 1 línea (`8-seen≥5`). El
  modelo actúa sobre resultados que nunca recibió completos. Invierte la
  premisa de SWE-agent/ACI (colapsa lo ya procesado; una observación se muestra
  completa al menos una vez).
- **Fix:** nunca colapsar observaciones de la última ronda de tool calls (el
  run consecutivo final de `ToolCallCompleted`).

### F6 · [MEDIA] El nudge de repetición miente tras una mutación intermedia: bloquea el patrón read→edit→re-read

- **Ubicación:** `engine.rs:1031-1058` (`seen_calls` per-turno, nunca
  invalidado).
- **Escenario:** flujo canónico en un turno: `read_file(x)` → `write_file(x,…)`
  → `read_file(x)` para verificar. La re-lectura es un duplicado exacto →
  nudge con `is_error=true` afirmando "the result has not changed" — **falso**,
  hubo un write entremedio. Un 3-7B concluye que su edición no se aplicó.
  Agravante: el insert en `seen_calls` ocurre *antes* de la validación de
  schema, así que la rama `attempt==2` del schema-repair es inalcanzable para
  reintentos idénticos.
- **Fix:** invalidar `seen_calls` (o las entradas de tools de lectura) tras un
  dispatch exitoso de tool mutante; reformular el nudge.

### F7 · [MEDIA] El rescate textual mutila el plan del planner: los pasos que nombran tools en sintaxis de template se extraen y se descartan

- **Ubicación:** `engine.rs:817-830` (la ronda de planning pasa por el rescate)
  + `845-861`.
- **Escenario:** `planning_system_prompt` pide "naming the concrete tools…
  and their key arguments". Un planner local responde con su template nativo
  (`1. <tool_call>{…}</tool_call>`); el rescate extrae esos bloques (que
  `attempt_planning_round` luego ignora) pero el `text_buffer` ya quedó **sin**
  ellos → el plan persistido perdió justo los pasos que nombraban tools; si
  todos eran bloques, el plan queda vacío → el A/B del planner mide un turno
  sin plan creyendo que hubo planner.
- **Fix:** `rescue_enabled: bool` en `complete_once_with`, `false` en la ronda
  de planning.

### F8 · [MEDIA] Paridad de sampling y validación de rango: `top_p/top_k/repeat_penalty` solo en Ollama; el bench los descarta en silencio para OpenRouter/Anthropic (extiende D2)

- **Ubicación:** `openrouter.rs`/`anthropic.rs` (sin setters);
  `braze-bench/src/backend_spec.rs:233-262` (builder OpenRouter ignora
  top_p/top_k/repeat_penalty, la omisión está solo en un doc comment).
- **Escenario:** replicar el sampling Qwen contra un Qwen servido vía
  OpenRouter corre en silencio con defaults del proveedor — contamina la
  comparación cross-backend igual que N-34 para temperature/seed. Sin
  validación de rango: un sweep uniforme con `--temperature 1.5` rompe
  Anthropic (máx 1.0) con 400.
- **Fix:** `with_top_p/with_top_k`(+`repetition_penalty`) en OpenRouter/
  Anthropic, cablearlos en el bench; `warn` cuando un flag se descarta.

### F9 · [MEDIA] `ollama_wire::tool_call_from_json` quedó fuera del hardening del ítem 3: dropea calls sin `name` en silencio y colapsa args stringificados sin escalera (consolida C5/C6)

- **Ubicación:** `ollama_wire.rs:405-431`. (a) `function.name` ausente → `None`
  → call dropeada sin `warn`; (b) `arguments` string truncado →
  `from_str(s).ok()` → `{}` **sin** `parse_arguments_with_repair` ni `warn`.
  Anthropic/OpenRouter habrían recuperado y logueado.
- **Fix:** rama `Value::String(s)` → `parse_arguments_with_repair`; `name`
  ausente → emitir con `name:""` para caer en la rama `NotFound` que sí
  retroalimenta.

### F10 · [MEDIA/BAJA] Asimetrías residuales de los fixes de v2 en el wire de Anthropic, y colapso de escapes unicode truncados

- **`message_stop` no drena `tool_use` pendientes** (`anthropic_wire.rs:320-330`
  vs el drain de `[DONE]` de OpenRouter, N-18): un proxy Anthropic-compatible
  que cierra sin `content_block_stop` pierde el tool call en silencio. MEDIA
  (la API real siempre envía `content_block_stop`).
- **`tool_use` sin `id` → `id:""`** (`anthropic_wire.rs:363-373`): sin el
  fallback `synth_id` que N-21 dio a los otros dos wires. BAJA.
- **args_repair colapsa un escape unicode truncado** (`args_repair.rs:59-99`):
  `{"path":"caf\u00` → cierre → `\u00"` inválido → colapsa a `{}` perdiendo
  `path` entero, habiendo información recuperable (`{"path":"caf"}`). BAJA.

> **Verificado limpio en Cluster F (no son gaps):** la escalera de reparación
> nunca corrompe args válidos (parse directo primero; daño estructural pre-corte
> cae al colapso sin inventar); el colapso ACI es render-only (no rompe pares
> tool_use/result ni ids); no hay doble parseo rescate×wire; la degradación del
> planner nunca falla el turno; `http_client.rs` compartido y `synth_id::
> process_nonce` son correctos.

---

## 7. Roadmap de remediación priorizado (SLM-first)

Ordenado por **retorno para el objetivo declarado / esfuerzo**. Los dos
primeros grupos desbloquean capacidad real de tarea con esfuerzo bajo-medio;
el tercero desbloquea el paper.

### Grupo O — La superficie de archivo usable (máxima prioridad, esfuerzo bajo-medio)
`A1, A2, A3, A4`. **A1 (paginación de `read_file`) es el prerequisito
individual de mayor impacto de toda la auditoría** — sin él ni whole-file ni
search/replace funcionan en un archivo mediano, y A2 (corrupción por
truncado) cae de gratis con él. A3/A4 (mejores errores) son pulido
inmediato. Todo en `braze-tools-local`, sin tocar contratos congelados.

### Grupo P — Calibrar el contexto a `num_ctx=8192` (alta prioridad, esfuerzo bajo)
`B5, B1, B2, B4`. Empezar por **B5** (bajar `max_tokens`, cero código, casi
duplica el presupuesto de prompt) y **B1** (cap agregado sobre las 5
observaciones full). B2/B4 (estimador coherente con el colapso; margen que
cuente tools MCP) cierran el overflow silencioso. B3 (restore
post-compactación) es mejora de capacidad, va después.

### Grupo Q — Hablarle al modelo en su plantilla (alta prioridad conceptual, esfuerzo medio)
`D1, D2, F2, C1, C2, F1`. **D1 (prompt/formato por familia) es el gap
conceptual central** — el objetivo del proyecto vive aquí. **F2 (coerción de
tipos del rescate XML) es la corrección de fiabilidad de mayor retorno
inmediato: rompe el schema sistemáticamente con qwen3.5-coder, el mejor modelo
local del proyecto** — fix acotado y guiado por el schema que ya está a mano.
**D2 (cablear sampling a producción) es trivial y ya casi está hecho** (los
`with_*` existen). C1 (schema real para MCP ya tocadas), C2 (pythonic de
Llama) y F1 (no ejecutar ejemplos fenceados / rechazar definiciones OpenAI)
extienden y endurecen el rescate. C3-C6/F9 son pulido.

### Grupo R — Economía de la escalación (media, esfuerzo medio)
`F3, D3, D6, D4/F4, D5, F6, F7`. **F3 (la escalación es ciega al guardrail
post-edit) es el de mayor prioridad del grupo: las dos palancas se anulan
justo en el flounder de edición que el harness apunta.** D3 (no escalar por
falla de entorno) y D6 (gatear best_of_n en Ollama) evitan quemar el modelo
caro / la latencia local por ruido. D4 (contadores de escalación vs best-of-n,
triple-corroborado) y F4 (remap de índice sin id) son de interacción entre
features. D5/F6 (nudge intra-turno / que miente tras mutación) y F7 (rescate
mutila el plan) cierran el runtime del loop.

### Grupo S — Hacer el bench un instrumento de paper (crítico para publicar, esfuerzo medio-alto)
`E1, E4, E6, E3, E2, E5`. **E1 (ablación componible, reusando el patrón
`+plan:` que ya funciona) + E6 (metadata de corrida) son prerequisitos de
infraestructura; E4 (matching estricto) + E3 (suite ampliada) son
prerequisitos de validez.** Sin E1 la curva harness-vs-escala no existe como
medición reproducible. E2 (ancla mini-swe-agent) y E5 (tradeoff por-skill)
elevan la contribución.

---

## 8. Fortalezas confirmadas (no tocar)

- **La plomería defensiva SLM-first es genuinamente buena y mejor que los
  pares OSS:** validez de protocolo (`protocol_check.rs`), rescate textual con
  preservación de prosa y bloques malformados visibles, `args_repair.rs`
  (walker de strings/escapes, balanceo de brackets, colapso final infalible,
  sin inventar sobre daño estructural pre-corte), ids únicos, tool alucinada →
  lista de tools válidas, repetición → nudge en vez de re-dispatch.
- **Tools locales:** schema real y detallado up-front, con descripciones
  por-campo buenas para un modelo chico (`schema.rs`).
- **La reparación de schema incluye el schema real en el primer intento**
  (no solo el error crudo) — buena base SLM, solo le falta el ejemplo (C4).
- **El system prompt está bien calibrado en longitud** para un 3B (corto,
  imperativo, sin few-shot inflado).
- **El planner es limpiamente opt-in** — la ruta default no paga su costo.
- **El split durable/táctico determinístico** sigue validado por A-MAC como
  la decisión de diseño correcta (no migrar a LLM/RL).

---

## 9. Nota metodológica

Esta auditoría se apoyó, para los hallazgos de severidad más alta, en
re-verificación manual del código (marcados ✔): A1 (`MAX_TOOL_OUTPUT_BYTES`),
C1 y C4 (flujo de schema/reparación en `engine.rs`), D1 y D2 (firma de
`default_system_prompt`, rama Ollama de `build_model_backend`), E1/E4/E6
(ausencia de flags de ablación, falso positivo `v2`, `Vec<TaskResult>`
desnudo), F2 (coerción XML en `parse_function_xml_tool_call`) y F3
(`append_post_edit_feedback` deja `is_error:false`). Dos hallazgos raíz
fueron **corroborados independientemente por múltiples agentes**: la
paginación de `read_file` (Cluster A, por los agentes de edición y contexto) y
la interacción best-of-n × contadores de escalación (D4/F documentado por
tres agentes distintos) — les da la mayor confianza de toda la pasada.

Sobre la procedencia: cinco agentes de dominio (clusters A–E) más dos pasadas
de profundidad sobre `braze-model`/`braze-engine` (cluster F). Los hallazgos
del cluster F se integraron tras spot-check manual de los dos de mayor impacto
(F2, F3), ambos confirmados exactamente como se reportaron.

**El patrón de fondo, para tenerlo presente al priorizar:** los gaps de esta
auditoría no son bugs — son *constantes y decisiones tomadas para un modelo
grande* (el "5" de SWE-agent, el `max_tokens=4096`, el prompt genérico, el
matching siempre-on) que nunca se reescalaron al régimen que el proyecto
ahora declara como su objetivo. El código está bien; está afinado para el
modelo equivocado.
