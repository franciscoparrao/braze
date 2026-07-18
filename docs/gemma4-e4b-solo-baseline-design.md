# Diseño pre-registrado: baseline solo de `gemma4:e4b`

Fecha: 2026-07-13
Estado: **CERRADO — el criterio dispara "REVISAR FRAMING".**
`gemma4:e4b` solo (87/95, 91.6% [84.3,95.7]% Wilson) es
**estadísticamente indistinguible** del compuesto 1B+lead (85/95,
89.5% [81.7,94.2]%) — delta (compuesto − solo) = −2.1pp, Newcombe 95%
CI [−10.9, +6.6], cruza cero limpio, y el punto estimado incluso
favorece levemente al modelo solo. Sweep:
`docs/sweep-gemma4-e4b-solo-2026-07-13.json`. **Registro OSF quedó
pendiente** (ver § "Registro externo" — decisión explícita de correr
sin esperarlo, dado que esto es una medición descriptiva sin cláusula
de iteración). Sigue la disciplina de pre-registro del planner
(`PLAN.md` § split), del explorador
(`docs/explorador-aislado-ab-design.md`) y de constrained-decoding
(`docs/constrained-decoding-ab-design.md`): el criterio se escribió
ANTES del sweep que lo decide, y no se modificó después de ver el
número.

Origen: `/paper-review-emse` sobre `paper/main.tex` (review completa en
`~/vault/journals/emse/reviews-generated/2026-07-13_16-34_braze-harness-paper.md`,
checklist en `docs/emse-review-2026-07-13-checklist.md`, Issue 2). El
paper mismo ya admite el hueco en `\S\ref{sec:threats}` ("Single fixed
helper model", línea 905): "we do not know how sensitive the curve's
shape is to that choice" — este documento cierra esa pregunta para el
caso más importante, el propio `gemma4:e4b`.

## La hipótesis

El headline del paper (`\S\ref{sec:curve}`) es que un executor de 1B
con lead (89%) supera a los baselines sin asistir de 3B (68%) y 7B
(80%). `gemma4:e4b` es el modelo que abre el turno en TODO arm `+lead`
del paper — pero su propio pass rate en solitario, sobre la misma
suite, nunca se midió. Si `gemma4:e4b` solo ya saca un número alto en
esta suite, gran parte de la ganancia atribuida al mecanismo de "lead
proactivo" podría ser, en los hechos, "enrutar más trabajo a un modelo
más capaz" — una explicación mucho menos interesante que "el harness
compensa la escala".

Predicción si la hipótesis "es principalmente capacidad del lead" es
cierta: `gemma4:e4b` solo (baseline arm, sin lead, sin planner, harness
default) cae **cerca o por encima** del 89% del compuesto 1B+lead — es
decir, el compuesto no le agrega mucho al techo que el propio
`gemma4:e4b` ya alcanzaría actuando solo.

Predicción alternativa (la que confirma el framing actual del paper):
`gemma4:e4b` solo cae **claramente por debajo** de 89% — el compuesto
1B+lead logra algo que ni el 1B solo (19%) ni el `gemma4:e4b` solo
logran por separado, evidencia de sinergia genuina entre "el lead abre,
el executor de 1B ejecuta" y no solo de sustitución de capacidad.

## Mecanismo mínimo a implementar

Ninguno — es la misma suite, mismo harness default, un solo backend
spec nuevo. No requiere cambios de código en absoluto (confirmado:
`ollama:gemma4:e4b` ya es un spec válido y genérico en
`crates/braze-bench/src/backend_spec.rs::parse_single`, sin
special-casing; `gemma4:e4b` ya aparece como literal en tests y en
todo arm `+lead:`/`+plan:` del sweep de curva — nunca como fila propia
sin modificador).

## Brazos y executors

Un solo arm nuevo, mismos parámetros que el resto de la curva de
escala (`docs/sweep-curva-multiescala-2026-07-10.md`): suite
`crates/braze-bench/suites/default.toml` (19 tasks), 5 repeticiones
($n{=}95$), temp 0.2, Nitro, `--no-ollama-stop`, un sweep a la vez.

| Arm | Backend spec | Qué mide |
|---|---|---|
| `gemma4:e4b` solo | `ollama:gemma4:e4b` (sin `+lead:`, sin `+plan:`, sin `+ablate:`) | Techo de capacidad del propio modelo lead, aislado del compuesto |

No hace falta re-correr ningún arm existente — este es un punto de
referencia nuevo que se superpone sobre la Fig.~1 ya generada, no
reemplaza ninguno de los 16 cells del sweep original.

## Criterio pre-registrado

Sea $X$ el pass rate (Wilson 95% CI) de `gemma4:e4b` solo, $n{=}95$.

- **Revisar el framing** de "+70pp a 1B / harness compensa una orden
  de magnitud de parámetros" (abstract, intro, contribuciones,
  `\S\ref{sec:curve}`) si $X \geq 80\%$ **O** si el intervalo Newcombe
  95% del delta (1B+lead $-$ $X$) no excluye claramente el cero a
  favor del compuesto. En ese caso, agregar explícitamente: "part of
  the 1B+lead gain reflects `gemma4:e4b`'s own standalone capability on
  this suite ($X$\%)" y reencuadrar la contribución como "el lead
  proactivo enruta eficientemente hacia un modelo capaz" en vez de
  "el harness compensa la escala" como mecanismo distinto de "usar un
  modelo más grande en algún punto del loop".
