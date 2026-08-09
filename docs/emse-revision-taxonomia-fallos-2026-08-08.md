# Nota para la revisión de EMSE: `FailureCause` contra la taxonomía de aristas

Fecha: 2026-08-08
Estado: **material de revisión, NO del manuscrito enviado**

El manuscrito está congelado (`paper/submission-emse/`, tag
`emse-submission-2026-07-29`, EMSE-S-26-01210, esperando primera
decisión). **Nada de este documento toca ese paquete.** Esto es lo que se
propone incorporar *si y cuando* llegue una decisión que permita revisar.

## 1. Por qué hay que abrir este tema en la revisión

Raj et al. (Scale AI, arXiv:2607.28802, 30-jul-2026) abren su abstract
así: las evaluaciones existentes reducen los fallos de agentes a
resultados de nivel de sistema, lo que oscurece dónde se originó el fallo
y qué intervención mejoraría de verdad la siguiente iteración.

Esa crítica **aplica a `braze-bench`**. Su enum `FailureCause` es casi
todo *dónde paró el loop*, no *quién falló*:

| `FailureCause` | Qué dice | Qué NO dice |
|---|---|---|
| `Timeout` | se acabó el reloj de infra | por qué no convergía |
| `WallClockExhausted` | se acabó el presupuesto del turno | ídem |
| `MaxIterationsExhausted` | se acabaron las rondas | ídem |
| `TurnBudgetExhausted` | se acabaron los tokens | ídem |
| `Assertion{ToolCall,Text,Files,CargoCheck}` | el oráculo dijo que no | qué componente lo causó |
| `IncompleteStream` | el stream murió sin evento terminal | proveedor, red o parser |
| `ModelBackendError` | el backend erró | transporte vs política vs parseo |

Cinco de las quince categorías son **presupuestos agotados**. Un
presupuesto agotado no es una causa: es el síntoma de que algo no
convergía. Reportarlo como categoría de fallo es exactamente el colapso
que la taxonomía describe.

Es una limitación honesta del instrumento y conviene declararla nosotros
antes de que la declare un reviewer — sobre todo si el reviewer viene del
grupo de Hassan, que publicó `hasan2026testing` (ya citado) y
`sghaier2026blame` sobre el mismo problema de atribución.

## 2. El mapeo

Notación de Raj et al.: `COMP1 — COMP2 · fault: SIDE`.

