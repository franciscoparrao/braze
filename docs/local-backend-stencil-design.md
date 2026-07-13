# Diseño: `LocalBackend` in-process + `stencil` (constrained decoding nativo)

Fecha: 2026-07-12/13
Estado: **CERRADO (2026-07-13) — `stencil` DESCARTADO;
`LocalBackend`-por-CAPACIDAD prerrequisito RESUELTO y el documento
ARCHIVADO como histórico.** `gpt-oss:20b` supera a `qwen3.5-coder` en
pass rate Y latencia sirviendo desde la infraestructura actual de
Nitro, sin offloading ni RAM nueva — la ganancia de capacidad que este
documento perseguía ya está capturada sin construir nada de lo que
propone. Ningún `impl ModelBackend` nuevo se justifica con esta
evidencia. Ver § "LocalBackend-por-capacidad" abajo para el detalle y
`docs/sweep-capacity-hardware-2026-07-13.md` para los datos.

## `stencil` (constrained decoding in-process) — DESCARTADO

El A/B que gateaba esta mitad (`docs/constrained-decoding-ab-design.md`)
cerró RECHAZADO, con su única iteración corrida y también negativa: el
sweep original (1.045 corridas) disparó la cláusula de iteración, y la
iteración (`oneOf` por tool con el schema real — la versión MÁS
estricta de constraint que `stencil` habría implementado) verificó el
mecanismo limpio (`schema_validation_failures` 99→0 en llama3.2:1b)
pero el pass rate **empeoró** en las tres filas medidas (−13.7pp a
−41.1pp, todos los ICs fuera de cero). Detalle completo en
`docs/sweep-constrained-decoding-2026-07-12.md`. No hay señal en estos
datos de que subir el rigor del constraint compre nada — al contrario,
lo empeora. **No se persigue esta mitad.** El hallazgo entra al paper
(§ ablations) como evidencia a favor de la tesis del harness: la
escalera de rescate es el tradeoff correcto; controlar el decoder no
ayuda a los modelos débiles y además cuesta.

## `LocalBackend`-por-capacidad (§ Hardware) — PRERREQUISITO RESUELTO: `gpt-oss:20b` gana, sin RAM nueva

Esto mata solo la mitad de "constrained decoding". La mitad de
CAPACIDAD (correr en Nitro un modelo más capaz, independiente de si
restringir el decoder ayuda) no depende del veredicto de arriba.

**Prerrequisito corrido 2026-07-13** (`docs/sweep-capacity-hardware-2026-07-13.md`,
285 corridas, `gpt-oss:20b`/`gemma4:12b`/`qwen3.5-coder` como ancla en
el mismo sweep): `gpt-oss:20b` (recién bajado — no estaba instalado)
**limpia la barra en las dos dimensiones**, sin tocar RAM ni offloading:

| Modelo | Pass rate | Latencia | vs `qwen3.5-coder` (mismo sweep) |
|---|---|---|---|
| **`gpt-oss:20b`** | **98.9%** (94/95) | **13.0s** | **+6.3pp** [+0.2,+14.0] Newcombe95%, **1.9× más rápido** |
| `gemma4:12b` | 82.1% | 29.8s | −10.5pp, más lento — descalificado, igual que Qwen2.5-7B |
| `qwen2.5:7b` | 80% [71,87] (sweep previo) | 5.1s (sweep previo) | −18pp — descalificado sin re-testear |

Mecanismo limpio: `schema_fail=0`, `rescues=0`, una sola falla en 95
corridas (`grep_basic`, `assertion_tool_call`) — la ganancia no es un
artefacto de medición.

**Implicación — cambia la recomendación del documento original**:
`gpt-oss:20b` corre hoy vía `OllamaBackend` normal, sirviendo desde los
16GB actuales de Nitro sin ningún truco de offloading. Eso significa
que **la ganancia ya está capturada sin `LocalBackend` in-process ni
compra de RAM** — el camino más barato (cambiar qué modelo sirve Nitro
por default) ya resuelve lo que el eje de capacidad buscaba. La
pregunta "¿vale la pena el 30B-A3B offloaded a 64GB?" pasa de
prerrequisito bloqueante a curiosidad de segundo orden, sin
justificación de negocio clara todavía — nadie va a construir
`LocalBackend` (crate nuevo, mistral.rs/candle, GGUF in-process) para
perseguir una mejora marginal sobre un modelo que YA gana con la
infraestructura existente. **`LocalBackend` queda sin justificación
activa; el documento entero se archiva como histórico.**

