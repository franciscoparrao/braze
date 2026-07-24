# Diseño: modo **doc-QA** — RAG léxico offline sobre el `LocalBackend`

> **Estado (2026-07-23):** **Pasos 1-2 del MVP EJECUTADOS** — el resto,
> propuesta. (1) La crate `braze-docs` (`crates/braze-docs`):
> `chunk_wiki`/`chunk_markdown` (chunker markdown por headings + fallback
> a párrafos) y `LexicalIndex` detrás del trait `Retriever` (port del
> scoring de `search_stubs`). (2) El subcomando **`braze docs`**
> (`--dir <wiki> <pregunta> [--backend/--model/--ollama-url/--top-k/
> --max-tokens]`): pipeline retrieve-then-answer despachado como
> `run_docs` en `main.rs` (sin engine/sesión/tools), con el system
> prompt mínimo de grounding y la lista de fuentes citadas. 26 tests
> verdes (16 cli_args + 10 docs), clippy `-D warnings` limpio.
> **Verificado en vivo** (offline, ollama qwen2.5:3b): la MISMA pregunta
> de las capturas de Claudio ("pasos UJR aprobar/rechazar heredero")
> sobre una mini-wiki SPE dio una respuesta sintetizada limpia de 5 pasos
> + 5 fuentes citadas — el contraste directo con su "modo degradado".
> **Offline total confirmado** (2026-07-23): la misma corrida con
> `--backend local` (llama.cpp in-process, sin Ollama ni ningún
> servicio) dio la misma respuesta limpia + fuentes — valida el
> requisito literal de Claudio ("ninguna llave de servicio"). **UI
> EJECUTADA** (2026-07-23): `braze docs --serve [--port N]` — server HTTP
> mínimo hecho a mano (sin framework web, sobre `tokio::net`, `+net` en
> tokio) que carga índice+modelo UNA vez (quedan calientes) y sirve una
> página de chat autocontenida. `GET /` → la página; `POST /ask
> {question}` → `{answer, sources}`; acceso al modelo serializado con un
> Mutex. Verificado en vivo offline (`--backend local`, qwen2.5:3b): GET
> / sirve el HTML, POST /ask devuelve la respuesta de 5 pasos + 5
> fuentes. El MVP de la línea está completo; el resto es propuesta. Nace
> de un
> caso de uso externo (Claudio Álvarez, conversación 2026-07-23): un
> chatbot **liviano, 100% offline (sin ninguna llave de servicio),
> sobre documentación**, para usuarios de soporte con hardware modesto y
> preguntas "de uso típico". Es un vecino cercano de braze pero **no es
> el loop agéntico**: es RAG (retrieval-augmented generation) sobre una
> wiki con un chat mínimo encima. Este documento bosqueja cómo braze da
> ese producto reusando el `LocalBackend`, el retriever léxico que **ya
> existe** (`tool_search::search_stubs`) y la doctrina contexto-chico,
> con muy poco código nuevo. Decisión de arquitectura central:
> **pipeline RAG (retrieve-then-answer, sin tool call) como default**,
> RAG agéntico como escalón. **Anclado a evidencia** (§ Evidencia
> empírica): dos capturas del propio Claudio muestran que su app RAG con
> embeddings + retrieval híbrido rinde PEOR que Ollama pelado por
> **síntesis sobrecargada** ("modo degradado", 90s) — confirma la
> doctrina contexto-chico y fija el enemigo del diseño. Relacionado:
> `docs/local-backend-design-2026-07-20.md` (el motor),
> `docs/harness-engineering-hooks-skills-2026-07-10.md` (§ I.3, el
> mecanismo `search_tools` que se recicla como retriever).

## El caso de uso, sin adornos

El requisito del usuario externo, literal:

- **Liviano, en un "PC más o menos viejo".** Restricción dura de
  hardware: modelos chicos (probó `qwen3:4b` / `qwen3:1.7b`), poca RAM,
  puede no haber GPU.
- **Offline total.** "No podi conectar ninguna llave de algún servicio"
  — nada de Anthropic/OpenRouter. Esto **excluye por diseño** cuatro de
  los cinco `ModelBackend` de braze y deja exactamente uno: el
  `LocalBackend`.
