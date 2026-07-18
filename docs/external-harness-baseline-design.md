# Diseño pre-registrado: baseline de harness externo (bare lead+executor)

Fecha: 2026-07-13
Estado: **CERRADO — el criterio dispara "LA COMPOSICIÓN BASTA".** El
loop bare lead+executor (84/95, 88.4% [80.4,93.4]% Wilson) es
**estadísticamente indistinguible** del compuesto completo de `braze`
(85/95, 89.5% [81.7,94.2]%) — delta (compuesto − bare) = +1.1pp,
Newcombe 95% CI [−8.2, +10.3], cruza cero limpio. También indistinguible
de `gemma4:e4b` solo (87/95, 91.6%) — delta (bare − solo) = −3.2pp,
Newcombe 95% CI [−12.1, +5.7]. Sweep:
`docs/sweep-external-bare-lead-2026-07-13.json`. Sigue la disciplina de
pre-registro del planner (`PLAN.md` § split), del explorador
(`docs/explorador-aislado-ab-design.md`), de constrained-decoding
(`docs/constrained-decoding-ab-design.md`) y del baseline de
`gemma4:e4b` (`docs/gemma4-e4b-solo-baseline-design.md`): el criterio se
escribió y committeó ANTES del sweep que lo decide, y no se modificó
después de ver el número.

**Nota de secuencia, honesta**: a diferencia de los documentos anteriores,
la construcción del comparador (`crates/braze-bench/src/bare_lead_baseline.rs`)
ocurrió ANTES de escribir este documento — Fase 3 requería explorar
viabilidad técnica (¿se puede reusar `braze_model` sin `Engine`? ¿qué tan
independiente puede ser la implementación?) antes de poder escribir un
criterio concreto y realista. Lo que la disciplina de pre-registro exige
—que el criterio se fije antes del **sweep que decide el resultado**— se
mantiene: solo se corrió un smoke test de 2 tareas para validar que la
infraestructura funciona mecánicamente (produce respuestas PASS/FAIL
sensatas), no el sweep de 95 corridas que este documento pre-registra
abajo. Ningún número de ese sweep existe todavía al escribir este
criterio.

Origen: `/paper-review-emse` sobre `paper/main.tex` (review completa en
`~/vault/journals/emse/reviews-generated/2026-07-13_16-34_braze-harness-paper.md`,
checklist en `docs/emse-review-2026-07-13-checklist.md`, Issue 1). Cita
textual de la review: *"every comparison in the paper is braze against
itself... a claim like 'the harness compensates the model' [is]
evidenced by exactly one harness."* El paper mismo ya admite el hueco en
`\S\ref{sec:threats}` ("Suite coverage"): *"an external, non-braze
baseline harness was planned... but was never implemented past a stubbed
interface."*

## La hipótesis

El headline del paper es que el compuesto lead+executor de `braze`
(1B+lead, 89.5%) rescata dramáticamente al 1B de su baseline (19%). Pero
`braze` no es solo "lead abre, executor ejecuta" — es esa composición
MÁS rescate textual, compactación de observaciones, deferral de tools,
post-edit check, etc. (Tabla de levers del paper). ¿Cuánto del +70pp le
corresponde a la composición lead+executor en sí, y cuánto a la
ingeniería adicional específica de `braze`?

Predicción si "la composición basta" es cierta: un loop mínimo
lead+executor, sin ninguna otra palanca de `braze`, sobre la MISMA
composición (mismo modelo lead `gemma4:e4b`, mismo modelo executor
`llama3.2:1b`, mismo `lead_turns=3`), alcanza un pass rate **cercano** al
89.5% del compuesto completo de `braze` — es decir, la ingeniería
adicional de `braze` no agrega mucho más allá de la composición cruda.

Predicción alternativa (la que justificaría la ingeniería de `braze`
como valor agregado real, no solo la composición): el loop bare cae
**sustancialmente por debajo** de 89.5% — evidencia de que el rescate
textual, la compactación, o el manejo de errores de `braze` son los que
realmente cierran la brecha, no la mera presencia de un lead.

