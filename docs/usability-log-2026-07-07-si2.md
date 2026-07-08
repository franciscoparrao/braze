# Registro de ejecución — SI-2 (auto-mejora supervisada)

**Fecha**: 2026-07-07
**Ejercicio**: SI-2 — Sintaxis de spec `+lead:` en `braze-bench` (`docs/self-improvement-exercises.md`)
**Backend/modelo probado**: modelo barato de OpenAI (vía OpenRouter, nombre exacto no confirmado por el usuario en esta sesión)
**Commit base de braze**: `f6ac1da` (worktree `../braze-self-improve-si1`, rama `self-improve/si-1` — sigue siendo la misma rama, SI-2 aún no tiene rama propia)
**Modo**: `braze chat --tui --supervised`
**Sesión**: `f57e3005-b65b-46ab-97eb-d30e06661b4b`

## Registro

| # | Intento | Qué esperaba | Qué pasó | Severidad |
|---|---------|--------------|----------|-----------|
| 1 | Prompt de investigación: leer `backend_spec.rs`, entender `BackendSpec::parse` y el sufijo `+plan:`. | Resumen fiel de la lógica real, como paso previo a implementar `+lead:`. | El modelo leyó el archivo (con fricción menor: dos intentos de usar una tool `search` inexistente, algunas relecturas redundantes) y dio un resumen **verificado correcto** contra el código real (`parse`, `parse_single`, `display_name`, `build_planner`, `ollama_models`, `executor_is_ollama`, `executor_model_name` — todo coincide). Pero terminó el turno con `stop_reason: "stop"` sin pedir continuar a la implementación — se autointerrumpió tratando la investigación como respuesta final, pese a que el prompt original pedía explícitamente implementar el cambio en el mismo turno. | Molesto (no bloqueante — el usuario solo necesitó mandar "Continua e implementá") |
| 2 | Mensaje de seguimiento explícito pidiendo implementar `+lead:`, `build_lead`, wiring en `runner.rs`, `display_name`, tests, `cargo test -p braze-bench`. | El modelo procede a `edit_file`/`write_file` con el cambio. | Un intento fallido de tool `search` (mismo patrón que el intento 1), luego `grep` y `read_file` exitosos (progreso real). La ronda siguiente vino vacía (sin texto, sin tool calls) — activó el mitigante de U-1 (`attempt_tools_free_summary_round`), que **también volvió vacía** — turno terminó en `EngineError::EmptyModelResponse`. `git status` confirmado limpio: no hubo escritura, nada que perder. | Bloqueante para el turno (cero progreso persistido), pero sin pérdida de datos — ver **U-10** |

## Hallazgos que ameritan seguimiento

- **U-10**: el mitigante de U-1 (`attempt_tools_free_summary_round`, `crates/braze-engine/src/engine.rs:871-887`) da un único reintento "sin tools, dame un resumen" cuando una ronda viene vacía después de que el turno ya tuvo progreso real. Con este modelo barato, ese reintento **también** volvió vacío, agotando la única red de seguridad que existe y terminando el turno en `EmptyModelResponse` sin ningún diff propuesto. No es un bug de lógica (el código hace exactamente lo que dice el comentario en `engine.rs:855-870`) sino un límite empírico: un reintento alcanza para el caso que motivó el fix (Qwen en Nitro, U-1 original) pero no necesariamente para todos los modelos. Sin fix — candidato a investigar si vale la pena más de un reintento, o si más allá de cierto punto es mejor fallar rápido y dejar que el operador decida (que es, en la práctica, lo que ya pasa). Mitigante operativo confirmado: no hay pérdida de trabajo — ninguna escritura ocurre antes de que el diff se proponga y se apruebe, así que este modo de falla es solo pérdida de un turno, recuperable con un simple reintento.

## Sesión 2 — `openrouter:moonshotai/kimi-k2.5`

**Sesión**: no capturada por id en este registro (sesión nueva, distinta de `f57e3005-...`).