- **Q&A sobre documentación.** "Dada esta wiki, respóndeme cosas
  básicas de uso." No es programación, no es multi-paso complejo.
- **Cara de GPT.** Interfaz de chat simple para usuarios no técnicos.

El dominio real (visto en sus pantallas, § Evidencia): **Posesiones
Efectivas** (SPE), un sistema legal-administrativo chileno — herederos,
rol UJR, aprobar/rechazar solicitudes, tablas `TBDevRec`/`TBAsiAbo`/
`TBAudit`. Documentación interna, preguntas de uso típico de
funcionarios. Que sea un dominio **legal** sube el peso del grounding:
una respuesta inventada tiene costo real.

Los dos síntomas que ya observó — y que son el pivote de este diseño:

1. **"Cuando integro algo tipo wiki, queda entero aweonao."** Está
   metiendo la documentación entera al contexto. Un modelo chico se
   ahoga: el contexto largo lo distrae y `num_ctx` no alcanza.
2. **"Tiene muchas reglas y llega un punto que la wea no piensa bien."**
   System prompt largo / demasiadas instrucciones degradan el
   razonamiento de un modelo chico. Es exactamente la **doctrina
   contexto-chico** de braze (el A/B accidental deepseek, bitácora
   `6bf2f2a`).

Ambos síntomas son evidencia anecdótica *de la tesis del proyecto*: el
harness liviano compensa la escala del modelo. Su tercer comentario —
"en Ollama me responde mejor que en Codex"— es lo mismo: el harness de
Codex es pesado (muchas reglas y tools en contexto) y castiga al modelo
chico. braze existe para ganar justo ahí.

## Evidencia empírica (2026-07-23): sus dos pantallas