Tercer punto de referencia (ya medido, Fase 1): `gemma4:e4b` solo
alcanza 91.6% en esta suite — un techo de capacidad que ni el compuesto
de `braze` ni (presumiblemente) el loop bare deberían superar de forma
sostenida.

## Mecanismo implementado

`crates/braze-bench/src/bare_lead_baseline.rs` (`BareLeadExecutor`,
implementa `ExternalHarness` de `external.rs`), disponible vía
`--external "bare-lead:<spec>"` en `braze-bench` (spec debe llevar
sufijo `+lead:`). Detalle completo de qué tiene y qué NO tiene en el
doc comment del módulo; resumen:

**Tiene**: loop de tool-calling propio (no `braze_engine::Engine`), lead
abre los primeros `lead_turns=3` rounds (igual que
`EscalatingBackend::DEFAULT_LEAD_TURNS`, pero reimplementado desde cero
— NO reusa `EscalatingBackend` — para que la independencia de
implementación sea real, no solo nominal), ejecución de tools real vía
`braze_tools_local::LocalToolsProvider` (mismas 6 tools, mismo
`WorkdirAllowlist` de sandbox, misma postura `DenyAll` para acciones
irreversibles que el resto de `braze-bench`), cap de 20 rounds (igual a
`Engine::MAX_TURN_ITERATIONS`), system prompt genérico de 4 oraciones
(NO el `default_system_prompt` de 463 líneas de `braze`).

**NO tiene**: rescate textual de tool calls malformados, compactación de
observaciones, deferral de tools (schemas completos desde la ronda 1),
post-edit check, best-of-n, harness notes, task list/planner, project
memory.

Validado: 143 tests unitarios verdes (2 nuevos en
`bare_lead_baseline.rs`), clippy `-D warnings` limpio en el workspace,
smoke test de 2 tareas reales contra Nitro (`read_smoke`, `no_tool_smoke`)
— ambas PASS, confirmando que el loop resuelve tareas reales antes de
comprometerse al sweep completo.

## Brazos y executors

Un solo arm nuevo, misma suite y convención que el resto del paper:
suite `crates/braze-bench/suites/default.toml` (19 tasks), 5
repeticiones ($n{=}95$), temp 0.2, Nitro, `--no-ollama-stop`.

| Arm | Spec | Qué mide |
|---|---|---|
| bare lead+executor | `--external "bare-lead:ollama:llama3.2:1b+lead:ollama:gemma4:e4b"` | Composición lead+executor sin ninguna otra palanca de `braze` |

Escala elegida: **1B** — la fila más informativa (mayor gain relativo
del compuesto de `braze` en el paper, +70pp) y la que hace la pregunta
de este documento más nítida. Si el cómputo de Nitro lo permite después,
extender a 3B/7B/coder queda como trabajo futuro, no bloqueante para
este criterio.

No hace falta re-correr ningún arm existente de `braze` — el compuesto
1B+lead (85/95, 89.5%) y `gemma4:e4b` solo (87/95, 91.6%) ya están
medidos (Fase 1, `docs/gemma4-e4b-solo-baseline-design.md`).

## Criterio pre-registrado

Sea $Y$ el pass rate (Wilson 95% CI) del loop bare, $n{=}95$. Comparado
contra el compuesto completo de `braze` (85/95, 89.5% [81.7,94.2]%) vía
delta Newcombe 95% CI (compuesto `braze` $-$ $Y$):

- **La composición basta** (la ingeniería adicional de `braze` no es lo
  que cierra la brecha) si $Y \geq 79\%$ **Y** el intervalo del delta no
  excluye claramente el cero a favor del compuesto de `braze` (es decir,
  no hay evidencia de que `braze` le gane al loop bare fuera de ruido).
  En ese caso, el paper debe reportar explícitamente que gran parte del
  headline es atribuible a la composición lead+executor per se, no a la
  ingeniería específica de `braze` — sección nueva o expansión de
  `\S\ref{sec:curve}`/`\S\ref{sec:discussion}`.