**Acción recomendada, fuera del scope de este documento**: promover
`gpt-oss:20b` a modelo local recomendado en `CLAUDE.md` § "Modelos
locales recomendados", reemplazando a `qwen3.5-coder` como mejor local
del proyecto — con la salvedad de que este sweep es n=95 de un solo A/B
de capacidad, no la batería completa de skills (`g10-weak-skills`) que
calibró a `qwen3.5-coder` originalmente; correr esa batería antes de
promoverlo formalmente.

---

Hand-off desde una sesión de análisis (a raíz de `JustVugg/colibri`).

_Actualizado 2026-07-12: se agregó § Hardware (Nitro, offloading vs streaming,
modelo candidato por RAM) y se corrigió § alcance — la primera versión descartó
de más al meter offloading y streaming en la misma bolsa._

> **Nota de archivo (2026-07-13, actualizada)**: todo lo que sigue de
> acá para abajo es la PROPUESTA ORIGINAL, escrita antes de que ambos
> ejes cerraran. **El documento completo está archivado como
> histórico** — ni `stencil` (rechazado por el A/B) ni `LocalBackend`
> in-process (sin justificación: `gpt-oss:20b` ya gana sin construirlo,
> ver § arriba) tienen trabajo pendiente. No ejecutar nada de lo que
> sigue — "Qué construir", "La única decisión abierta", "Alcance V1" y
> el "Prompt para retomar" del final quedan todos obsoletos. Se
> conserva solo como registro de cómo se llegó a la decisión.

## Origen y relación con el A/B en curso

El A/B de constrained decoding ya distingue dos capas del espectro que hizo
explícito la revisión de colibri (grammar-forced drafts):

- **Capa de harness** (lo que Braze aporta hoy): escalera de rescate textual.
- **Capa de inferencia** (constrained decoding): hoy *delegada* al campo
  `format` de Ollama (structured outputs) en `OllamaBackend`.

Este documento es el paso natural siguiente en la capa de inferencia: **traer
el constrained decoding adentro del proceso Rust**, a nivel de sampler, en vez
de delegarlo a un servidor Ollama externo. No duplica el A/B — lo extiende: el
A/B mide *si* el constraint ayuda a los modelos débiles; esto propone *dónde*
vivirá el constraint si la respuesta es sí.

## La observación que corrige el alcance (importante)

colibri tiene dos novedades: (1) **streaming de expertos MoE desde NVMe** para
correr modelos de 700B en 25 GB de RAM, y (2) **grammar-forced drafts**
(constrained decoding). Hay que separar (1) en dos regímenes que NO son lo mismo
— en una primera pasada los metí en la misma bolsa y descarté de más:

- **(2) grammar-forced drafts → SÍ, el núcleo.** Ataca tu problema real (sintaxis
  rota del tool-calling). De aquí sale `stencil`.
- **(1a) offloading de expertos (RAM↔VRAM) → SÍ, el objetivo real del
  `LocalBackend`.** Un MoE mediano (p.ej. Qwen3-30B-A3B: 30B totales, ~3B
  activos) corre con compute de 3B y calidad ~30B, con los expertos inactivos en
  RAM. Rápido y usable en un loop agéntico. Corres modelos chicos por **límite de
  hardware, no por gusto** — el offloading sube de clase de modelo sin cambiar de
  máquina. Ver § Hardware.
- **(1b) streaming de expertos desde NVMe (lo de colibri) → NO, para interactivo.**
  Modelos de 100B–700B con pesos en disco, a **~0.05–0.4 tok/s**: un envelope de
  50 tokens tarda minutos. Un loop agéntico (muchas idas y vueltas) es el peor
  caso. Solo batch/overnight — no para Braze interactivo.

Conclusión corregida: el `LocalBackend` **no es "reducir a modelos chicos"** — es
correr en Nitro un modelo *más capaz* (vía offloading) con constrained decoding
nativo, atacando la capacidad en la raíz. Se descarta solo el streaming NVMe.

## Hardware: Nitro y el modelo candidato

Specs de Nitro (confirmadas): **RTX 3050 6 GB VRAM + 16 GB RAM** (nodo LAN de
bench; ver CLAUDE.md § Benchmarking y `docs/usability-log-2026-07-07-si2.md`;
contención de RAM ya documentada en la auditoría v5, H-15). El techo del modelo
lo fija la **RAM del sistema**, no la VRAM.

Punto de partida honesto: Nitro **ya corre bien `qwen3.5-coder`** (6/6 en
`g10-weak-skills`, el mejor local del proyecto — CLAUDE.md § Modelos locales
recomendados), aunque es *thinking model* a ~20-27 s/tarea. Así que la pregunta
no es "salir de los 1–3B" sino: **¿un MoE offloaded supera a `qwen3.5-coder` en
pass rate Y latencia?** Se decide en `braze-bench`, no a priori.