Claudio mostró dos capturas de la MISMA pregunta ("¿qué pasos debe seguir
un usuario UJR para aceptar o rechazar un heredero?"). Son el "antes" del
caso y **confirman y afinan** el diagnóstico.

**Pantalla A — app de Ollama (la simple).** Respuesta **buena**: una
tabla limpia de 5 pasos ("Aprobar o Rechazar un Heredero (Para UJR)"),
con las tablas del sistema y una columna "Importante". Sintetiza bien.
Contexto focalizado → generación limpia. (Nit de render: los `<br>` de
las celdas salen literales.)

**Pantalla B — su app propia (la elaborada).** "Posesiones Efectivas ·
Documentación del sistema", con nav de wiki real, vista "Asistente
local", y **"embeddings · visión disponible"** en el encabezado. La misma
pregunta da una respuesta **peor**: tres bullets tangenciales que no
arman el procedimiento. Los badges lo confiesan: **"retrieval ·
local-search · recuperación hybrid · modo degradado"**, "5 fuentes",
**"90,2 s totales"**.

### Lo que esto cambia respecto de la asunción inicial

Claudio **ya construyó la versión sofisticada** — embeddings, retrieval
híbrido, UI pulida. Su problema **no es falta de infraestructura**. Es
que la versión elaborada **rinde peor que Ollama pelado**, y las
pantallas dicen por qué:

- **El retrieval funciona** (5 fuentes recuperadas). **La síntesis es la
  que falla.** "Modo degradado" = el modelo se ahogó generando, tardó
  90s, y el sistema se rindió: dejó de sintetizar y volcó los fragmentos
  crudos como bullets. Ollama pelado, con contexto focalizado, sintetiza
  limpio.
- Es la **doctrina contexto-chico en un screenshot**: menos maquinaria +
  contexto acotado le gana a más maquinaria + contexto sobrecargado. Sus
  propias pantallas son evidencia anecdótica de la tesis de braze.
- **90 segundos** es además throughput — el mismo tema de hardware del
  resto del proyecto.

### Implicaciones directas para el diseño

1. **El enemigo a matar tiene nombre: el "modo degradado" = síntesis
   sobrecargada.** No es un problema de recuperar mejor, es de no ahogar
   la generación. Refuerza el default **pipeline A**: pocos chunks
   chicos + prompt mínimo + una generación focalizada. El presupuesto de
   síntesis es el recurso escaso, no el retrieval.
2. **Grounding vs. vistosidad — no confundir.** La respuesta "bonita"
   (A) puede estar **menos fundamentada** que la degradada-pero-citada
   (B, cita 5 fuentes reales). En un sistema legal eso importa. El
   objetivo no es "que se vea como A", es **síntesis limpia Y citada**.
   El grounding + cita de fuente del diseño apuntan ahí.
3. **Embeddings no le resolvieron el problema** — ya los tiene y su app
   igual degrada. Confirma que el cuello no está en la calidad del
   retrieval sino en la etapa de generación bajo carga. El léxico del
   MVP no es un downgrade respecto de lo suyo en el eje que le duele.
4. **Menos "hybrid/local-search/embeddings", más disciplina de contexto.**
   Su stack acumuló mecanismos (los badges lo muestran); el resultado es
   peor. La palanca es quitar, no agregar.

## Qué es realmente distinto de braze (y por qué importa)

Braze es un **loop agéntico de tool-calling** estilo Claude Code:
rescate de tool calls, escalada lead/worker, colapso ACI, permisos. El
caso de Claudio **no necesita casi nada de eso** — le sobra el aparato
agéntico. Lo que necesita es:

    documentación → recuperar lo relevante → responder con cita

Eso es RAG clásico. La confusión de "esto es como braze" viene de que
**la pieza más difícil de su problema ya la resolvió braze**: cargar un
modelo local, offline, sin servidor, en hardware chico. El resto
(chunking + retrieval + un chat) es poco y sin dependencias pesadas.

**Regla de encuadre para no sobre-construir:** este modo NO reabre el
eje agéntico. Si algún día el caso pide razonamiento multi-paso sobre la
doc, ese es el escalón "RAG agéntico" (abajo), no el default.

## Inventario: qué se reusa, qué es nuevo

### Gratis — ya existe y está probado

- **`LocalBackend`** (`crates/braze-model/src/local.rs`,
  `docs/local-backend-design-2026-07-20.md`). Inferencia in-process
  sobre `llama-cpp-2`, sin Ollama ni servidor. Ya trae el arreglo de
  VRAM del 2026-07-23 (`483f8e2`: KV en host + micro-batch chico) que lo
  hace correr plano en GPUs chicas — justo el perfil "PC viejo".
  `from_gguf_path` / `from_ollama_model`, tres familias de plantilla.
- **`braze-config`, `braze-session`, el CLI.** Andamiaje de
  configuración, sesión y entrada.
- **El retriever, casi entero — `tool_search::search_stubs`**
  (`crates/braze-engine/src/tool_search.rs:144`). Y este es el hallazgo
  de reuso: el mecanismo `search_tools` (la fig3 del paper) **ya es un
  retriever léxico**. Rankea un corpus por solape de tokens contra una
  query — tokeniza, `to_lowercase`, cuenta hits en nombre+summary,
  ordena por score, corta a `limit`. Es "deliberadamente no-BM25"
  porque el corpus son inventarios chicos. Sustituye "tool stub" por
  "fragmento de wiki" y es el retriever de un RAG, ya escrito y ya
  validado en el paper.

### Nuevo — poco, y sin ML pesado

Una crate `braze-docs` con tres responsabilidades:

```rust
// Un fragmento con su procedencia. La procedencia es lo que habilita
// "cita la fuente" — sin ella el modelo chico no puede fundamentar.
pub struct DocChunk {
    pub id: usize,
    pub source: String,   // p.ej. "instalacion.md"
    pub heading: String,  // p.ej. "## Configurar impresora"
    pub text: String,
}

// Chunker: parte markdown/wiki por headings; cae a párrafos si un
// bloque excede un tope de tokens. Ancla cada chunk a archivo+sección.
pub fn chunk_wiki(dir: &Path) -> Result<Vec<DocChunk>, DocsError>;

// Índice + retriever detrás de un trait, para poder cambiar el backend
// de recuperación sin tocar el resto.
pub trait Retriever {
    fn top_k(&self, query: &str, k: usize) -> Vec<&DocChunk>;
}

// Implementación MVP: calca la lógica de tool_search::search_stubs
// (solape de tokens, cero dependencias, corre en una papa).
pub struct LexicalIndex { chunks: Vec<DocChunk> }
impl Retriever for LexicalIndex { /* ... */ }
```

## Decisión de arquitectura: pipeline RAG como default

Hay dos formas de conectar el retriever al modelo. La elección **no es
de gusto**: la fija el hardware y el tamaño de modelo de Claudio.

### A) Pipeline RAG — retrieve-then-answer, SIN tool call *(default)*

Por cada pregunta del usuario:

1. `LexicalIndex::top_k(pregunta, k=3..5)` → los fragmentos relevantes.
2. Se arma un prompt corto con esos fragmentos inyectados.
3. Una sola generación del `LocalBackend`.

El modelo **nunca ve la wiki entera** ni tiene que *decidir* nada. Elimina
los dos modos de falla que un modelo chico exhibe:

- No hay contexto gigante que lo ahogue → mata el síntoma 1.
- No hay tool call que formular (un `qwen3:1.7b` lo fumbleará) → una
  fuente de error menos.

Es la doctrina contexto-chico aplicada al pie de la letra.

### B) RAG agéntico — un `DocsProvider` que expone `search_docs` *(escalón)*

Se implementa el trait `ToolProvider`
(`crates/braze-tools-core/src/provider.rs:25` —
`list_stubs`/`resolve_schema`/`invoke`) con una tool `search_docs`. El
modelo llama la tool, recibe chunks, razona, puede volver a buscar.
Reusa el **loop completo** de braze sin cambios. Ventaja: razonamiento
multi-paso sobre la doc, follow-ups. Costo: le pide al modelo formular
la query y decidir cuándo buscar — barato para `gpt-oss:20b`, frágil
para un 1.7B.

### Veredicto

**Default A.** Para el hardware y los modelos de Claudio, A es
estrictamente más robusto: quita el contexto largo *y* el tool call, que
son justo sus dos dolores. B es el camino cuando el modelo aguanta
(gpt-oss) o cuando el caso realmente pide multi-paso.

Lo relevante para el esfuerzo: **A y B comparten `LocalBackend` + el
mismo `Retriever`**. Ofrecer las dos es casi gratis — se empieza por A y
B es un envoltorio `ToolProvider` sobre la misma crate.

## El prompt mínimo (contexto-chico + grounding)

El síntoma 2 ("muchas reglas → no piensa bien") se respeta con un system
prompt corto que carga **una sola** regla dura, la anti-alucinación:

```
Responde SOLO con la información de los fragmentos de abajo.
Si la respuesta no está ahí, di "no lo encuentro en la documentación".
Cita la sección de donde sacaste la respuesta.

[fragmentos recuperados, cada uno con su encabezado [source: archivo#sección]]
```

Nada más. Cada regla extra en un modelo chico le resta razonamiento —
**se mide, no se asume** (metodología del proyecto: compilar ≠ funcionar,
y "parece mejor" ≠ mejor). El grounding + la cita de fuente son la
defensa contra la invención, que es el modo de falla #1 de un modelo
chico haciendo Q&A.

## Retrieval: por qué léxico y no embeddings (en el MVP)

La tentación es "usa embeddings, es lo que se hace". Para **este**
requisito, no:

- Embeddings locales exigen **otro modelo cargado** + más RAM + más CPU.
  Rompe "PC más o menos viejo".
- Para "documentación de uso típico" el vocabulario de la pregunta y el
  de la doc se solapan fuerte ("¿cómo configuro la impresora?" ↔ sección
  "Configurar impresora"). El léxico rinde bien en ese régimen.
- El léxico es **determinístico y auditable** — importa para el ángulo
  de publicación (reproducibilidad) y para depurar por qué recuperó tal
  fragmento.

Embeddings quedan como **upgrade opcional detrás del mismo trait
`Retriever`**, no en el MVP. El trait existe precisamente para que ese
cambio no toque el chunker, el prompt ni el loop.

### IDF (2026-07-23): el port de `search_stubs` necesitaba peso por rareza

Probando el server en vivo, "¿qué es Braze?" sobre los `docs/` del
propio proyecto dio una respuesta mala (confundió Braze con solo el
`SessionStore`). El diagnóstico destapó **tres capas** de causa, y vale
como taxonomía limpia de modos de falla del RAG léxico:

1. **Scope del corpus** — el overview canónico ("qué es braze") vive en
   `CLAUDE.md`/`PLAN.md`/`wiki/index.md`, **fuera de `docs/`**. Contribuye
   pero *no es la causa dominante*: meter esos archivos al corpus **no
   cambió** el top del ranking (refutado empíricamente).
2. **Retriever sin IDF** — el port original de `search_stubs` no pesaba
   por rareza. "braze" aparece en **69/77** docs: sin IDF suma +3 a
   cualquier heading tangencial y *distorsiona* en vez de discriminar. Se
   agregó **IDF (BM25 suavizado)** al `LexicalIndex`, default on,
   kill-switch `BRAZE_DOCS_IDF=off` (brazo de ablación). Medición
   before/after sobre el corpus real: **neutro en queries con término
   discriminante** ("localbackend", "search_tools" — mismo top, sin
   regresión) y **mejora en queries que mezclan término ubicuo + raro**
   (un unit test lo fija: sin IDF el chunk relevante queda último, con
   IDF va primero). Pero para "qué es braze" el IDF re-rankeó #2–#5 sin
   arreglarlo del todo: **es el peor caso del léxico** — la query no tiene
   *ningún* término discriminante ("qué"/"es" son ruido, "braze" es
   ubicuo), así que no hay señal que rankear. Ahí el arreglo de fondo es
   embeddings (match semántico) o curación del corpus (una página titulada
   exactamente "Qué es el sistema", sin headings que compitan).
3. **Síntesis del modelo** — el hit #1 ("Braze es un loop agéntico de
   tool-calling estilo Claude Code…") *era* una definición decente en
   ambos modos; gpt-oss igual eligió sintetizar desde un fragmento peor
   (SessionStore). Parte de la falla es del modelo, no del retrieval.

Lectura para el paper: el modo de falla **no es el intuitivo** ("no está
en el corpus"); es la interacción (corpus × retriever-sin-IDF × elección
de fragmento del modelo). Refuerza que la calidad del doc-QA depende del
sistema completo, no solo del modelo.

### Bug del tokenizer (2026-07-23): la puntuación rompía el match

Probando el server, "¿Cómo funciona search_tools?" —una pregunta *buena*,
con término discriminante— devolvió "No lo encuentro" con fuentes
irrelevantes. Causa: el tokenizer (heredado de `search_stubs`) partía por
espacios sin limpiar puntuación, así que `search_tools?` (con el `?` de la
pregunta pegado) **no matcheaba** `search_tools` en los docs → el término
que anclaba se perdía y solo quedaba "funciona" (ruido). Toda pregunta
natural trae `¿`/`?`/`.`, así que rompía el caso normal. Arreglo:
`tokenize()` limpia la puntuación de los **bordes** conservando la interna
(`search_tools`, `gpt-oss:20b`, `co-simulation` siguen siendo un término).
Verificado en vivo: la misma pregunta con signos ahora recupera el doc
correcto de #1 y el server responde bien. **Lección metodológica:** el
smoke sintético no lo agarró porque se tipeó sin signos; el test *en vivo*
sí — otra instancia de "compilar ≠ funcionar" del proyecto. Se agregó un
unit test de regresión con la query puntuada.

## La interfaz "cara de GPT" — RESUELTA con `braze docs --serve`

Claudio quiere "parecido a un GPT como interfaz". La `braze-tui` es
**terminal** — para un usuario de soporte no técnico eso no es "cara de
GPT". Era la única pieza que braze no regalaba barata.

**Insight que fijó el diseño:** una UI de chat necesita que el **modelo
quede caliente**. La CLI one-shot carga el modelo por pregunta (los
~minutos de CPU que medimos) — inaceptable para uso interactivo. Así que
la UI no es "un HTML", es un **server chico** que carga índice+modelo una
vez y atiende muchas preguntas. Eso descartó la opción "HTML estático +
CLI por pregunta" (recargaría el modelo cada vez).

**Lo construido** (`braze docs --serve`, `run_docs`/`serve_docs` en
`main.rs`):

- Server HTTP/1.1 **mínimo hecho a mano** — sin framework web, sobre
  `tokio::net::TcpListener` (solo `+net` en las features de tokio). En el
  espíritu del proyecto (el engine agéntico también es from-scratch); un
  parser de request de ~un puñado de líneas, `Connection: close`, una
  request por conexión.
- Carga el `LexicalIndex` y el backend **una vez** en `DocsServerState`;
  el acceso al modelo se **serializa con un `Mutex`** (el contexto
  llama.cpp no es seguro para decodes concurrentes; para un usuario en
  localhost, atender de a una es lo correcto).
- `GET /` sirve una **página de chat autocontenida** (HTML+CSS+JS inline,
  sin recursos externos): burbujas usuario/bot, caja de texto, indicador
  "Pensando…", y las **fuentes citadas** debajo de cada respuesta
  (rendered con nodos DOM/`textContent`, no `innerHTML`, para no inyectar
  desde un heading). `POST /ask {question}` → `{answer, sources}`.
- Empaquetado para Claudio: un solo binario + `--dir <wiki>`; el usuario
  abre `http://localhost:8080`. Offline total con `--backend local`.

**Verificado en vivo** (offline, `--backend local`, qwen2.5:3b): GET /
sirve el HTML (HTTP 200), POST /ask con la pregunta real del caso →
respuesta de 5 pasos + 5 fuentes. Observación de mecanismo: una query
telegráfica ("cómo rechazo un heredero") produjo una respuesta
degenerada (solo la cita `[1]`) — es la fragilidad de modelo chico ante
prompts pobres, sensible a la query, no un bug del server. Refuerza que
la calidad depende del par (modelo, redacción de la pregunta), lo que
conecta con la pregunta de hardware/modelo pendiente para Claudio.

Sigue como propuesta (no construido): multi-turno con historial (hoy cada
`/ask` es independiente), el escalón RAG agéntico (`DocsProvider`), y
streaming token-a-token a la página (hoy el server colecta y responde
completo).

## Ruta de menor esfuerzo hasta un prototipo demostrable

1. Crate `braze-docs`: `chunk_wiki` + `LexicalIndex` (portar la lógica
   de `tool_search::search_stubs`; es ~40 líneas de scoring).
2. Un modo `braze docs --dir ./wiki --backend local --model <gguf>` que
   ejecuta el pipeline A: recuperar → prompt mínimo → `LocalBackend` →
   responder con cita.
3. Probarlo **contra la doc real de Claudio** — su queja concreta
   ("integro la wiki y queda aweonao") es el test de aceptación directo.
   Verificación en vivo, como todo en el proyecto.
4. Si el modelo chico rinde, ya hay caso aplicado para el paper. Si se
   quiere razonamiento, se sube a B (el `DocsProvider`) sin rehacer nada.

## Ángulo de publicación (nota, no compromiso)

Encaja con el Paper 1 (harness engineering para modelos chicos) por dos
vías:

- **Validación aplicada de la tesis:** "asistente de documentación
  offline en hardware limitado para usuarios no técnicos" aterriza el
  argumento abstracto en un caso real con un colaborador externo.
- **Paper aplicado propio:** RAG léxico + modelo chico + harness mínimo
  para soporte de sistemas en el mundo real. El nicho que el propio
  Claudio intuyó — "soluciones para casos muy limitados"— es publicable.

## Riesgos y preguntas abiertas

- **Hardware objetivo real.** Todo el diseño de modelo depende de qué
  máquina usan los usuarios finales (RAM, ¿GPU sí/no?). `gpt-oss:20b`
  (el mejor modelo del proyecto) quiere ~16GB — **no** es "liviano" para
  un PC viejo. Lo realista ahí es `qwen3:1.7b/4b` o un gemma chico, más
  débiles pero suficientes con buen RAG. **Primera pregunta a Claudio
  antes de recomendar modelo.**
- **Calidad del chunking** en wikis mal estructuradas (sin headings
  consistentes). Fallback a párrafos + tope de tokens; medir.
- **Idioma.** La doc y las preguntas serán en español; el retriever
  léxico es agnóstico, pero conviene un stemming/normalización mínima
  (tildes) — evaluar si hace falta o si el solape crudo basta.
- **Cita de fuente fiel.** Que el modelo cite la sección correcta y no
  una inventada — parte del grading al medir el prototipo.
