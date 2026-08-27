# Nota de lectura: Harness-Bench (2605.27922) y Belief Divergence (2607.04528)

Fecha: 2026-08-25. Los dos trabajos del campo que **más se solapan con
el Paper 1**, ambos anteriores a su freeze (29-jul) y ninguno citado en
él. Leídos por eso.

## Harness-Bench — Yao et al., 13 autores, 27-may-2026

*"Measuring Harness Effects across Models in Realistic Agent
Workflows"*. Clave: `yao2026harnessbench`.

**Qué hace**: 106 tareas sandboxed, **5.194 trayectorias de ejecución**
sobre múltiples pares modelo×harness; captura artefactos finales,
trazas, uso y salidas de validador. Mide "efectos de harness a nivel de
configuración": contexto, tools, estado, restricciones, permisos,
tracing, recuperación.

**Su conclusión central es, palabra por palabra, la tesis de nuestra
nota de agencia**: la capacidad de un agente *"debería reportarse a
nivel de configuración modelo-harness, en vez de atribuirse al modelo
base"*. Publicado en mayo; nuestra nota es del 19-ago. **Nos ganaron el
framing por tres meses.** Hay que decirlo así, sin adornos.

**Lo que NO hacen** (verificado en el abstract, a confirmar con el
texto completo): ni tests estadísticos, ni intervalos, ni piso de
ruido. Con 5.194 trayectorias y cero inferencia, es el ejemplo más
grande del gap que el Paper 3 mide.

## Belief Divergence — Yi & Song, 2 autores, 5-jul-2026

*"Measuring Harness-Induced Belief Divergence in Multi-Step LLM
Agents"*. Clave: `yi2026belief`.

**Qué hace**: define la divergencia de creencias inducida por el
harness —cambios en las creencias multi-paso del agente causados por
*qué ve, qué acciones puede tomar, qué fallos se le reparan*, no por la
tarea ni el modelo—. Diagnóstico de rollout de creencias en 9
dimensiones (progreso, riesgo, recuperabilidad, restricciones, modo de
fallo, incertidumbre, éxito futuro, costo de reparación, próxima
acción), descompuesto en un término de "llegada" (el cambio inmediato
de interfaz) y otro de "crecimiento" (dependiente del horizonte).

**Su frase**: *"harness design is an experimental variable in agent
evaluation, not an implementation detail"* — la tesis del Paper 1,
enunciada por terceros en julio.

**Su hallazgo más interesante para nosotros**: el **éxito terminal
puede preservarse mientras las creencias que guían las decisiones
siguientes cambian sustancialmente**.

## La conexión que sí es nuestra: RouteMiss ≈ belief divergence operacional

Ese hallazgo suyo es, en otro vocabulario, lo que la **métrica dual**
del proyecto mide desde el 12-ago: `passed` (logro funcional) puede
mantenerse mientras `passed_strict` (adherencia de ruta) cae — el
RouteMiss. Ellos lo detectan con un diagnóstico de 9 dimensiones que
requiere elicitar creencias; nosotros con **dos booleanos y un oráculo
`cargo check`**, sin preguntarle nada al modelo. Es la misma clase de
fenómeno con instrumentos de costo muy distinto, y esa comparación es
material propio: *un proxy barato y objetivo de la divergencia inducida
por el harness*.

## Riesgo real para el Paper 1 (en revisión desde julio)

Ambos son anteriores al freeze y comparten framing. Un revisor que los
conozca puede leer su ausencia como desconocimiento del campo. **No se
puede tocar el manuscrito ahora**, pero la diferenciación existe y hay
que tenerla escrita para una eventual revisión:

| | Harness-Bench / Belief-Div | Paper 1 |
|---|---|---|
| Tipo de evidencia | **descriptiva**: variación entre pairings / entre configuraciones | **causal por palanca**: A/B controlados, una palanca a la vez |
| Decisión | ninguna: reportan variación | **criterios de adopción pre-registrados** por palanca |
| Inferencia | no reportada | McNemar+Holm, pass^k, piso de ruido, equivalencia |
| Régimen | workflows realistas (frontier, aparentemente) | **SLM local 3-20B**, donde las palancas cambian de signo |
| Escala vs profundidad | 5.194 trayectorias, muchos pairings | menos corridas, cada una con pre-registro y diagnóstico de mecanismo |

En una frase para el rebuttal: *ellos muestran que el harness importa;
nosotros medimos **cuál** palanca, **cuánto**, **con qué signo según la
escala del modelo**, y bajo qué criterio se adopta.* Eso sigue siendo
nuestro.

## Acciones

1. Bib del follow-up: `yao2026harnessbench`, `yi2026belief`.
2. **Preparar el párrafo de Related Work** para una eventual revisión
   del Paper 1 (guardarlo listo; no se toca el manuscrito congelado).
3. **Paper 3 se refuerza**: Harness-Bench es el caso de mayor escala
   sin inferencia estadística del campo.
4. **Ángulo propio nuevo, anotado**: la métrica dual como proxy barato
   y objetivo de la divergencia inducida por el harness — se puede
   contrastar contra su diagnóstico de 9 dimensiones. Candidato a
   sección del follow-up, no a paper propio todavía.
5. Confirmar contra el texto completo que efectivamente no hacen
   inferencia (el abstract no la menciona, pero el claim del Paper 3
   no puede descansar en un abstract).

---

## Confirmación contra TEXTO COMPLETO (2026-08-25, tarde)

