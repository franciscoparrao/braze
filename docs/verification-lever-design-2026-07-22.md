# Palanca de verificación de fin de turno (H2) — diseño + pre-registro

> **Estado (2026-07-22):** DISEÑO PRE-REGISTRADO, sin implementar. El
> criterio de adopción de abajo se commitea **antes** de correr el A/B
> que lo decide — corrigiendo la falla de procedencia que la auditoría
> del paper destapó el 21-jul (los criterios de gemma-solo y bare-lead
> entraron a git DESPUÉS de sus sweeps; ver `paper/main.tex` §
> "Registry mechanism" y `docs/sweep-stencil-ab-2026-07-21.md`). Este
> documento es el gate registrar-antes-de-correr en acción.

## Qué motiva esto

El hallazgo **#15** de la bitácora (`docs/bitacora-harness-modelo.html`):
el modelo declara *"cargo test passes"* sin haber ejecutado el test —
confirmado cross-backend el 22-jul (LocalBackend GPU **y** OpenRouter/
deepseek), o sea es **propiedad del modelo, no del backend**, y agravado
bajo la presión del `iteration_cap`. Es una falla de **juicio, no de
capacidad**: no la arregla un modelo más grande (deepseek-v4-flash, a
49/50 en la suite, también confabula), así que una palanca que la ataque
es **model-agnostic** — la única clase de mejora de harness que sigue
teniendo sentido cuando el modelo ya es capaz (ver la discusión del
2026-07-22 sobre por qué el rescate de formato no rinde en modelos
fuertes).

La idea: **mover la verificación de la discreción del modelo a la
garantía del harness.** Hoy el turno termina cuando el modelo deja de
llamar tools y emite texto final — y el harness *confía* en que ese
texto ("los tests pasan") es cierto. La palanca corre el comando de
verificación ella misma antes de aceptar el turno; si falla, inyecta el
resultado real como observación y le da al modelo otra ronda para
arreglarlo.

## Por qué es H2 (y por qué eso importa)

El sistema de hooks actual (`crates/braze-engine/src/hooks.rs`) es
deliberadamente **audit-only (H0/H1)**: un hook OBSERVA (eventos,
requests) pero **no puede mutar el turno**. El módulo doc lo dice
explícito: la superficie transformadora (H2) y de autoridad (H3) quedan
"para después de que el bench demuestre que valen su riesgo".

Esta palanca **es la primera H2**: inyecta una ronda, cambia el
resultado del turno. No es una extensión del hook de auditoría — cruza
la frontera que se dejó cerrada a propósito. Por eso el paso correcto
NO es codear: es pre-registrar el A/B que es, textualmente, el "que el
bench demuestre que vale su riesgo" que el propio módulo pidió.

## Mecanismo preciso

Punto de inyección: `engine/turn.rs:414`, la rama `if
tool_calls.is_empty()` con `text_buffer` no vacío — el modelo dio su
respuesta final. Antes de aceptarla:

1. Si hay un **comando de verificación** configurado para esta sesión
   (ver Config), correrlo (con timeout y captura de stdout/stderr).
2. **Éxito** (exit 0): el turno termina normal.
3. **Fallo** (exit ≠ 0): inyectar la salida (acotada, como el
   `post_edit_check`) como un `ToolResult`/observación sintética con un
   marcador claro ("verification failed: …"), NO terminar el turno, y
   dar al modelo otra ronda — hasta `MAX_VERIFY_ROUNDS` veces. Agotadas,
   terminar el turno pero marcándolo como no-verificado (evento nuevo
   `AgentEvent::VerificationFailed` para que el caller/usuario lo vea).
