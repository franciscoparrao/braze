# Nota de lectura: Harness Continual Learning (arXiv 2608.19013, 19-ago-2026)

**Kang, Gu, Lv, Li, Wang, Gao — "Harness Continual Learning: Continual
Adaptation Beyond Model Parameters"** — Nanjing University (State Key
Lab for Novel Software Technology) + Wollongong. PDF en
`docs/2608.19013v1.pdf`. Leída 2026-08-21. Clave: `kang2026hcl`.
**Cuarto** del arco meta-harness en cinco meses: Meta-Harness (mar) →
LoopsBench (jul) → AutoDesign (ago-13) → HCL (ago-19).

## Qué aporta que los otros no

Formaliza el harness como **estado de aprendizaje continuo** alrededor
de un modelo congelado, y nombra el problema que eso crea:
**harness-level forgetting** — actualizar el harness puede romper
comportamiento que antes era confiable, aunque los pesos no cambien.
Cuatro componentes (Task Interface, Experience Memory, Capability Map,
Adaptive Router) y **guarded harness evolution**: separar la
generación del update (Continual Optimizer) del *commit* del estado
(Continual Evaluator). Ganancias >10% relativas; "controlled retention
sweeps" que hacen medible el olvido y el trade-off
estabilidad-plasticidad.

Su Continual Evaluator exige **tres** condiciones antes de commitear:

1. **Current improvement**: `Δn = P(H̃n+1, Vn) − P(Hn, Vn) ≥ δn`.
2. **Historical retention**: un *anchor set* `An` de casos previos que
   no deben degradarse — con "presupuesto de retención" ajustable.
3. **Validity**: harness y salidas siguen usables.

Todo "under the same model, decoding, tool, environment, and **seed**
conditions".

## Dónde nos toca directo

**El anchor set es nuestra suite de no-regresión, formalizada.** Y su
"harness-level forgetting" nombra algo que braze ha tratado sin
bautizar: el DBV (`--baseline-ref`), la suite de no-regresión de los
pre-registros, la exigencia de que una palanca nueva no rompa lo
anterior. Ellos lo convierten en condición de commit y en objeto de
medición. **Es el primero de los cuatro que se preocupa por el gate**,
y eso hay que reconocérselo.

## El gap que sigue abierto (cuarto datapoint del ángulo)

La decisión de commit es **un umbral sobre una diferencia de
estimaciones puntuales, con seed fijo y sin repeticiones**: ni
intervalos, ni tests, ni piso de ruido medido. "Same seed conditions"
da una sensación de control que nuestro propio banco desmiente:

- Ollama no es bit-exacto con seed fijo (documentado desde julio).
- El piso de la suite discriminante es **~20% de celdas volteando con
  prompts byte-idénticos** (pm-ab, 2026-08).
- El pm-ab produjo **+13 tareas con McNemar p=0.011 Holm-corregido**
  que el control del mismo prompt disolvió a +4 (p=0.541).
- Y el 20-ago aprendimos que la **deriva temporal del nodo** se
  confunde con el tratamiento si los brazos no se intercalan.

Con un `δn` de mejora mínima y una sola corrida, cualquiera de esos
cuatro efectos cruza el umbral. Su gate acepta ruido — y como el
estado se **commitea**, el ruido aceptado queda incorporado al harness
desplegado y contamina todas las evaluaciones posteriores (el anchor
set se vuelve anchor de una decisión ruidosa). En un loop continuo,
eso compone.

## El ángulo, ahora afinado por cuatro sistemas

Los cuatro proponen optimizar el harness; **ninguno mide bajo ruido**:
Meta-Harness y AutoDesign aceptan por estimación puntual; HCL agrega
retención histórica pero con el mismo tipo de decisión; LoopsBench
mueve el foco al loop sin tocar el problema. La contribución que braze
puede hacer ya no es "hace falta estadística" en abstracto, sino algo
más preciso:

> **Un gate de commit para harness continuo en régimen ruidoso**:
> pre-registro del criterio, contraste pareado exacto con corrección
> de multiplicidad, **piso de ruido in-sweep medido con un brazo de
> configuración idéntica**, MDE declarado, **orden intercalado** para
> desconfundir deriva, y corte secuencial anytime-valid para no pagar
> n fijo en cada iteración del loop. Todo eso existe ya en el
> proyecto (`sequential.rs`, McNemar+Holm, control mismo-prompt,
> métrica dual, DBV).

Y HCL aporta el término que le faltaba al encuadre: lo que el gate
protege no es solo la métrica actual, es la **retención**.

## Acciones

1. `kang2026hcl` a la cola del bib del follow-up (con
   lee2026metaharness, luo2026autodesign, li2026loopsbench,
   wang2026compint, lourie2026smallscale). `/verify-refs` en su
   momento.
2. **Idea propia que sale de aquí**: nuestro DBV + suite de
   no-regresión podrían formalizarse como *anchor set con presupuesto
   de retención* — cuando se adopta una palanca, medir explícitamente
   el olvido inducido, no solo el pass rate nuevo. Barato y
   alimentaría el follow-up con instrumento propio.
3. El párrafo de Related Work del follow-up queda con arco de cuatro
   sistemas y un gap común, que es la mejor forma de posicionar la
   contribución.