| # | Intento | Qué esperaba | Qué pasó | Severidad |
|---|---------|--------------|----------|-----------|
| 1 | Prompt completo de SI-2 (investigar `+plan:` y proponer `+lead:` análogo). | Resumen fiel de `BackendSpec::parse`/`+plan:` como paso previo a implementar. | La parte de `BackendSpec::parse` fue correcta (verificado). Pero el bloque "Uso en runner.rs" fue **inventado**: afirmó que `runner.rs` usa `backend.build_executor(...)` y arma `EscalatingBackend::new(vec![planner.unwrap_or(executor.clone()), executor.clone()])`. Ninguna de las dos cosas existe — `runner.rs` no usa `EscalatingBackend` en absoluto hoy (solo `braze-cli/src/main.rs` para `--lead`), no existe `build_executor`, y `EscalatingBackend::new` toma `(lead: Box<dyn ModelBackend>, worker: Box<dyn ModelBackend>)`, no un `Vec` (`crates/braze-model/src/escalation.rs:83`). | Bloqueante — ver **U-11** |
| 2 | Corrección explícita (hechos reales de `runner.rs:215-216` y `escalation.rs:83` pegados textualmente) + instrucción de implementación. | El modelo corrige su entendimiento y procede a implementar sobre la base correcta. | Investigó de nuevo (varios `read_file`/`shell_exec` reales) y produjo una **segunda fabricación, distinta de la primera**: un `struct Runner` con campo `planner: Option<Box<dyn Planner>>` (línea 64), un método `Runner::with_planner()` (líneas 83-86), y `backend_spec.build_executor(self.backend_options.clone()).await?` (línea 173) — nada de eso existe; `runner.rs` no tiene ningún `struct`, es un módulo de funciones libres. | Bloqueante — ver **U-11** |
| 3 | "Ok, Búscalo entonces" (pedirle que verifique con `grep` dónde se usa `build_planner`). | El `grep` da resultados reales y el modelo los reporta tal cual. | El `grep` sí devolvió datos reales y correctos (`backend_spec.rs:217`, `runner.rs:215`, `main.rs:189`) — pero el modelo los envolvió en más narrativa fabricada ("esto complementa la línea 64 donde se guarda el planner explícito... el Runner soporta ambos mecanismos: 1. Planner explícito vía `Runner::with_planner()`... 2. Planner desde `BackendSpec`..."), manteniendo el `struct Runner` inventado del intento 2 pese a que el propio `grep` no lo mencionaba ni lo sustentaba. | Bloqueante — ver **U-11** |

## Hallazgos que ameritan seguimiento (sesión 2)

- **U-11**: `moonshotai/kimi-k2.5` fabricó tres arquitecturas distintas y falsas para la misma pregunta ("¿cómo usa `runner.rs` el `BackendSpec`?") en la misma sesión, pese a dos correcciones explícitas con hechos reales pegados textualmente. El patrón más preocupante no es la cantidad de fabricaciones sino que persistieron **incluso cuando sus propios tool calls devolvieron datos correctos** — en el intento 3, el `grep` que él mismo pidió correr trajo las líneas reales, y aun así las narró envueltas en un `struct Runner`/`with_planner()` que ese mismo `grep` no respalda en ningún lugar. Es una variante más resistente a corrección que U-7/U-8 (`deepseek-v4-flash`, que fabricó pero no reincidió tras una sola corrección exitosa en SI-1 — bueno, ahí tampoco se corrigió del todo, pero no llegó a un tercer intento). Sin fix de harness — recomendación operativa: no vale la pena una tercera corrección en la misma sesión; cambiar de modelo (mismo remedio que funcionó en SI-1 con `openrouter:anthropic/claude-sonnet-5`).

## Hechos verificados de `runner.rs` para la implementación real de SI-2 (para no depender de que ningún modelo los recupere bien)

```rust
// runner.rs:150 — el executor se construye así, no hay build_executor:
let model = spec.build(config, sampling)?;

// runner.rs:191 — Engine::new consume `model` directamente:
let mut engine = braze_engine::Engine::new(model, tools, /* ... */);

// runner.rs:215-217 — el ÚNICO uso de build_planner/with_planner hoy,
// no hay ningún struct Runner ni EscalatingBackend en este archivo:
if let Some(planner) = spec.build_planner(config, sampling)? {
    engine = engine.with_planner(planner);
}
```

```rust
// backend_spec.rs:217-226 — build_planner real, patrón a espejar en build_lead:
pub fn build_planner(
    &self,
    config: &Config,
    sampling: SamplingSpec,
) -> Result<Option<Box<dyn ModelBackend>>, BenchError> {
    self.planner
        .as_ref()
        .map(|planner| planner.build(config, sampling))
        .transpose()
}
```

```rust
// escalation.rs:83 — firma real, dos argumentos posicionales, no un Vec:
pub fn new(lead: Box<dyn ModelBackend>, worker: Box<dyn ModelBackend>) -> Self
```

La inserción correcta en `runner.rs` es entre las líneas 150 y 191 (después de construir `model`, antes de que `Engine::new` lo consuma):

```rust
let model = spec.build(config, sampling)?;
let model: Box<dyn ModelBackend> = match spec.build_lead(config, sampling)? {
    Some(lead) => Box::new(braze_model::EscalatingBackend::new(lead, model)),
    None => model,
};
```

## Sesión 3 — `openrouter:openai/gpt-4o-mini`

**Sesión**: `d0af390f-6e7f-42f6-bff3-ee029bbe535c`.