Escalera de modelos según la RAM de Nitro:

| RAM | Candidato | Nota |
|-----|-----------|------|
| **16 GB (as-is)** | Denso 7–8B function-calling (Qwen2.5-7B) **o** gpt-oss-20B (MoE ~3.6B activos, cabe en 16 GB); comparar contra `qwen3.5-coder` ya instalado | Sin gastar. Priorizar modelos fine-tuneados para tool-calling (técnica G6, CLAUDE.md). |
| **64 GB (upgrade ~$60–120)** | Qwen3-30B-A3B (Q4 ~18 GB, ~3B activos) offloaded | El salto grande, un solo nodo, velocidad usable (offloading, no streaming). **Mejor ROI.** |

Vías de hardware evaluadas:
- **Comprar RAM (16→64 GB): recomendado.** Barato, un solo nodo, desbloquea el
  30B-A3B por offloading.
- **Combinar PCs del cluster: NO para esto.** El pooling entre máquinas existe
  (`llama.cpp` RPC, `exo`), pero cada token sincroniza activaciones sobre LAN de
  consumo (~1 Gbps) → la latencia domina y mata el loop agéntico. Reservar para
  modelos que no caben en ningún nodo, en batch.

**Velocidad a MEDIR (prerrequisito):** el offloading en una 3050 (swap de
expertos por PCIe por token) es mucho más rápido que el streaming NVMe, pero
puede seguir siendo lento — `braze-bench` lo mide directo, ANTES de comprometer
la compra de RAM.

## El seam de integración (confirmado por auditoría)

Punto de enganche: **`trait ModelBackend`** en
`crates/braze-model/src/backend.rs`. Es dyn-dispatch (`Box<dyn ModelBackend>`
en `braze-engine/src/engine.rs`), con tres implementadores independientes
(`AnthropicBackend`, `OllamaBackend`, `OpenRouterBackend`) → **enchufa un
cuarto sin tocar el engine**. Plantilla de referencia: `ollama.rs` +
`ollama_wire.rs` (ya trae los flags `with_prompt_tools`/`with_constrained_tools`).

Contrato que el backend debe cumplir:
1. Devolver `Stream<CompletionEvent>` con `TextDelta` / `ToolCallRequested{id,name,arguments}` / `Usage` / `Done`. **Invariante: terminar en `Done` o `Err`.**
2. Traducir `Vec<Message>` (bloques `Text`/`ToolUse`/`ToolResult` estilo Anthropic, `crates/braze-types/src/message.rs`) a su plantilla de chat.
3. Consumir `ToolStub` (nombre+summary) y resolver schema on-demand vía `ToolProvider::resolve_schema` (`crates/braze-tools-core/src/provider.rs`).
4. Reportar `Usage` (para local el precio es $0; `default_model_pricing()` en `braze-config/src/config.rs` ya tiene catch-all).

Registro: agregar el nombre a `Config::default_backend` (`braze-config/src/config.rs`) y al composition root de `braze-cli`. Permisos (`braze-permissions`) son agnósticos al backend — sin cambios.

Estado hoy: **la "inferencia local" de Braze es Ollama como servidor externo**
(nodo Nitro por HTTP). Nadie carga pesos dentro del proceso. Ese es el hueco.

## Qué construir

### 1. `LocalBackend` — cuarto `impl ModelBackend`, inferencia in-process
Carga el modelo GGUF dentro del proceso Rust (sin servidor Ollama) y produce el
`Stream<CompletionEvent>`. Beneficio inmediato: **mata la dependencia del
servidor Ollama externo** y da control del decoder (logits), que los backends
HTTP no exponen. **Beneficio de fondo (ver § Hardware):** con offloading corre en
Nitro una clase de modelo que Ollama ahí no da cómodo (7–20B, o 30B-A3B con RAM
nueva) — el salto de capacidad, no solo quitar una dependencia.

### 2. `stencil` — el sampler restringido (la pieza novedosa, tuya)
Crate enfocado: **schema del envelope de tool-call → máscara de logits** token
a token, de modo que la sintaxis del envelope sea imposible de romper *antes* de
emitirse. Integra con la escalera de rescate (objetivo: `schema_fail + rescues
≈ 0`, la métrica de verificación del A/B). Nombre provisional.

Por qué `stencil` es lo correcto para construir tú mismo:
- Es **substrato-independiente**: la lógica gramática→máscara se testea con un
  vocabulario mock, sin cargar un modelo. Es un core enfocado, estilo del
  ecosistema.
