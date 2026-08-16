# Nota de lectura: Small-Scale Experiments (arXiv 2608.11859, 12-ago-2026)

**Lourie, Cho, Ullrich, Lotfi — "Small-Scale Experiments: Are We There
Yet?"** — FAIR (Meta) + NYU. PDF en `docs/2608.11859v1.pdf`. Leída
2026-08-16. Clave sugerida para el bib del follow-up:
`lourie2026smallscale`.

## Qué es

Rehabilitación metodológica del experimento a pequeña escala en
pretraining. La tesis: las scaling laws SÍ existen hasta 4M de
parámetros; parecían no existir porque **los modelos chicos son
hipersensibles a los hiperparámetros**, y las regularidades solo
emergen sobre la "fully tuned frontier" — que exige búsquedas que casi
nadie corre. Mecanismo geométrico: al crecer el modelo, la dimensión
intrínseca de la superficie de pérdida de hiperparámetros BAJA (hasta
~1 dimensión efectiva) — los grandes son fáciles de tunear, los chicos
no. Proponen una metodología de tres herramientas: noisy quadratic
limit (¿estás en la frontera tuneada?), scaling laws como diagnóstico
(no como extrapolación ciega — "compound small errors"), y la
correspondencia perplexity–capability (válida solo a datos fijos).
Demo: recuperan pre-norm > post-norm desde escala chica.

## El mapeo con braze (el mismo fenómeno, una capa más arriba)

| Ellos (pretraining) | braze (harness/agéntico) |
|---|---|
| Modelos chicos hipersensibles a hiperparámetros | SLMs hipersensibles a palancas de contexto/harness (planner en prosa dañino, edit-fence, stencil — todo el catálogo del Paper 1) |
| La sensibilidad se desvanece con la escala (dim. intrínseca → 1) | Los grandes saturan las suites con o sin palancas; las palancas muerden solo abajo — la curva harness-vs-escala |
| Regularidades solo en la "tuned frontier" | qwen2.5:3b: 0/6 → 2/6 SOLO con el sampling recomendado por Qwen — la evidencia propia de que el operating point mueve el resultado |
| Noisy quadratic: ¿estás tuneado o midiendo ruido? | Piso de ruido + control A/A del mismo-prompt: ¿estás sobre el piso o midiendo la config? |
| Perplexity–capability se rompe al cambiar los datos | Benchmark-capacidad ≠ confiabilidad agéntica — la brecha que el proyecto mide hace un año |
| "Cada supuesto se vuelve un diagnóstico" | Pre-registro + gates + DBV — la misma actitud, formalizada distinto |

La convergencia es la misma que con AutoDesign/CompInt/LoopsBench:
piezas independientes apareciendo alrededor del mismo problema. Este
paper aporta lo que braze no tenía: **un mecanismo** (dimensión
intrínseca decreciente) para el patrón que braze solo describía
("palancas obvias nulas o dañinas abajo, irrelevantes arriba").

## La advertencia que SÍ nos toca (honestidad primero)

Su crítica central aplica contra nosotros mismos: braze compara
palancas de harness mayormente **en un operating point compartido por
default** (temp 0.2 del bench), no en la frontera tuneada de cada
modelo. Si los chicos son hipersensibles, algunos veredictos de
palanca podrían ser artefactos del punto de operación — el A/B que dio
nulo a temp 0.2 podría no ser nulo con el sampling recomendado de la
familia. Mitigantes que ya tenemos: diseños pareados (el operating
point afecta ambos brazos), pisos de ruido, y el precedente qwen
documentado. Lo que NO tenemos: un chequeo de sensibilidad *por
palanca*. Idea concreta para el backlog (NO pre-registrada aquí):
re-correr UN A/B de palanca ya cerrado (p.ej. task-list sobre
qwen2.5:3b) en el sampling recomendado de la familia vs el default del
bench — si el veredicto flipea, los threats del Paper 1/follow-up
ganan un párrafo medido; si no flipea, el diseño pareado queda
defendido con datos.

## Dónde braze es metodológicamente distinto (no comparable en fuerza)

- Ellos miden **loss continua** con cientos de modelos entrenados;
  braze mide **decisiones discretas** (pass/fail con oráculo) con
  gates pre-registrados. Herramientas distintas para variables
  distintas — el noisy quadratic no aplica directo a pass rates; el
  análogo braze es el e-process/McNemar+piso.
- Su alcance es explícitamente **model-centric** (declaran que
  data-centric rompe el marco). El territorio de braze — el harness
  como variable — ni siquiera está en su mapa: braze opera la capa que
  ellos dan por fija.

## Escepticismo justo

- Sus resultados son de **pretraining loss en ladders 4M-escala**; la
  transferencia a nuestro régimen (3-20B post-entrenados, tool-tuned,
  tareas agénticas) es analógica, no directa. Los "hiperparámetros" de
  nuestro régimen (sampling + config del harness) son otros objetos.
- "Loss compra toda capability a datos fijos" es elegante pero
  frágil fuera de pretraining — en post-training/agentes la
  correspondencia está lejos de demostrada (ellos mismos lo acotan).

## Acciones

1. **Cola del bib del follow-up**: `lourie2026smallscale` junto a
   luo2026autodesign/li2026loopsbench/wang2026compint — el párrafo de
   Related Work "experimentación rigurosa en régimen sensible" es su
   casa natural; pasa por `/verify-refs` con las demás.
2. **Backlog (idea, sin pre-registro aún)**: chequeo de sensibilidad
   de veredicto por operating point (el re-run de arriba). Barato: 1
   palanca × 1 modelo × 2 sampling points.
3. **Paper 1, si llega revisión**: la dimensión intrínseca decreciente
   es el mecanismo citable para "por qué el harness importa más en
   modelos chicos" — refuerza la tesis con literatura de otra capa.
4. **Guardia epistémica**: cuando un A/B nuestro dé nulo en un modelo
   chico, el reflejo nuevo es preguntar "¿nulo en la frontera tuneada,
   o nulo en el default?" — anotarlo en la plantilla mental de
   pre-registros futuros (el de KV-quant de hoy ya fija temp 0.2 +
   cláusula L-9; su lectura deberá recordar esta cota).

## Contexto de la lectura

Llega el mismo mes que AutoDesign (meta-harness), LoopsBench (loop
engineering) y CompInt (compaction loss), y el mismo día que el
pre-registro del A/B KV-quant. Cuatro fuentes independientes
convergiendo en el mismo encuadre: los regímenes chicos son sensibles,
la medición honesta ahí exige maquinaria que la mayoría no corre, y
esa maquinaria — no el modelo — es la ventaja acumulable del proyecto.