| # | Intento | Qué esperaba | Qué pasó | Severidad |
|---|---------|--------------|----------|-----------|
| 1 | Prompt completo de SI-2. | El modelo lee, entiende, y propone ediciones reales vía `edit_file`. | Leyó los archivos (con la fricción habitual de relecturas bloqueadas por el guard de duplicados), anunció el plan correcto en texto, y lanzó 5 `edit_file` seguidos — **los 5 fallaron** con `old_string not found`. A diferencia de U-7/U-8/U-11 (alucinar una arquitectura *plausible*), acá el `old_string`/`new_string` eran **pseudocódigo/plantilla literal**: `old_string: "fn build_planner(&self) -> Planner {"` (la firma real lleva `config`/`sampling`/`Result<Option<Box<dyn ModelBackend>>, BenchError>`, nada que ver), `old_string: "// Current initialization of EscalatingBackend"` (comentario inventado), y el más claro: `new_string: "self.display_name = format!(\"some_format\", ...);"` — un placeholder literal (`"some_format", ...`), no un intento de imitar código real. También intentó escribir en `crates/braze-bench/src/tests.rs` (no existe), pese a que el prompt pide explícitamente los tests "en el mismo archivo". Tras los 5 rechazos, el modelo no usó la salida sugerida por el propio mensaje de error (`"si no podés reproducir el texto exacto, usá write_file"`) — releyó, chocó con el guard de duplicados, y se rindió pidiéndole al usuario que le pegue el contenido de los archivos. | Bloqueante — ver **U-12** |

## Hallazgos que ameritan seguimiento (sesión 3)

- **U-12**: `gpt-4o-mini` no llegó ni al nivel de "alucinación plausible" de los modelos anteriores — sus intentos de `edit_file` usaron literalmente pseudocódigo de plantilla (`"some_format", ...`) como si fuera texto a buscar/reemplazar en el archivo real. El guard de `old_string not found` de `edit_file` (con matching tolerante a espacios en blanco, más una sugerencia de usar `write_file` como salida) funcionó exactamente como debía — rechazó los 5 intentos sin corromper nada, `git status` quedó limpio. El modelo, sin embargo, no aprovechó la salida sugerida ni intentó `write_file`; se rindió. Confirma que el guard de coincidencia exacta en `edit_file` es una defensa necesaria y suficiente contra este modo de falla — no hace falta ningún fix de harness, el modelo simplemente está por debajo del umbral de esta tarea.

## Sesión 4 — `openrouter:openai/gpt-5-mini`

**Sesión**: `c41a8c64-013b-4489-b4e2-b2f2f8a7cedb`.

| # | Intento | Qué esperaba | Qué pasó | Severidad |
|---|---------|--------------|----------|-----------|
| 1 | Prompt completo de SI-2. | El modelo lee, entiende, y propone las ediciones. | Mismo patrón de relectura excesiva que U-6 (deepseek): leyó `backend_spec.rs` completo al menos 3 veces en fragmentos superpuestos (1-400+400-712, luego 1-200+200-399, luego 1-712 entero de nuevo, luego 480-679+680-712 otra vez), intentó leer `braze-cli/src/main.rs` con una ruta incorrecta (le faltó el prefijo `crates/`, error manejado correctamente por el harness), y terminó con `grep "lead:"` sin resultados (esperable, todavía no existe ese sufijo). Nunca llegó a proponer ningún `edit_file`. El turno terminó en `error: model backend error: model backend's completion stream failed: Provider returned error` — un fallo genérico del proveedor upstream (mismo tipo que el primer fallo de `deepseek-v4-flash` en SI-1, sesión `e77a13dd`, intento 1), no un error de contenido. `git status` limpio, nada perdido. | Bloqueante (transitorio, no de contenido) — ver **U-13** |

## Hallazgos que ameritan seguimiento (sesión 4)

- **U-13**: fallo upstream genérico (`Provider returned error`) de OpenRouter sirviendo `gpt-5-mini`, sin relación aparente con el contenido de la tarea — mismo tipo que el hallazgo del intento 1 de SI-1 con `deepseek-v4-flash`. Combinado con el mismo patrón de relectura excesiva del archivo completo ya visto en U-6, aunque esta vez no llegó a agotar el tope de rondas porque el proveedor cortó antes. No es evidencia contra el modelo en sí (podría ser simplemente mala suerte de infraestructura en ese momento) — si se repite en un reintento limpio, sí valdría la pena anotarlo como patrón.

## Marcador de modelos probados para SI-2 (a la fecha)