- Es **la IP real**: afinado a *tu* envelope y *tu* rescate, no un servidor LLM
  genérico.
- Es **publicable**: tu A/B pre-registrado es el paper; `stencil` es el
  mecanismo que mide en la capa de inferencia.

## La única decisión abierta: el sustrato de inferencia

No reinventar el transformer (commodity; ahí competirías con llama.cpp/candle).
Opciones, de menos a más trabajo:

1. **Envolver `mistral.rs` como librería (RECOMENDADO para arrancar).** Rust,
   in-process, soporta Gemma/Qwen/Llama, GGUF cuantizado, **device offloading de
   MoE** (lo que habilita el 30B-A3B de § Hardware) **y ya trae
   constrained/grammar decoding**. `LocalBackend` sería sobre todo el glue al
   trait. Valor inmediato con poco código.
2. **Sobre `candle`.** Más bajo nivel, control total del sampler; encaja con que
   ya usas inferencia in-process (`geoembed-rs` con tract). Más trabajo.
3. **Desde cero.** No — es el colibri maximalista, sin aporte sobre 1/2.

**Recomendación:** empezar por (1) para tener `LocalBackend` funcionando ya, y
construir tú solo `stencil` afinado al envelope. Caveat honesto: como
`mistral.rs` ya trae constrained decoding, la cuña novedosa **no es "un motor de
inferencia"** (eso se compra) sino **el sampler afinado a lo agéntico + el
resultado medido**. Es más chico de lo que suena — y por eso encaja.

## Alcance V1 (a congelar al arrancar)

Dentro: `LocalBackend` (impl `ModelBackend` sobre mistral.rs/candle, streaming
token→`TextDelta`, `ToolCallRequested`, `Usage` $0, invariante `Done`) +
`stencil` (schema del envelope → máscara de logits) + modo `constrained` a nivel
sampler. Fuera de V1: streaming de expertos MoE, multi-GPU, servir API,
speculative decoding.

## Validación

Ya existe: **`braze-bench`**. Criterio = el del A/B (`constrained-decoding-ab-design.md`):
en executors débiles, ¿el brazo constrained-nativo mejora sobre baseline y sobre
prompt-tools, con `rescues + schema_fail ≈ 0`? Gold standard = tu propio banco.

**Condicionamiento por el A/B en curso:** si el sweep concluye que el constraint
ayuda a los débiles → `stencil` in-process vale (y desbloquea modelos que la API
nativa rechaza, como gemma3:1b). Si concluye que domina el *format tax* (los
fallos son semánticos, no sintácticos) → reconsiderar: quizás el harness ya es
el tradeoff correcto y `stencil` no cambia la ecuación. **No arrancar esto
antes de leer ese resultado.**

## Riesgos / caveats honestos

- `mistral.rs` ya hace constrained decoding → riesgo de que `LocalBackend` sea
  "un backend más" sin novedad; la novedad debe ser el afinado agéntico + la
  medición, no el motor.
- Fast-moving target: cada arquitectura de modelo nueva puede exigir soporte
  nuevo en el sustrato (mitigado al apoyarse en mistral.rs/candle, que lo
  mantienen ellos).
- El format tax (documentado) puede hacer el resultado negativo — lo cual es un
  hallazgo válido, no un fracaso.

## Cierre (2026-07-13)

**No queda nada que retomar de este documento.** El prerrequisito de
hardware se corrió (`docs/sweep-capacity-hardware-2026-07-13.md`):
`gpt-oss:20b` supera a `qwen3.5-coder` en pass rate Y latencia sirviendo
desde la infraestructura actual, sin offloading ni RAM nueva. Eso
resuelve la pregunta que motivaba todo el eje de capacidad sin construir
nada de lo que este documento proponía — ni `LocalBackend` in-process
ni la compra de RAM tienen justificación activa. La única acción
derivada (fuera del scope de este documento) es evaluar promover
`gpt-oss:20b` a modelo local recomendado en `CLAUDE.md`, tras correr la
batería `g10-weak-skills` que calibró a `qwen3.5-coder` originalmente
(ver limitaciones de `docs/sweep-capacity-hardware-2026-07-13.md`).

Si en el futuro cambia el panorama (Nitro cambia de hardware, aparece un
modelo local sensiblemente mejor que ninguno de los servidos por Ollama,
o el proyecto necesita control del decoder por una razón distinta a
tool-calling), este documento sirve como registro de qué ya se
descartó y por qué — no como punto de partida para retomar directo.