PDFs descargados (`docs/2605.27922v1.pdf`, `docs/2607.04528v1.pdf`) y
analizados. La conclusión del abstract se sostiene pero **con un matiz
que corrige la versión anterior de esta nota**.

### Harness-Bench (Peking University + Qiyuan Tech) — confirmado, y mejor de lo esperado para el Paper 3

En 6.804 palabras: **cero** p-values, tests, intervalos, repeticiones,
seeds o error bars. Pero usan "variance" 13 veces, y al definirla
escriben:

> *"we compute its average score under each configurable harness across
> all tasks and report the variance of these harness-level averages.
> This variance reflects cross-harness variation over the fixed task
> suite, **not repeated-run stochastic variance**."*

**Reconocen explícitamente la varianza estocástica entre corridas y
declaran que su métrica no la captura.** Para el Paper 3 esto es más
fuerte que un vacío: no hay que argumentar que el problema existe —
el propio trabajo de mayor escala del campo (5.194 trayectorias) lo
nombra y sigue sin medirlo. Es una cita, no una inferencia nuestra.

### Belief Divergence — CORRECCIÓN: más cuidadosos de lo que dije

Su protocolo es riguroso en el **diseño**: grilla de seis harnesses ×
ocho tareas × cuatro horizontes K∈{1,3,5,8} × **tres semillas
aleatorias**; LLM base, plantilla de elicitación, decodificación y
esquema de creencias fijos; **semillas apareadas** entre
configuraciones; 21 celdas pareadas por harness base; implementación
con 77/77 tests unitarios.

Lo que NO hacen es **inferencia**: ni un p-value, ni un intervalo, ni
desviación estándar reportada. Comparan magnitudes.

**El hueco preciso, entonces, no es "ignoran el ruido"** — a este
equipo claramente le importa — **sino que controlan la varianza en el
DISEÑO y no la propagan a la DECISIÓN**. Esa formulación es más justa
y más difícil de refutar, y es la que debe ir al Paper 3. La versión
anterior de esta nota los metía en el mismo saco; era injusto.

### Consecuencia para el claim del Paper 3

Queda una escala de tres niveles, que es mejor material que un
veredicto binario:

1. **Sin control ni inferencia**: Meta-Harness, AutoDesign (selección
   por estimación puntual).
2. **Control de diseño sin inferencia**: Belief Divergence (semillas
   apareadas, condiciones fijas) y Harness-Bench (que además declara
   la limitación).
3. **Control + inferencia + criterio pre-registrado**: lo que este
   proyecto aporta.

Y HarnessOpt-Bench (held-out inaccesible) ocupa un cuarto lugar:
protege contra sobreajuste de búsqueda, no contra ruido de medición.

---

## El párrafo de Related Work, redactado (2026-08-27)

Cierra la acción 2 de esta nota y la 3 de
`nota-campo-harness-2026-08-25.md`. **No se toca el manuscrito
congelado**: esto se guarda para la eventual revisión del Paper 1, o
para la respuesta a un revisor que pregunte por estos trabajos.

Escrito en el registro del § Related Work existente (`paper/main.tex`,
l. 288): `\paragraph{}` temático, cita con números concretos, y cierre
que dice qué hace braze distinto. Va después de
`\paragraph{Agent-computer interfaces.}`, que es donde encaja
temáticamente.

```latex
\paragraph{Concurrent work on harness effects.} Two contemporaneous
studies measure harness effects directly and reach the framing of
this paper independently. Harness-Bench \citep{yao2026harnessbench}
runs 5{,}194 execution trajectories over 106 sandboxed tasks across
model$\times$harness pairings and argues that agent capability
``should be reported at the model-harness configuration level rather
than attributed to the base model.'' Belief-divergence work
\citep{yi2026belief} states the same premise as ours in as many
words---``harness design is an experimental variable in agent
evaluation, not an implementation detail''---and operationalizes it
as a nine-dimension belief rollout over a grid of six harnesses,
eight tasks, four horizons and three paired seeds; its central
finding, that terminal success can be preserved while the beliefs
guiding subsequent decisions shift substantially, is the phenomenon
our dual functional/strict metric detects with two booleans and a
compiler oracle instead of belief elicitation. Both are descriptive
where this paper is causal: they characterize how much outcomes vary
across configurations, whereas we vary one lever at a time against a
pre-registered adoption criterion, report paired exact tests with
multiplicity correction against a measured in-sweep noise floor, and
work in the local small-model regime where several of these levers
change sign rather than magnitude. Notably, Harness-Bench defines its
variance as cross-harness variation over a fixed task suite and
states that this is ``not repeated-run stochastic variance''---the
distinction our noise floor is built to measure.
```

**Notas de uso.**

1. Las dos claves de bib (`yao2026harnessbench`, `yi2026belief`) hay
   que crearlas y pasarlas por `/verify-refs` antes de usar el
   párrafo. No están en `refs.bib` del Paper 1.
2. La última oración es la que más trabaja: convierte la cita de un
   competidor en apoyo del Paper 3 sin sacar los pies del Paper 1.
3. **Postura si un revisor pregunta por qué no estaban citados**: son
   de mayo y julio de 2026, el manuscrito se congeló el 29-jul, y la
   respuesta honesta es que se incorporan ahora. No inventar que se
   conocían.
4. Si el manuscrito exige versión anónima, el self-cite del Paper 1
   pasa a tercera persona pero este párrafo no se toca (no se
   auto-cita).