- **Confirmar el framing actual** si $X \leq 70\%$ con el intervalo
  Newcombe 95% del delta (1B+lead $-$ $X$) excluyendo cero a favor del
  compuesto — evidencia de que el compuesto logra algo que ni el 1B
  solo (19%) ni `gemma4:e4b` solo logran por separado. `gemma4:e4b`
  solo se cita entonces como evidencia a favor, no como amenaza.
- **Zona intermedia** (70-80%, o CIs solapados sin exclusión clara):
  reportar honestamente como "parcialmente atribuible a la capacidad
  del lead, parcialmente a la composición" — matizar el lenguaje de
  "compensa una orden de magnitud" independientemente del resultado
  puntual (ver Fase 4 del plan, ítem "Abstract").
- Reportar además $X$ contra los baselines sin asistir de 3B (68%),
  7B (80%) y el techo qwen3.5-coder (98%) — posiciona a `gemma4:e4b`
  en su propio tier de capacidad para el lector, independiente del
  criterio adopt/reject de arriba.

No hay cláusula de iteración: es una medición de un punto de
referencia, no una palanca a ajustar — el resultado, sea cual sea, se
reporta y se usa para calibrar el framing, no se itera.

## Resultado (2026-07-13)

$X = 87/95 = 91.6\%$ Wilson 95% CI $[84.3, 95.7]\%$
(`docs/sweep-gemma4-e4b-solo-2026-07-13.json`). Desglose por skill:
`single_tool` 30/35, `no_tool` 15/15, `multi_step` 14/15,
`error_recovery` 13/15, `distractor_selection` 15/15 — mecanismo
limpio (`schema_fail=0` en las 95 corridas).

**Comparación contra el compuesto 1B+lead**: 85/95 = 89.5% Wilson 95%
CI $[81.7, 94.2]\%$ (recontado desde
`docs/sweep-curva-multiescala-2026-07-10.partial-1b.json`, backend
`ollama:llama3.2:1b+lead:ollama:gemma4:e4b`, coincide con el 89% que
reporta el paper). Delta (compuesto − solo) = $-2.1$pp, Newcombe 95%
CI $[-10.9, +6.6]$ — **cruza cero limpio, el punto estimado incluso
favorece levemente a `gemma4:e4b` solo sobre el compuesto**.

**Veredicto contra el criterio pre-registrado**: $X = 91.6\% \geq
80\%$ **Y** el intervalo del delta no excluye cero a favor del
compuesto → **REVISAR FRAMING**, sin ambigüedad — no es un caso límite
de la zona 70-80%, es directamente el escenario que el criterio
identificó como el más informativo: nada en estos datos distingue "el
compuesto logra algo que `gemma4:e4b` no lograría solo" de "el
compuesto simplemente hereda el techo de capacidad de `gemma4:e4b`".

**Contra los otros baselines** (posicionamiento, fuera del criterio
adopt/reject): `gemma4:e4b` solo (91.6%) supera al baseline sin
asistir de 3B (68%) y de 7B (80%), y se acerca al techo de
qwen3.5-coder (98%, CI se solapa parcialmente) — `gemma4:e4b` no es un
"lead barato y modesto", es en esta suite uno de los modelos más
capaces de todo el estudio, comparable a la escala 7B-o-mejor pese a
su tamaño nominal menor.

**Qué SÍ sigue siendo un resultado genuino**: el salto de 19% (1B
solo) a 89.5% (1B+lead) es real y grande — el compuesto claramente
rescata al 1B de su baseline propio. Lo que este resultado mata es
específicamente el framing "compensa una orden de magnitud de
parámetros" comparado contra 3B/7B *sin asistir* — esa comparación
mezcla "el 1B mejora mucho con ayuda" (cierto) con "esa ayuda supera lo
que un sistema de un solo modelo lograría con el mismo presupuesto de
capacidad" (no soportado por estos datos: `gemma4:e4b` solo, sin
ningún 1B adjunto, ya iguala al compuesto).

## Adenda: aumento de potencia (2026-07-13, post-Fase 3)