- **La ingeniería de `braze` agrega valor real** si $Y \leq 69\%$ **Y**
  el intervalo del delta excluye cero a favor del compuesto de `braze`
  (`braze` claramente por encima del loop bare, fuera de ruido). En ese
  caso, el paper gana su primera comparación genuinamente externa
  positiva: `braze` no es solo "cualquier lead+executor," su ingeniería
  específica mide una diferencia real.
- **Zona intermedia** (69-79%, o CIs solapados sin exclusión clara):
  reportar como resultado mixto — parte de la ganancia es composición,
  parte es ingeniería — sin forzar una lectura binaria que los datos no
  sostienen.
- Reportar además $Y$ contra `gemma4:e4b` solo (91.6%) y contra el
  baseline sin asistir del 1B (19%) — posiciona el loop bare en el mismo
  espacio de referencia que el resto de la curva, independiente del
  criterio adopt/reject de arriba.

**Sin cláusula de iteración**: a diferencia de constrained-decoding, este
documento no propone una palanca a ajustar tras un resultado ambiguo —
es una medición de un punto de comparación, igual que el baseline de
`gemma4:e4b`. El resultado, cualquiera que sea, se reporta como está.

## Resultado (2026-07-13)

$Y = 84/95 = 88.4\%$ Wilson 95% CI $[80.4, 93.4]\%$
(`docs/sweep-external-bare-lead-2026-07-13.json`). Desglose por skill:
`single_tool` 35/35 (100%), `no_tool` 15/15 (100%), `multi_step` 8/15
(53%), `error_recovery` 14/15 (93%), `distractor_selection` 12/15
(80%). Mecanismo: `schema_fail=0`, `exec_fail=0`, `denied=0` en las 95
corridas — el loop bare no reporta fallos de schema ni denegaciones (el
executor 1B, pese a su historial de emitir JSON de function-call como
texto en vez de tool calls reales — ver
`docs/grader-validation-2026-07-13.md` — no mostró ese patrón en esta
corrida específica, o lo mostró en instancias donde igual convergió a
una respuesta correcta).

**Comparación contra el compuesto completo de `braze`**: 85/95 = 89.5%
Wilson 95% CI $[81.7, 94.2]\%$. Delta (compuesto − bare) = $+1.1$pp,
Newcombe 95% CI $[-8.2, +10.3]$ — **cruza cero limpio**.

**Comparación contra `gemma4:e4b` solo**: 87/95 = 91.6% Wilson 95% CI
$[84.3, 95.7]\%$. Delta (bare − solo) = $-3.2$pp, Newcombe 95% CI
$[-12.1, +5.7]$ — **también cruza cero limpio**.

**Veredicto contra el criterio pre-registrado**: $Y = 88.4\% \geq 79\%$
**Y** el intervalo del delta (compuesto `braze` − bare) no excluye cero
a favor del compuesto → **LA COMPOSICIÓN BASTA**, sin ambigüedad — no
es la zona intermedia 69-79%, es directamente el escenario que el
criterio identificó como más informativo.

**La lectura de conjunto (tres mediciones independientes, todas
mutuamente indistinguibles)**: `gemma4:e4b` solo (91.6%) ≈ compuesto
completo de `braze` (89.5%) ≈ loop bare sin ninguna de las palancas de
`braze` (88.4%). Nada en estos datos separa "el compuesto logra algo
más allá de lo que el propio `gemma4:e4b` alcanzaría solo" de "el
compuesto simplemente hereda el techo de `gemma4:e4b`, y ni la
composición lead+executor en sí ni la ingeniería adicional de `braze`
(rescate, compactación, deferral, post-edit check) le agregan una
ganancia medible por encima de eso, en esta suite y a esta escala**.
Esto no es evidencia de que la ingeniería de `braze` no sirva para
nada — los intervalos son anchos ($n{=}95$) y compatibles con una
diferencia de hasta ~10pp en cualquier dirección — es evidencia de que,
si sirve, esta suite y este tamaño de muestra no lo detectan.