4. Sin comando configurado, o comando ausente/timeout: **skip
   silencioso** (trace-level), nunca bloquear un turno legítimo — misma
   postura de falla que el `post_edit_check` ("solo agrega feedback,
   nunca convierte un turno bueno en fallo").

## Relación con lo que ya existe (no reinventar)

- **`post_edit_check`** (`braze-tools-local`): corre un formatter/checker
  DESPUÉS DE CADA EDIT y devuelve errores en el mismo tool result. Esta
  palanca es su **generalización a fin de turno**: en vez de "tras cada
  edit, compila", es "antes de aceptar el turno terminado, corre la
  verificación completa y loopea". Reusa su lógica de cap de salida y su
  postura de falla.
- **El grader del bench** ya hace verificación independiente del outcome
  (no confía en el reporte del modelo — por eso caza #15 hoy, marcando
  la corrida como fallida). **Consecuencia crítica para el A/B:** en el
  bench el grader YA atrapa el falso-éxito, así que el efecto medible de
  la palanca NO es "detectar" el falso-éxito (el grader ya lo hace) sino
  **dejar que el modelo se RECUPERE de él** — convertir un falso-éxito
  atrapado en un éxito real, gastando rondas extra. El valor
  *interactivo* (que el usuario vea el resultado real en vez de creerle
  al modelo) es separado y NO medible en el bench — se documenta como
  beneficio cualitativo, no se le pide número.
- **`expect_cargo_check`** de la suite (v8): el comando de verificación
  por tarea del bench; se reusa como el "comando configurado" del brazo
  treatment.

## Criterio pre-registrado (COMMITEAR ANTES DE CORRER)

**Diseño:** A/B pareado sobre la suite `verification-lever.toml` (ver
la ENMIENDA abajo — el design original decía `default.toml`),
ejecutores locales débiles donde el falso-éxito/parada-prematura ocurre
de verdad (`qwen2.5:3b` y `gemma4:e4b` — NO gpt-oss:20b, que satura a
pass^k=100% y no deja headroom). `reps=3`, seed fijo, McNemar exacto
pareado por (tarea, rep).

> **ENMIENDA DE SUITE (2026-07-22, ANTES de correr — commiteada con el
> cableado del bench y la suite nueva).** El design original apuntaba a
> `default.toml`, pero al cablear el bench se descubrió que
> `default.toml` tiene **cero** tareas cargo-verificables (sus tareas de
> "código" editan `.txt`/`.py`), así que no puede medir esta palanca: el
> gate nunca dispararía. Se sustituye por
> `crates/braze-bench/suites/verification-lever.toml` — 6 errores de
> compilación de Rust (borrow/move/tipo/mut) que un ejecutor débil deja
> roto al declarar "listo", con `expect_cargo_check` como criterio de
> pass y `cargo check` como comando del gate. Es una corrección de
> instrumento (la suite no puede medir lo que el criterio pide), no un
> ajuste del criterio de adopción — ese queda intacto. Esta enmienda se
> registra ANTES del sweep para no repetir la falla de procedencia del
> 21-jul (criterios que entraron a git DESPUÉS de sus datos).

> **RUN CONFIRMATORIO POTENCIADO (2026-07-22, ANTES de correr).** El
> primer A/B (n=18 = 6 tareas × 3 reps por brazo,
> `docs/sweep-verification-lever-ab-2026-07-22.md`) mostró dirección
> impecable (0 reversiones en 8 movimientos) pero significancia marginal
> (gemma p=0.062). No es p-hacking correr con más n: la dirección y el
> tamaño de efecto YA están establecidos por el piloto; esto fija un
> **n adecuado de una vez** — 20 bugs de compilación Rust distintos ×
> 3 reps = **60 pares por ejecutor** (suite
> `verification-lever-n20.toml`) — como el test potenciado del MISMO
> criterio pre-registrado. n comprometido ANTES de correr (no "correr
> hasta p<0.05"); se reporta el resultado sea cual sea, incluido un
> efecto que se encoja. El piloto n=18 queda como piloto; ESTE es el
> test.

- **Control:** fin de turno actual (`turn.rs:414` sin cambios).
- **Treatment:** gate de verificación con `MAX_VERIFY_ROUNDS = 2` y el
  comando `expect_cargo_check` de cada tarea como verificación.

**Hipótesis:** el treatment sube el pass rate en las tareas donde el
modelo tiende a parar antes de terminar o a declarar éxito falso, a
costa de más rondas/tokens promedio.

**Criterio de adopción (ADOPT si TODO se cumple):**
1. pass rate del treatment ≥ control **+5 pp** en al menos uno de los
   dos ejecutores débiles, con IC Newcombe 95% del delta fuera de cero;
2. **y** el mecanismo verifica: el conteo de turnos que dispararon el
   gate y luego pasaron el re-check es > 0 (o sea, la recuperación
   ocurrió de verdad, no es ruido);
3. **y** el costo en rondas promedio del treatment ≤ 1.5× el control
   (la palanca no debe degenerar en loops de "no puedo arreglarlo").

**REJECT** si el pass rate no mejora fuera de ruido en ningún ejecutor,
**o** el mecanismo no verifica (el gate dispara pero el modelo nunca se
recupera — #15 en su forma dura: el modelo ignora la observación
inyectada igual que ignoró correr el test), **o** el costo de rondas
explota. **Una sola iteración permitida** (misma disciplina que el A/B
de constrained decoding): si REJECT por costo, se permite reajustar
`MAX_VERIFY_ROUNDS` a 1 y re-medir una vez.

**Registro externo:** crear la OSF Registration con este texto ANTES de
correr el sweep (osf.io/registries) — esta vez de verdad antes, no
después. Si no se alcanza a crear, el paper/doc debe decir "committeado
en git el 2026-07-22, commit <hash>, OSF pendiente" y el hash de commit
DEBE preceder al JSON del sweep (verificable con `git log -S`).

## Riesgos / caveats honestos

- **El caveat de fondo (#15 en su forma dura):** un modelo que confabula
  "test pasa" puede *también* ignorar la observación de fallo inyectada
  y volver a declarar éxito. El gate fuerza el RESULTADO al contexto,
  pero no puede forzar al modelo a actuar bien sobre él. Si eso pasa, el
  mecanismo no verifica y el criterio dice REJECT — y ese resultado nulo
  sería en sí un hallazgo (la verificación forzada no basta; hace falta
  algo más fuerte, p.ej. no dejar terminar hasta exit 0, que es H3/
  autoridad). Al menos el usuario ve el resultado real (valor
  interactivo intacto aunque el bench dé nulo).
- **Autoridad (roza H3):** correr un comando arbitrario de verificación
  es ejecución con efectos. En el bench es el sandbox de tarea; en uso
  interactivo el comando pasa por la misma capa de permisos que
  `shell_exec`. NO auto-ejecutar comandos no confirmados fuera del
  sandbox.
- **Loop infinito:** acotado por `MAX_VERIFY_ROUNDS`. Agotado, termina
  con el turno marcado no-verificado, nunca cuelga.
- **Costo:** rondas y tokens extra — parte de lo que el criterio #3 mide.

## Prior art (no sobre-vender novedad)

Loops de agente guiados por ejecución / test-driven ya existen: el
auto-test de Aider, la verificación de SWE-agent. La contribución NO es
inventar la idea — es **medirla como palanca ablacionable a escala de
modelo chico** (el método del paper: ¿cuánto rinde forzar la
verificación, y en qué régimen?), y el hallazgo específico de si el
falso-éxito de #15 se recupera o resiste la observación forzada.

## Plan de implementación (si el diseño se aprueba)

1. `braze-config`: `VerificationConfig { command, timeout, max_rounds }`
   (patrón `FormatterConfig`); toggle de ablación
   `+ablate:no-verify-gate` en el bench.
2. `braze-engine`: el gate en `turn.rs:414` + evento
   `AgentEvent::VerificationFailed` + `VerificationRan` (para el conteo
   del mecanismo).
3. `braze-bench`: cablear `expect_cargo_check` como el comando del brazo
   treatment; reporte del conteo gate-disparado/recuperado.
4. Verificación en vivo (compilar ≠ funcionar): pty contra un modelo que
   confabule, ver el gate disparar y la observación inyectada.