| Modelo | Resultado |
|---|---|
| Modelo barato de OpenAI (sesión `f57e3005`, no confirmado cuál) | Entendimiento correcto, pero `EmptyModelResponse` al pedir implementar (U-10) |
| `openrouter:moonshotai/kimi-k2.5` | 3 arquitecturas fabricadas distintas pese a 2 correcciones (U-11) |
| `openrouter:openai/gpt-4o-mini` | Intentos de edición con pseudocódigo/plantilla literal, ningún diff (U-12) |
| `openrouter:openai/gpt-5-mini` | Relectura excesiva + fallo upstream genérico antes de proponer nada (U-13) |
| `openrouter:z-ai/glm-5.2` | Relectura extrema (21-36 lecturas según la sesión, U-14) + gramática de tool-call propia no cubierta por la escalera de rescate del harness (U-15) + esa misma gramática filtrándose sin rescate en la ronda de resumen sin tools (U-16) + compactación de sesión borrando todo el detalle cada ~13 lecturas (U-18) — los cuatro son hallazgos de harness, no del modelo, y todos menos la relectura de fondo ya tienen fix |
| `openrouter:anthropic/claude-sonnet-5` | (no probado todavía para SI-2 — sí resolvió SI-1 correctamente) |

## Sesión 5 — `openrouter:z-ai/glm-5.2`

**Sesión**: `2350cb2f-13bf-4a3f-8015-97d4e15dc2f3`.

| # | Intento | Qué esperaba | Qué pasó | Severidad |
|---|---------|--------------|----------|-----------|
| 1 | Prompt completo de SI-2. | El modelo lee, entiende, y propone las ediciones. | 21 llamadas a `read_file` en el turno (mismo patrón de sobre-relectura que U-6/U-9/U-13 — el archivo completo de 712 líneas releído en ≥5 combinaciones distintas de `offset`/`limit`). El turno terminó no con un error sino con un bloque de tool-call mal formado mostrado como **texto crudo sin ejecutar**: `<tool_call>read_file<arg_key>limit</arg_key><arg_value>120</arg_value><arg_key>offset</arg_key><arg_value>63</arg_value>...` — ver U-15. `git status` limpio, nada perdido. | Bloqueante — ver **U-14** (relectura) y **U-15** (gramática no cubierta, hueco real de harness) |

## Hallazgos que ameritan seguimiento (sesión 5)

- **U-14**: mismo patrón de sobre-relectura que U-6/U-9/U-13, esta vez el caso más extremo (21 `read_file` sobre un archivo de 712 líneas, releído completo al menos 5 veces). Refuerza que el umbral `TACTICAL_FULL_OBSERVATIONS=5` (ver U-6) es insuficiente independientemente del modelo probado — es un patrón transversal, no específico de un backend.
- **U-15 (hueco real de harness, no solo el modelo)**: la escalera de rescate textual de tool calls (`crates/braze-engine/src/engine.rs:490-520`, `RESCUE_LADDER`) cubre 3 gramáticas: `<tool_call>{json}</tool_call>` (Qwen2.5/Hermes), `<function=...>` XML (qwen3-coder), `func(...)` pythonic (Llama). `z-ai/glm-5.2` emite una **cuarta gramática no cubierta**: `<tool_call>nombre<arg_key>K</arg_key><arg_value>V</arg_value>...</tool_call>` — usa la etiqueta `<tool_call>` (así que la escalera lo intenta) pero el contenido no es JSON, así que la extracción falla y el bloque completo se muestra como texto plano sin ejecutarse. A diferencia de todos los hallazgos anteriores de esta sesión, **esto no es evidencia de que el modelo razone mal** — GLM hizo tool calls nativos exitosos el resto del turno (los 21 `read_file`/`grep` reales funcionaron); es específicamente esta gramática puntual la que el harness no sabe interpretar. Candidato concreto y accionable: agregar `extract_glm_arg_tag_tool_calls` (o nombre equivalente) a `RESCUE_LADDER`, siguiendo el patrón de `extract_tagged_tool_calls`/`extract_function_xml_tool_calls` ya existentes en el mismo archivo. Sin fix implementado todavía.

## Fix de causa raíz — U-6/U-9/U-13/U-14 (relectura excesiva)

Tras 5 modelos distintos tropezando con el mismo patrón, se atacó la causa raíz en vez de seguir cambiando de modelo. Diagnóstico afinado: no era (solo) que `TACTICAL_FULL_OBSERVATIONS=5` colapsara páginas viejas — era que **cuando un modelo pedía explícitamente un `limit` grande (p.ej. "traé todo el archivo de una vez"), el propio `read_file` dejaba de emitir su trailer de continuación** (`end_line == total_lines` porque el rango pedido "cabía"), y la truncación genérica por bytes de `provider.rs::wrap` (`MAX_TOOL_OUTPUT_BYTES = 8_000`) se activaba en su lugar — con un trailer ("narrow your query — a more specific path/pattern, or a smaller file") que es consejo correcto para un `grep`/`glob` grande, pero **activamente engañoso para `read_file`**: la salida correcta es seguir paginando con `offset`, no "acotar la consulta". `backend_spec.rs` (29 346 bytes) excede varias veces el cap de 8 000 bytes — cualquier intento de traerlo completo en una sola llamada topaba con este mensaje equivocado, y ningún modelo probado logró interpretar correctamente "narrow your query" como "seguí paginando con offset".

