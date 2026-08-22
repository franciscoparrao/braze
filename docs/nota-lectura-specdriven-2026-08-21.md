# Nota de lectura: Spec-Driven Test Generation (arXiv 2608.17177, 17-ago-2026)

**Tufano, McClure, Cambronero, Cheng, Shi, Wei, Chen, Ivančić,
Dalloro, Rondon — "Grounding AI Agents in Contracts: An Empirical
Evaluation of Spec-Driven Test Generation"** — Google. PDF en
`docs/2608.17177v1.pdf`. Leída 2026-08-21. Clave: `tufano2026specdriven`.
No pertenece al arco meta-harness: es **una palanca de harness
concreta, evaluada con estadística seria**.

## Qué mide

Instruir al agente a razonar y documentar explícitamente
**pre-condiciones, post-condiciones y comportamiento indefinido**
antes de generar tests — una spec semi-formal que actúa de "cognitive
scaffold". Contra un agente de generación directa, sobre **90 bugs de
producción reales de Google** (fixes verificados, fallos
reproducibles): **+9,8 pp en detección de bugs (p=0,0352)** y **+2,5
pp en cobertura de ramas (p=0,0034)**; con LLM-as-a-Judge, superior al
baseline en 77,8% y a los tests escritos por humanos en 56,7%.

## Por qué es el mejor diseñado del lote reciente

- **Ambos brazos comparten arquitectura agéntica, toolset y modelo**
  (Gemini 3 Flash, temp 1.0 default del proveedor): el único cambio es
  la palanca. Control limpio, del tipo que este proyecto exige.
- **Bootstrapping a nivel de bug** (10.000 resamples) y **McNemar
  para la detección binaria** — el mismo test que usamos nosotros. Es
  el primero de los papers leídos este mes que hace inferencia de
  verdad, y hay que reconocérselo: cultura de SE empírico, no de
  leaderboard.

## El contraste con nuestros datos (lo valioso)

Braze midió que el **planner en prosa libre DAÑA** a ejecutores chicos
(−22 pp en la matriz de 4 brazos), y a la vez que **la task list
TIPADA rescata al 3B** donde la prosa degenera. Este paper mide que
una **spec semi-formal estructurada AYUDA** (+9,8 pp) en Gemini 3
Flash — su modelo rápido, no un frontier.

Los tres resultados son coherentes bajo una sola lectura, y es la que
el proyecto ya sostenía: **el eje que decide no es "razonamiento
previo sí/no" sino "estructurado vs prosa libre"**. Contratos,
listas tipadas y specs con campos definidos ayudan; el texto libre
que el modelo debe re-interpretar cada ronda daña, y daña más cuanto
más chico es el ejecutor. Evidencia externa, en otro dominio
(generación de tests), con otro proveedor y otro tamaño de modelo,
apuntando al mismo eje. Es de las mejores confirmaciones indirectas
que hemos encontrado.

## Escepticismo justo

- **LLM-as-a-Judge con acoplamiento de familia**: Gemini 3.1 Pro
  juzgando salidas de Gemini 3 Flash. Lo rescata que los endpoints
  duros (detect@k, cobertura) son objetivos y van primero; el judge es
  secundario.
- **Multiplicidad sin corregir declarada**: reportan al menos dos
  endpoints más el judge. Con p=0,0352 en el endpoint principal, una
  corrección de Holm por 2-3 comparaciones lo dejaría al filo. No
  invalida el resultado — el segundo endpoint está a p=0,0034 — pero
  es exactamente el detalle que nuestro pipeline cuida (y que el
  Paper 2 tuvo que corregirse a sí mismo).
- n=90 bugs de un solo ecosistema (Google, con su estilo de código y
  su CI); generalización a otros repos, declarada como pendiente.

## Acciones

1. `tufano2026specdriven` a la cola del bib del follow-up — encaja en
   el párrafo de la curva harness-vs-escala como evidencia externa del
   eje estructura-vs-prosa, y como ejemplo de palanca evaluada con
   inferencia (contraste con los cuatro del arco meta-harness, que no
   la hacen).
2. Idea de backlog (NO pre-registrada): la clase "spec/contrato antes
   de actuar" es una palanca que braze nunca probó en su forma
   estructurada — el análogo local sería un paso de pre-condiciones
   tipadas antes de `edit_file` en tareas de reparación. Barata de
   implementar sobre la task list existente; su prior, dado este
   paper y nuestro hallazgo tipado-vs-prosa, es positivo — lo que la
   hace justamente una candidata a medir, no a asumir.