Tras cerrar también el baseline de harness externo
(`docs/external-harness-baseline-design.md`), las tres mediciones
independientes (`gemma4:e4b` solo, compuesto de `braze`, loop bare)
resultaron mutuamente indistinguibles a $n{=}95$ — un nulo con CIs
anchos (~±6-7pp), compatible con hasta ~10pp de diferencia real en
cualquier dirección. Decisión (usuario, antes de correr nada de esta
adenda): angostar los tres intervalos agregando **10 repeticiones más**
a cada brazo ($n{=}285$ total por brazo, el mismo tamaño que ya usa el
sweep de aislamiento de mecanismo del paper, `lead-3brazos`) en vez de
decidir el framing del título con el nulo ancho de $n{=}95$. No es un
criterio nuevo — el adopt/reject de arriba no cambia — es la misma
pregunta con más potencia. Resultado de esta adenda en
`docs/power-increase-2026-07-13.md`.

## Riesgos anotados

- **Clasificación de `gemma4:e4b` desconocida**: el paper no documenta
  si `gemma4:e4b` tiene fine-tuning de function-calling o es genérico
  (a diferencia de la clasificación explícita que sí da para
  llama3.2:1b/qwen2.5/qwen3.5-coder en `\S\ref{sec:setup}`,
  "Executors and fixed roles"). Si $X$ sale bajo, vale la pena
  verificar `ollama show gemma4:e4b` para no confundir "modelo débil
  en esta suite" con "modelo sin soporte nativo de tools" (el mismo
  gotcha que descartó `gemma3:1b` del brazo A en
  `constrained-decoding-ab-design.md`).
- **No aísla el mecanismo de apertura proactiva**: este baseline mide
  capacidad bruta de `gemma4:e4b`, no si "abrir el turno y ceder" es
  mejor o peor que "resolver todo uno mismo" — son preguntas
  relacionadas pero distintas. El three-arm de
  `\S\ref{sec:mechanism}` (proactive vs. reactive) ya cubre la
  segunda; este documento solo cubre la primera.
- **Un solo punto, sin replicación**: a diferencia del baseline de 3B
  (replicado en 4 sweeps independientes, `\S\ref{sec:curve}`
  "Replication"), este es un sweep único. Si el resultado es
  sorprendente (muy alto o muy bajo), vale la pena una segunda corrida
  antes de comprometerse al framing revisado.

## Registro externo (OSF)

Primer uso del registro externo del proyecto — atiende el Issue 3 de
la review EMSE (el pre-registro auto-alojado en git es evidencia más
débil de lo que el paper implica). Texto listo para pegar en una OSF
Registration (Registries → "Preregistration" template, o el template
genérico si no hay uno específico de SE):

> **Title**: gemma4:e4b solo baseline — pre-registered framing
> criterion (braze harness-vs-scale paper)
> **Hypothesis**: [sección "La hipótesis" de este documento, verbatim]
> **Design**: [sección "Brazos y executors", verbatim]
> **Analysis plan**: [sección "Criterio pre-registrado", verbatim]
> **Date**: 2026-07-13, antes de correr el sweep.

**Acción pendiente del usuario**: no tengo credenciales/API de OSF en
este entorno — hay que crear la registration a mano en
osf.io/registries con el texto de arriba. **Decisión explícita
(2026-07-13)**: dado que este documento no es la adopción de una
palanca (es una medición descriptiva de un baseline faltante, sin
cláusula de iteración ni decisión irreversible), se relaja la
disciplina de "registrar antes de correr" por esta vez — el sweep
corre con el criterio ya committeado en git (mismo nivel de evidencia
que el resto del proyecto hasta ahora), y el registro OSF se completa
en paralelo o después. Si el registro OSF no se cierra antes de la
submission, el paper debe citar esto honestamente (mismo estándar de
disclosure que el resto de `\S\ref{sec:threats}`) en vez de implicar
que este criterio específico tuvo registro externo cuando no lo tuvo.
Una vez registrado, actualizar `Estado` a `CERRADO — registrado en
OSF` con el ID/link.

## Conexión con el paper

Si el resultado confirma el framing actual: entra a
`\S\ref{sec:curve}` como una oración adicional citando $X$ como
evidencia de que el compuesto no es solo "usar `gemma4:e4b`". Si
revisa el framing: entra como matiz explícito en abstract + intro +
contribuciones + `\S\ref{sec:curve}`, y se menciona en
`\S\ref{sec:threats}` como el threat que se cerró (reemplazando la
admisión actual de "we do not know"). En ambos casos, Fig.~1 gana una
línea de referencia horizontal o un marcador anotado para `gemma4:e4b`
solo. El ID de la registration OSF se cita en
`\section{Pre-registration texts}` (apéndice) junto a los otros
criterios pre-registrados del paper.

## Qué NO es este documento

No es una palanca a implementar — cero código nuevo, un solo comando
de `braze-bench` ya soportado. Costo estimado: minutos de cómputo en
Nitro (un solo arm, $n{=}95$, misma suite que ya corre en ~10-15 min
por arm según los sweeps previos) más el tiempo de crear la
registration en OSF. Es el ítem más barato del plan de resolución de
issues — por eso es el primero.