**Fix aplicado** (`crates/braze-tools-local/src/read_file.rs`, commit pendiente): nueva función `clamp_to_output_budget` que acota la página devuelta al presupuesto de bytes de `provider.rs::MAX_TOOL_OUTPUT_BYTES` (ahora `pub(crate)`) *antes* de decidir si hace falta el trailer de continuación — así, un `limit` sobredimensionado siempre termina con el trailer correcto (`"call read_file again with offset=X"`), nunca con el genérico de `wrap`. Garantiza al menos una línea siempre (una línea individual más grande que el presupuesto igual se devuelve entera, para no devolver una página vacía).

Verificado: `cargo test --workspace` (nuevos: 3 en `braze-tools-local`, total 617→620) y `cargo clippy --workspace --all-targets -- -D warnings`, ambos limpios. Binario global reinstalado (`cargo install --path crates/braze-cli --locked --force` desde el repo principal) para poder probarlo en el worktree.

**Pendiente de confirmar**: si esto realmente reduce la relectura en la práctica (el síntoma downstream —requerir muchas llamadas para ver el archivo completo— podría persistir aunque el trailer ahora sea correcto, si el modelo de todos modos no lo sigue bien). Próxima corrida con cualquiera de los modelos ya probados debería mostrar menos llamadas a `read_file` y, con suerte, offsets que avanzan monótonamente en vez de superponerse.

## Sesión 6 — `openrouter:z-ai/glm-5.2` (con el fix de relectura ya instalado)

**Sesión**: `6cdeb009-9fac-4913-a64f-87c7b3b4a228`.

Confirmado que el clamp de presupuesto de bytes funciona: las páginas ahora llegan como `[lines 1-173 of 712]` en vez de `[lines 1-200 of 712]` (`backend_spec.rs` es más denso en bytes/línea que `classifier.rs`, así que el clamp recorta de 200 a ~173 para caber bajo el cap). Pero **la relectura superpuesta siguió ocurriendo igual** — el modelo releyó `[1-173]` cuatro veces, `[174-346]` dos veces, etc. — el trailer correcto no alcanza si el modelo no lo usa para decidir su próxima llamada. El turno terminó, esta vez sin error explícito visible, con un bloque de tool-call de GLM sin parsear mostrado como texto:

```
<tool_call>read_file<arg_key>limit</arg_key><arg_value>170</arg_value><arg_key>offset</arg_key><arg_value>1</arg_value><arg_key>path</arg_key><arg_value>crates/braze-bench/src/backend_spec.rs</arg_value></tool_call>
```

## Hallazgo U-16 (bug real en el fix de U-15, no en el modelo)

Verificado con un test aislado que el parser de U-15 (`parse_glm_arg_tag_tool_call`) **sí** parsea este string exacto correctamente — el problema no estaba ahí. La causa real: este bloque no llegó por la ronda principal (`Engine::complete_once_with`, que sí tiene la escalera de rescate) sino por `Engine::attempt_tools_free_summary_round` — el mecanismo de rescate de U-1/U-10 (activado porque la ronda anterior vino completamente vacía, tras una llamada nativa exitosa a `read_file`). Esa función tiene **su propio loop de consumo de stream sin ninguna lógica de rescate**: toma lo que sea que el modelo devuelva y lo persiste tal cual como respuesta final del turno — y además transmitía cada delta en vivo al observer *antes* de poder limpiarlo.

**Fix aplicado** (`crates/braze-engine/src/engine.rs`):
- Nueva función `strip_leaked_tool_call_shapes`, que reusa la misma escalera de extracción (`extract_tagged_tool_calls`/`extract_function_xml_tool_calls`/`extract_pythonic_tool_calls`) para **detectar y descartar** bloques con forma de tool-call en el texto de la ronda de resumen — no para despacharlos (esta ronda declara `tool_stubs: Vec::new()`, no hay nada a lo que despachar), sino para que nunca se le muestren al usuario como si fueran la respuesta real. Si después de limpiar no queda texto, la ronda se trata como fallida (`Ok(false)`), igual que antes se trataba una respuesta genuinamente vacía.
- `attempt_tools_free_summary_round` ya no transmite deltas en vivo al observer durante el streaming — mismo trade-off que `complete_with_best_of_n` ya documenta y acepta ("no hay una única respuesta que mostrar token a token hasta tener la completa"). El texto limpio se entrega de una sola vez recién al final.

Verificado: 3 tests nuevos (incluye un test end-to-end con `ScriptedModel` que reproduce el escenario real: ronda vacía → fallback → bloque de tool-call filtrado, ni en el streaming en vivo ni en lo persistido). `cargo test --workspace`: 623/623 (antes 620). `cargo clippy --workspace --all-targets -- -D warnings`: limpio. Binario global reinstalado.

