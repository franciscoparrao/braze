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