| `FailureCause` de braze | Arista/lado más probable | Comentario |
|---|---|---|
| `AssertionToolCall` (llamó la tool equivocada) | `MODEL — TOOL · fault: MODEL` | *Incorrect Tool Selection* |
| `AssertionToolCall` (no llamó ninguna) | `MODEL — OWNER · fault: MODEL` | *Under-initiative* |
| schema fail → reparación → fail | `MODEL — TOOL · fault: MODEL` | *Malformed Arguments* |
| `tool_execution_failures` sin recuperación | `MODEL — TOOL · fault: MODEL` | *Tool Recovery Failure* |
| `AssertionCargoCheck` con el error devuelto al modelo | `MODEL — TOOL · fault: MODEL` | *Tool Feedback Neglect* |
| `AssertionFiles` tras compactación | `MODEL — CONTEXT · fault: HARNESS` | *Context Rationale Erosion* — **el harness es culpable cuando la compactación es harness-driven** |
| `EmptyModelResponse` por canal no mapeado | `MODEL — TOOL · fault: TOOL` | *Mistranslation* |
| bug del parser harmony server-side de Ollama (incidente #1) | `MODEL — TOOL · fault: TOOL` | *Mistranslation*, y el `LocalBackend` la elimina **por construcción** |
| `ModelBackendError` (5xx del proveedor) | `MODEL — EXTERNAL ENV · fault: ENV` | *Service Failure* |
| `CircuitOpen` → `HarnessError` | `MODEL — EXTERNAL ENV · fault: ENV` | ya excluido del denominador — coincide con la taxonomía |
| `Timeout` / `WallClockExhausted` / `MaxIterationsExhausted` / `TurnBudgetExhausted` | **no mapeable** | son cortes, no fallos. La causa está río arriba y el bench no la registra |
| `AssertionText` | **no mapeable sin la traza** | puede ser cualquier cosa |

Lo que el mapeo revela: **cuatro de nuestras categorías no son
atribuibles en absoluto**, y en el piloto de round-economics esas cuatro
se llevaron 74 de las filas fallidas. O sea que la fracción no atribuible
no es marginal.

## 3. Dos hallazgos del proyecto que la taxonomía clasifica bien

**El bug del parser de Ollama** es *Mistranslation* de manual: la capa de
integración corrompe una acción por lo demás correcta. Poder decir que el
`LocalBackend` elimina una clase entera de fallo **por construcción**, y
nombrar esa clase con vocabulario de un tercero, es más fuerte que
decirlo con vocabulario propio.

**El hallazgo `U+1D62`** (el modelo no puede emitir un carácter, y con el
carácter nombrado y exhibido delante volvió a comérselo) cae del lado del
modelo bajo su regla: ningún harness crea capacidad ausente. Pero acá hay
**una tensión que la taxonomía no maneja y que es nuestra contribución**:

> La taxonomía manda la reparación de un fallo model-side a post-training.
> Nosotros medimos una reparación **de harness** sobre un fallo model-side
> que no toca la capacidad y aun así cambia el resultado: deadlock ciego
> de 20 rondas / 25 min con daño silencioso → rechazo honesto en 4 rondas
> / 7 min 31 s, verificado en vivo.

El harness no puede **crear** capacidad, y sí puede volver su ausencia
**legible y barata**. Esa distinción no está en su esquema: la
invisibilidad de un fallo es propiedad del harness, no del modelo. Es un
párrafo de Discussion, no una nota al pie.

## 4. Qué proponer en la revisión

1. **Threats to Validity**: declarar que `FailureCause` es taxonomía de
   corte y no de atribución, con la fracción no atribuible medida.
2. **Related Work**: agregar Raj et al. (taxonomía), Kapoor et al. (HAL,
   ICLR 2026 — el ancla peer-reviewed) y Ben Sghaier et al. (el vecino
   metodológico: fija el modelo, varía el harness).
3. **Discussion**: el argumento de § 3 — el harness no crea capacidad,
   la vuelve legible; y la invisibilidad es propiedad del harness.
4. **Future work** (no para esta revisión): un segundo eje en
   `FailureCause` que registre arista+lado. Es barato de agregar y
   caro de reconstruir después.

## 5. Referencias verificadas

Verificadas el 2026-08-08 contra fuentes primarias. **El nivel 1 de
`/verify-refs` dio NOT_FOUND en las cuatro y las cuatro existen**:
OpenAlex/CrossRef no indexan por título los preprints de arXiv recientes
ni las actas de workshop. Para esta literatura hay que ir a la API de
arXiv, OpenReview y CrossRef por DOI exacto.

```bibtex
@misc{raj2026taxonomy,
  title        = {Model or Harness? An Interaction-Centric Taxonomy for
                  Localizing Agent Failures},
  author       = {Raj, Harsh and Gupta, Vipul and Mahmoud, Anas and
                  Dumitru, Razvan-Gabriel and Yi, Darvin and
                  Sabharwal, Aakash and He, Yunzhong},
  year         = {2026},
  eprint       = {2607.28802},
  archivePrefix= {arXiv},
  primaryClass = {cs.AI},
  note         = {Preprint. Verificado 2026-08-08: sin venue peer-reviewed}
}

% ICLR 2026 Poster CONFIRMADO en OpenReview (submission 9583,
% camera-ready). El primer autor es Kapoor, no Stroebl.
@inproceedings{kapoor2026hal,
  title     = {Holistic Agent Leaderboard: The Missing Infrastructure
               for {AI} Agent Evaluation},
  author    = {Kapoor, Sayash and Stroebl, Benedikt and Kirgis, Peter and
               Nadgir, Nitya and Siegel, Zachary S. and Wei, Boyi and
               others},
  booktitle = {International Conference on Learning Representations},
  year      = {2026},
  note      = {ICLR 2026 Poster; arXiv:2510.11977}
}

@misc{sghaier2026blame,
  title        = {Don't Blame the Large Language Model: How Agent Harness
                  Evolution Shapes Coding Agent Quality},
  author       = {Ben Sghaier, Oussama and Li, Hao and Adams, Bram and
                  Hassan, Ahmed E.},
  year         = {2026},
  eprint       = {2607.03691},
  archivePrefix= {arXiv},
  primaryClass = {cs.SE},
  note         = {Preprint (v2, 2026-07-20). Verificado 2026-08-08: la
                  atribucion a TOSEM 2026 que circula es FALSA --- arXiv
                  no declara journal_ref ni DOI}
}

% OJO: el DOI desnudo 10.20944/preprints202604.0428 NO resuelve.
% Requiere el sufijo de version.
@misc{meng2026harnesssurvey,
  title     = {Agent Harness for Large Language Model Agents: A Survey},
  author    = {Meng, Qianyu and Wang, Yanan and Chen, Liyi and Li, Yihang
               and Wu, Wei and others},
  year      = {2026},
  doi       = {10.20944/preprints202604.0428.v3},
  publisher = {Preprints.org (MDPI)},
  note      = {Preprint, no revisado por pares (CrossRef type:
               posted-content). Formaliza el harness como
               H = (E, T, C, S, L, V), que braze instancia completo}
}
```

**Venue peer-reviewed dedicado, para round-economics**: AGENT — *International
Workshop on Agentic Engineering*, co-locado con ICSE. La edición 2026
existe y está indexada (30 artículos, **actas ACM**, DOIs bajo
`10.1145/3786167.*`), con invitación a un special issue de IEEE Software.
La convocatoria 2027 caería alrededor de noviembre de 2026.