## Hallazgo U-17 — la causa de fondo de la relectura (hueco real de harness)

Con el fix de U-6 (trailer correcto) instalado, GLM seguía releyendo `[1-173]` cuatro veces, `[174-346]` dos veces, etc. — el trailer ahora es correcto pero eso no alcanza si el modelo pierde de vista lo que ya leyó. La tarea de SI-2 requiere leer 3 archivos (`backend_spec.rs` 712 líneas, `runner.rs` 326, `main.rs` 779 — ~1800 líneas en total), lo que necesita ~10-12 llamadas a `read_file` para cubrir todo una vez.

Revisando `tactical_full_observation_indices` (`crates/braze-engine/src/history.rs`) encontré la causa real: además de `TACTICAL_FULL_OBSERVATIONS=5` (cuántas observaciones son *candidatas* a quedar completas), hay un segundo cap independiente, `MAX_FULL_OBSERVATIONS_TOTAL_CHARS=8_000` (agregado, en bytes, sobre esas 5). El walk es newest-first: la más nueva siempre entra completa sin importar su tamaño; la siguiente entra solo si el acumulado sigue cabiendo en 8 000. Como cada página de `read_file` ahora llega cerca del cap de 8 000 bytes por tool-output (`MAX_TOOL_OUTPUT_BYTES`, ver el fix de U-6 arriba, que hace exactamente esto en la práctica), **una sola observación grande ya consume casi todo el presupuesto agregado** — la segunda más reciente casi nunca entra. En la práctica, "las últimas 5 quedan completas" se degradaba a "la última 1 queda completa" para cualquier tarea que lea archivos reales de tamaño típico.

Ese cap de 8 000 caracteres estaba pensado específicamente para proteger el `num_ctx` de 8192 tokens de un modelo local chico corriendo en Ollama (el comentario original lo dice explícitamente) — pero se aplicaba **sin condición a todos los backends**, incluidos los de nube (OpenRouter, Anthropic) con ventanas de contexto uno o dos órdenes de magnitud más grandes, donde no hay ninguna razón real para mantenerlo tan ajustado.

**Fix aplicado** (`crates/braze-engine/src/history.rs` + `engine.rs`):
- `MAX_FULL_OBSERVATIONS_TOTAL_CHARS` pasa de constante fija usada directamente a *default* — `tactical_full_observation_indices`/`render_tactical_events`/`build_messages_with_full_observations` ahora reciben el presupuesto como parámetro explícito en vez de leer la constante.
- Nueva función `Engine::full_observations_byte_budget(context_budget_tokens: Option<u32>)`: si hay un `context_budget_tokens` configurado (Ollama, hoy), mantiene el cap original de 8 000 sin cambios — no se tocó el comportamiento que ya estaba validado para modelos locales chicos. Si no hay ninguno configurado (cualquier backend de nube), usa 10× ese valor (80 000 caracteres) — generoso pero acotado, no elimina la protección, solo la escala a un contexto donde realmente hace falta.
- `Engine::load_messages` calcula este presupuesto una vez y lo pasa tanto a la construcción de mensajes como al estimador de tokens que decide cuándo compactar, para que ambos midan lo mismo.

Verificado: 3 tests nuevos (el helper `full_observations_byte_budget` para `Some`/`None`, y un test que confirma que con presupuesto ampliado las 5 observaciones grandes quedan completas en vez de colapsar a 1). `cargo test --workspace`: 626/626. `cargo clippy --workspace --all-targets -- -D warnings`: limpio. Binario global reinstalado.

**Pendiente de confirmar en vivo**: si esto realmente frena la relectura con GLM u otro modelo — es la hipótesis mejor fundamentada hasta ahora (explica cuantitativamente por qué "las últimas 5" se comportaba como "la última 1"), pero sigue siendo una hipótesis hasta verse en una sesión real.

## Hallazgo U-18 — el fix de U-17 era la mitad de la historia

Reintenté con `z-ai/glm-5.2` usando el prompt de implementación, ya con el fix de U-17 instalado. Resultado: **igual o peor que antes** — 36 llamadas a `read_file` en un solo turno, releyendo `backend_spec.rs` completo 3 veces enteras. El propio modelo lo notó y lo dijo explícitamente en su texto: *"The read_file results keep getting cleared from context. Let me read the key sections in small targeted chunks to get the actual code into context."*

Revisé el log de sesión (`fa336b93-...`, 199 eventos): 3 `compaction_occurred` en el turno, aproximadamente cada 13 llamadas a `read_file` — coincide con el umbral por defecto de `tactical_compaction_threshold` (40 eventos, ~3 eventos por ronda de `read_file`). Y el contenido de esas compactaciones confirma exactamente lo que el modelo sospechaba:

