# Nota de lectura: *Stop Comparing LLM Agents Without Disclosing the Harness* (2605.23950)

Fecha: 2026-08-29
Zhang, Wang, Ge, Xu, Hamm, Reddy (Tulane / Rutgers / Virginia Tech),
7-may-2026. Paper de posición, 17 págs. PDF en `docs/`.

## Primero, una corrección a la nota de campo del 25-ago

Esa nota lo describía como *"posición + AutoHarness (síntesis
automática de harness)"*. **AutoHarness no es de ellos**: lo citan junto
con ADAS, Meta-Harness y AHE como trabajos de terceros que optimizan el
harness (refs [11], [15], [17], [19]). Meta-Harness —el sistema cuya
regla de aceptación el experimento central ya simula— es de Lee et al.,
arXiv:2603.28052.

Consecuencia práctica: **no hay un cuarto sistema nuevo cuyo gate deba
entrar al experimento central.** La tabla de cuatro filas sigue
completa.

## La tesis

**Binding Constraint Thesis.** Con `B(M,H)` el score del modelo `M` bajo
el harness `H`, definen

```
HV(M) = Var_{H~P(H)}[B(M,H)]        varianza inducida por el harness
MV(H) = Var_{M~P(M)}[B(M,H)]        varianza inducida por el modelo
```

y sostienen que, en tareas long-horizon con modelos frontier
comparables, **HV es comparable o mayor que MV** — de modo que los
protocolos actuales, que reportan `B(M, H*)` para un `H*` único y no
declarado, hacen HV inmedible y las comparaciones entre modelos
incompletas.

Encuadre bonito y con consecuencias: el agente es un sistema de control
en lazo cerrado, el harness es el **controlador** y el LLM la política
**en lazo abierto**. De ahí tres cantidades estructurales —estabilidad
(medida tipo Lyapunov), *context drift* (KL entre la distribución del
contexto y la inicial) y *control lag* (pasos entre detectar una anomalía
y que la corrección llegue a la política)— que son propiedades del
harness y no del modelo.

## Su factorial

Tres modelos (GPT-5.4, Kimi K2.6, GLM-5.1) × tres harnesses, sobre 100
tareas de SWE-bench Verified, **dos corridas independientes por celda**.

| | H₁ mínimo | H₂ mejorado | H₃ completo | HV(M) |
|---|---|---|---|---|
| GLM-5.1 | 52,5 | 56,5 | 65,5 | **29,56** |
| GPT-5.4 | 55,0 | 58,5 | 63,5 | 12,17 |
| Kimi K2.6 | 52,0 | 59,0 | 60,5 | 13,72 |
| **MV(H)** | 1,72 | 1,17 | 4,22 | |

Ratio agregado **HV/MV = 7,80×**, y **6 reversiones de ranking** en 9
comparaciones par-modelo × par-harness. Cambiar el harness mueve 8,5–13
puntos; cambiar el modelo, 3,0–5,0.

## El aporte para el Paper 3, y es técnico

Su ecuación 1 dice que la varianza total **"decomposes exactly as"**:

```
Var(B(M,H)) = MV + HV + Var(model × harness)
```

Eso es exacto para la varianza de la **media** `μ(M,H)`, no para la del
score **observado**. Si `B = μ(M,H) + ε` con `ε` el ruido de corrida,
falta un cuarto término:

```
Var(B) = MV + HV + interacción + σ²_ε
```

Y lo notable es que **su propio diseño lo estima**: con dos corridas por
celda hay grados de libertad para el residual, y de hecho lo usan — su
ecuación 2 define `η²_p = SS_interaction / (SS_interaction + SS_error)`,
donde `SS_error` **es** ese término.

Pero:

1. La Tabla 2 reporta *"mean pass@1 over two runs"* y **no reporta su
   dispersión**. Las dos corridas se promedian y desaparecen.
2. `SS_error` aparece solo como denominador de un estadístico de tamaño
   de efecto, nunca como la **vara contra la que se lee un delta**.
3. La descomposición que proponen como protocolo para la comunidad
   —"reportar HV por modelo, MV por harness, el ratio, las reversiones y
   η²_p"— **no incluye el residual entre las cantidades a reportar**.

O sea: el mismo patrón que Belief Divergence (controlan en el diseño, no
propagan a la decisión) y que Recuris y Maka (miden el piso, no lo
convierten en umbral). **Tres corridas del mismo patrón en cuatro
trabajos distintos** empieza a ser un hallazgo sobre el campo, no una
crítica a un paper.

**Y da al Paper 3 su formulación más limpia hasta ahora**: adoptar su
descomposición y añadirle el término que falta. El aporte deja de ser
"nadie mide el ruido" —falso— y pasa a ser *"el término residual existe,
varios diseños lo estiman sin querer, y ninguno lo reporta ni lo usa
para calibrar un umbral; acá está medido y acá está lo que cuesta
ignorarlo"*. Eso es constructivo, encaja en un marco que otros ya
publicaron, y no requiere caracterizar mal a nadie.

## Otras dos cosas aprovechables

**La Harness Card.** Proponen una ficha de disclosure estructurada en
siete capas (taxonomía ETCSOVG: Execution, Tool, Context, Scheduling,
Observability, Verification, Governance) para que un lector que compara
dos scores pueda localizar si la diferencia es del modelo, del harness o
de su interacción. Es el análogo externo de lo que `RunMetadata` hace
acá — y su capa *Execution* (sustrato de runtime, presupuestos) es
exactamente lo que `engine_version` empezó a registrar.

**Su Tabla 1** es un catálogo de cambios inducidos por harness con el
modelo fijo, de fuentes públicas: +9,5 pp (SEAL→Claude Code), +14–16 pp
(SWE-agent→scaffold de xAI), +13,7 pp en TerminalBench 2.0, 76,4 % con
optimización automática. Material citable para el Paper 1 tanto como
para el 3.

## Lo que declaran y conviene respetar al citarlos

- **Falsabilidad explícita**: la tesis se falsa con un factorial que
  encuentre `MV > HV` en tareas long-horizon a capacidad comparable.
- **No reclaman universalidad**: *"We do not claim that the 7.80× ratio
  is universal."*
- **Caveats de η²_p**: sesgo positivo en grillas chicas, recomiendan
  reportarlo junto a ω² o un bootstrap, y advierten que no es necesario
  ni suficiente para las reversiones de ranking.
- **Alcance restringido**: modelos frontier comparables y tareas
  long-horizon. Explícitamente fuera: horizonte corto y configuraciones
  donde un modelo domina.

Ese último punto importa para nosotros: **su régimen no es el nuestro**.
Ellos exigen capacidad comparable entre modelos frontier; el proyecto
trabaja con SLM locales de 3-20B donde las brechas de capacidad son
grandes. Su tesis no se aplica directamente acá, y afirmar lo contrario
sería el tipo de extrapolación que ellos mismos excluyen.

## Acciones

1. Bib: `zhang2026stopcomparing`, con procedencia de `pdfinfo` +
   `/verify-refs`, como las otras ocho.
2. Al outline v2 del Paper 3: la formulación del término faltante como
   el encuadre del aporte.
3. Corregir la nota de campo del 25-ago (AutoHarness no es de ellos).