**Qué SÍ sigue siendo un resultado genuino**: el propio ejercicio de
construir un comparador genuinamente aislado (Issue 1 de la review
EMSE) y encontrar que sobrevive al escrutinio — no colapsa, no se
comporta erráticamente, resuelve `single_tool`/`no_tool` perfectamente
y solo se degrada en las skills multi-step/distractor donde el 1B ya
es débil de por sí — es en sí mismo información: el patrón "lead abre,
executor ejecuta" es robusto incluso sin ningún andamiaje adicional,
al menos a esta escala.

## Riesgos anotados

- **No es verdaderamente "una implementación distinta" en el sentido
  fuerte** (LangGraph/AutoGen u otro framework de terceros) — sigue
  siendo Rust, sigue usando `braze_model::{OllamaBackend, ModelBackend}`
  para las llamadas HTTP a Ollama, y sigue usando
  `braze_tools_local::LocalToolsProvider` para ejecutar tools. La
  independencia es sobre el LOOP DE ORQUESTACIÓN (no reusa `Engine` ni
  `EscalatingBackend`) y la ausencia de las palancas de ingeniería en
  cuestión, no sobre el stack HTTP/tools subyacente — eso es
  infraestructura común, no la variable bajo prueba. Se documenta esta
  limitación explícitamente en vez de sobrevender "harness
  independiente" como si fuera un framework de terceros.
- **Un solo lead_turns, un solo cap de rounds** — ambos igualados a los
  defaults de `braze` a propósito (para que una diferencia de resultado
  no sea atribuible a un split point distinto), pero eso también
  significa que este documento no explora si un `lead_turns` distinto
  cambiaría el resultado del loop bare.
- **System prompt genérico de 4 oraciones** — podría ser
  desproporcionadamente peor que el de 463 líneas de `braze` por razones
  no relacionadas con las palancas bajo prueba (p.ej. el prompt de
  `braze` puede tener guía específica de formato de tool-calling que
  ayuda independientemente del rescate textual). Riesgo real: si el loop
  bare rinde mal, parte de eso podría ser "prompt peor," no solo
  "sin rescate/compactación." Se reporta el resultado igual, pero se
  anota esta confusión posible en Threats to Validity.
- **Sin rescate textual significa que un tool call malformado se pierde
  sin reparación** — para `llama3.2:1b` (el executor de esta corrida,
  con historial documentado de emitir JSON de function-call como texto
  plano en vez de tool calls reales, ver transcripciones de
  `docs/grader-validation-2026-07-13.md`) esto podría deprimir el pass
  rate del loop bare de forma que no separa limpiamente "la composición
  no basta" de "este executor específico necesita rescate textual para
  funcionar en absoluto." Vale la pena leer las transcripciones (con
  `BRAZE_BENCH_KEEP_SESSIONS=1`) si el resultado es sorprendentemente
  bajo, antes de interpretarlo solo desde el número agregado.

## Conexión con el paper

Entra como subsección nueva (`\S\ref{sec:external}` o similar, entre
`\S\ref{sec:mechanism}` y `\S\ref{sec:planner}`, o como parte de
`\S\ref{sec:threats}` si el resultado es negativo/no concluyente) citando
el veredicto contra el criterio de arriba. Es la primera comparación del
paper que no es `braze` contra sí mismo — se referencia explícitamente
desde el Threats to Validity actual ("Suite coverage" /
"an external... baseline harness was planned... but never implemented")
como el hueco que se cerró.

## Adenda: aumento de potencia (2026-07-13)