```
Previous context (compacted):
- Tools used: read_file(crates/braze-bench/src/backend_spec.rs), read_file(crates/braze-bench/src/backend_spec.rs), read_file(...), ...
```

Sin rutas de línea, sin contenido, sin nada aprovechable — solo la lista de qué tools se llamaron. U-17 (el cap agregado de bytes) protege el colapso *dentro* de una ventana no compactada, pero es un mecanismo completamente distinto y más superficial que la **compactación de sesión** (`compaction_occurred`), que cuando dispara, borra *todo* el detalle sin excepción. `DEFAULT_TACTICAL_COMPACTION_THRESHOLD = 40` (conteo de eventos, no bytes/tokens) es el mismo tipo de constante plana que U-17 corrigió — tuneada para un modelo local chico, aplicada sin condición a cualquier backend.

**Fix aplicado** (`crates/braze-engine/src/engine.rs`): nueva función `effective_tactical_compaction_threshold(configured, context_budget_tokens)`, mismo patrón exacto que `full_observations_byte_budget` (y ahora comparten la constante `NO_CONTEXT_BUDGET_SCALE_MULTIPLIER = 10`) — sin `context_budget_tokens` configurado, el umbral efectivo es 10× el configurado (400 en vez de 40 por defecto). `Engine::load_messages` usa este valor tanto para decidir si compacta como en el log de diagnóstico.

Verificado: 2 tests nuevos para el helper, 1 test existente ajustado (sembraba eventos basados en el umbral crudo, ahora usa el efectivo). `cargo test --workspace`: 630/630. `cargo clippy --workspace --all-targets -- -D warnings`: limpio. Binario reinstalado.

## Hallazgo U-19 — el fix de U-18 tenía un bug propio, y encontré la tercera capa

Reintenté con `z-ai/glm-5.2` con U-17+U-18 instalados. Resultado: **0 compactaciones esta vez** (confirmado en el log de sesión, `0879eb56-...`) — U-18 funcionó, ningún borrado de sesión completa ocurrió. Pero el modelo **igual** hizo 20 llamadas a `read_file`, releyendo `backend_spec.rs` completo 3 veces y `runner.rs` 2 veces, hasta agotar el tope de 20 idas-y-vueltas.

Esto aisló la causa: `tactical_full_observation_indices` (`crates/braze-engine/src/history.rs`) tiene una **tercera capa de cap, independiente de las otras dos**: solo las últimas `TACTICAL_FULL_OBSERVATIONS` (5 por defecto) observaciones son siquiera *candidatas* a quedar completas — sin importar cuán generoso sea el presupuesto de bytes (U-17) ni si hubo compactación (U-18). Con 20 llamadas a `read_file` en una sola ventana táctica sin compactar, para cuando el modelo hace la lectura #6, la #1 ya colapsó a su primera línea + marcador — exactamente el mecanismo original de U-6, que mis dos fixes anteriores nunca tocaron.

**Antes de aplicar el fix, corregí un bug real en mi propio fix de U-18**: `effective_tactical_compaction_threshold` escalaba *cualquier* valor configurado ×10 sin importar si era el default o un override explícito — lo cual habría corrompido `+ablate:tactical-threshold=N` de `braze-bench` (`AblationOverrides`, un knob que existe específicamente para medir el efecto de *ese* valor exacto). Agregué un guard: solo escala cuando el valor configurado es exactamente el default sin tocar (`DEFAULT_TACTICAL_COMPACTION_THRESHOLD`); un override explícito sobrevive intacto.

**Fix aplicado** (`crates/braze-engine/src/engine.rs`): nueva función `effective_tactical_full_observations`, mismo patrón que las otras dos (y con el mismo guard anti-corrupción de ablación desde el arranque, para no repetir el error de U-18). Sin `context_budget_tokens` configurado y sin override explícito de `+ablate:full-observations=N`, el valor efectivo es 10× el default (50 en vez de 5).

Con esto, **las tres capas de memoria que existían en el harness ya escalan según si hay un presupuesto de contexto chico configurado**: el cap de bytes por observación (U-17), el umbral de compactación de sesión (U-18), y el conteo de observaciones candidatas a quedar completas (U-19).

Verificado: 6 tests nuevos (incluye las regresiones específicas para el guard anti-ablación en ambas funciones). `cargo test --workspace`: 634/634. `cargo clippy --workspace --all-targets -- -D warnings`: limpio. Binario reinstalado.

## Sesión 7 — `ollama:gemma4:e4b` en Nitro (RTX 3050 6GB, `--ollama-url`)