Ver `docs/gemma4-e4b-solo-baseline-design.md` § "Adenda: aumento de
potencia" — mismo razonamiento, aplicado a los tres brazos (incluido
este). Decisión y resultado consolidados en
`docs/power-increase-2026-07-13.md`.

## Registro externo (OSF)

Segundo uso del registro externo del proyecto (el primero,
`gemma4-e4b-solo-baseline-design.md`, sigue con el registro pendiente
por falta de credenciales OSF en este entorno). Mismo texto/estructura
que ese documento (Título/Hypothesis/Design/Analysis plan, secciones de
arriba) listo para pegar en osf.io/registries. **Misma decisión
explícita**: se corre el sweep con el criterio ya committeado en git,
sin esperar el registro OSF — la razón es la misma (medición sin
cláusula de iteración, no adopción irreversible de una palanca), y el
mismo caveat de disclosure aplica si no se cierra antes de submission.

## Qué NO es este documento

No es una comparación contra un framework de terceros real
(LangGraph, AutoGen, mini-swe-agent) — es una ablación interna que aísla
"composición lead+executor" de "ingeniería adicional de `braze`,"
reusando la infraestructura de bajo nivel (`braze_model`,
`braze_tools_local`) que no es ella misma objeto de la pregunta. Una
comparación contra un framework genuinamente de terceros sigue siendo
trabajo futuro más caro, fuera del alcance de este documento. Costo de
lo que SÍ se hizo: implementación ya construida (~350 líneas nuevas +
tests), sweep de decisión pendiente: $n{=}95$, ~15-20 min de Nitro según
los tiempos observados en Fase 1/2 para arms similares.

## Candidato futuro para comparación genuinamente externa: Pi (pi.dev)

Encontrado 2026-07-13, mientras el sweep de decisión de este documento
corría. **Pi** (Earendil Inc., MIT, vía npm) es un "minimal agent
harness" con la filosofía inversa a `braze`: núcleo chico, todo lo demás
(sub-agentes, modo plan, sandboxing, protected-paths) como extensión
opcional — *"Adapt Pi to your workflows, not the other way around."*

Por qué es un candidato mejor que `BareLeadExecutor` para la versión
MÁS fuerte de este documento (Issue 1 de la review EMSE en su forma más
estricta — "no es artifact de esta implementación específica"):

- **Soporta Ollama explícitamente** entre sus 15+ providers — corre
  contra los mismos modelos locales que el resto del paper, sin cambiar
  de infraestructura.
- **Modo scriptable real, apto para un sweep no interactivo**: `pi -p
  "query"` (print mode, para scripts), `--mode json` (event streams),
  RPC por stdin/stdout — encaja con el diseño original de `external.rs`
  ("shelling out to its CLI inside the sandbox directory," nunca
  implementado hasta este documento).
- **Es código genuinamente ajeno** — a diferencia de `BareLeadExecutor`
  (que reusa `braze_model`/`braze_tools_local`, ablacionando *dentro*
  del código de `braze`), un adapter que shell-outea al binario de Pi no
  comparte ninguna línea de implementación con `braze`. Cierra la
  crítica de la review en su forma más fuerte, no solo la forma que
  este documento ya resuelve (aislar composición vs. ingeniería).

Caveat a resolver antes de wirearlo: Pi no trae flujo de confirmación
de permisos integrado ("No permission popups - Run in a container, or
build your own confirmation flow") — un sweep no interactivo necesita
un flujo que auto-apruebe o auto-deniegue (para igualar la convención
`DenyAll` del resto de `braze-bench`) sin bloquear esperando input
humano.

**No se persigue en este plan** (Fase 3 ya cerró su criterio con
`BareLeadExecutor`) — queda anotado como el paso natural siguiente si
se quiere la versión más fuerte de esta comparación en una iteración
futura del paper, implementando `ExternalHarness` para un
`PiSubprocessHarness` que shell-outea al binario `pi` en modo print/JSON
dentro del sandbox de cada tarea.