Primer intento con un modelo local en vez de vía OpenRouter, y primera vez que un modelo **sí escribe** en vez de solo leer/re-leer. Buena noticia menor: solo 4 `read_file` + 1 `grep` para entender `+plan:` — el patrón de relectura no apareció esta vez (dataset de 1, no hay que sacar conclusiones sobre el porqué). Mala noticia grande: el resultado es el peor de toda la ronda de model-shopping hasta ahora.

- Usó `write_file` (reemplazo completo del archivo) en vez de `edit_file` para lo que debía ser una adición focalizada — sobre un archivo de 712 líneas con 15+ tests existentes y varios métodos productivos.
- El contenido escrito inventa una arquitectura que no existe en el codebase (`Executor::parse`, `Planner::new`, `Lead::new`, `BackendSpec::parse_internal`) — ni siquiera `BackendSpec` queda en scope dentro de su propio `impl`.
- Resultado: 640 líneas borradas, 159 insertadas, **20 errores de compilación** (`cargo build -p braze-bench`), tests de `+plan:` completos eliminados, métodos productivos (`build`, `build_planner`, `ollama_models`, `executor_is_ollama`, `executor_model_name`, `ablation()`) desaparecidos.
- El modelo notó que algo se rompió, intentó un `edit_file` de parche, y en vez de correr `cargo test` como se le pidió, cortó el turno narrando en tono de tercera persona ("si estás continuando esta tarea, te recomiendo...") — como si estuviera dejando notas para otro, no terminando su propio trabajo.

Restaurado con `git checkout -- crates/braze-bench/src/backend_spec.rs` (nada commiteado, cero pérdida real). `cargo build -p braze-bench` limpio tras la restauración.

## Hallazgo U-20 — un modelo de 8B puede ser peor que "no converge"

A diferencia de todos los hallazgos anteriores (relectura infinita, alucinación de resúmenes, gramática de tool-call no reconocida), este es el primer caso donde el modelo **sí produjo una escritura real e irreversible** basada en una comprensión fundamentalmente equivocada de la tarea — y lo hizo con `write_file` (reemplazo ciego) en vez de `edit_file` (que al menos requiere que el `old_string` matchee texto real, actuando como un freno). Es el modo de falla más peligroso de toda la sesión: no es "no converge" (inofensivo, se reintenta), es "converge hacia algo activamente destructivo".

Mitigante que funcionó exactamente como debía: `--supervised` no impidió la escritura (el usuario la aprobó, razonablemente, sin poder anticipar que rompería todo), pero como nada se commitea automáticamente, el costo real de la falla fue cero — un `git checkout --` y viaje en el tiempo completo. Sin ese modo (o sin revisar el diff antes de commitear), esto se habría colado como un "arreglo" que en realidad es una regresión catastrófica.

No hay fix de harness que se derive de esto — 8B en Q4_K_M simplemente está por debajo del umbral de esta tarea, más aún que los modelos vía API probados antes. Sin fix implementado; queda como advertencia de que "diff que compila y pasa los tests propios que el mismo modelo escribió" **no es** un sustituto de correr `cargo build`/`cargo test` de forma independiente antes de aprobar — cosa que en este caso ni siquiera llegamos a evaluar porque el modelo nunca corrió `cargo test` como se le pidió.

## Estado de SI-2

No resuelto todavía — ningún diff propuesto (ni utilizable) en 9 sesiones contra 6 modelos "baratos" distintos. Se corrigieron 6 hallazgos reales de harness en el camino:
- U-15/U-16: gramática de tool-call de GLM no rescatada, ni en la ronda principal ni en el fallback sin tools.
- U-6 (parte 1): `read_file` emitía el trailer equivocado cuando un `limit` grande topaba con el cap de bytes de `wrap`.
- U-17: el cap agregado de "observaciones completas" (8 000 caracteres) degradaba "las últimas 5 quedan completas" a "la última 1 queda completa" para cualquier archivo real.
- U-18: la compactación de sesión (evento cada ~40, sin condición de backend) borraba *todo* el detalle cada ~13 llamadas a `read_file`.
- U-19: el conteo de observaciones candidatas a quedar completas (5 por defecto) era un tercer cap independiente que ni U-17 ni U-18 tocaban.

Las tres capas de memoria (U-17/U-18/U-19) están corregidas y escalan según si hay un `context_budget_tokens` configurado — pero la sesión con `gemma4:e4b` (U-20) demostró que resolver la memoria no resuelve el problema de fondo: un modelo de 8B alucinó una arquitectura completa y la escribió con `write_file` (reemplazo ciego, no `edit_file`), rompiendo 640 líneas de código real y borrando todos los tests existentes. Ningún ajuste de presupuesto de contexto arregla eso — es un límite de capacidad del modelo, no de memoria disponible. Todavía no probamos un modelo genuinamente capaz (Sonnet 5) *después* de las tres correcciones de memoria — sigue pendiente para saber si SI-2 es alcanzable con la infraestructura actual del harness.
