# Nota de lectura: AutoDesign (arXiv 2608.13560, 13-ago-2026)

**Luo et al., "AutoDesign: Meta-Harness Optimization for Long-Horizon
Agentic Design"** — Meituan + MBZUAI + varias universidades chinas.
PDF en `docs/2608.13560v1.pdf`. Leída 2026-08-15.

## Qué es

Formaliza el **meta-harness**: un agente de código que optimiza
recursivamente el harness *manteniendo el modelo fijo* (su ec. 2-3 es
exactamente el framing del Paper 1: `H* = argmax_H J(H)` con θ
congelado, citando la distinción model-vs-scaffold). Loop externo:
rollouts sobre un set de tareas → optimizador planner+editor que
propone UNA actualización acotada a UNO de cinco componentes del
harness → compuerta de aceptación (`J_train` sube ∧ `J_dev` no baja) →
registro de optimización `L` como contexto persistente. Instanciado en
paper-to-poster (PosterBench, 100 papers, rúbrica 7-dim regla+VLM,
evaluador externo congelado separado del de optimización). +12.4
puntos promedio sobre 7 coding agents; 7 días, 224 subagentes, 123
iteraciones, 54 updates aceptados. Human study system-blind (64%
Bradley-Terry) valida el protocolo automático.

## El mapeo con braze (evolución convergente, pieza a pieza)

| AutoDesign | braze |
|---|---|
| Outer loop de optimización del harness | Auditorías v1-v9 + backlog de palancas |
| Un componente por iteración (credit assignment) | Disciplina una-palanca-por-A/B |
| Acceptance gate train/dev | McNemar+Holm + piso de ruido + suite de no-regresión |
| Optimization record `L` persistente | PLAN.md, hypothesis-*.md, AUDITORIA-*.md |
| Evaluador congelado vs evaluador de optimización | `metadata.grading` versionado + DBV |
| 5 componentes: Context&Memory / Tools&Specs / Runtime / Orchestration / Eval&Feedback | braze-session+memory / braze-tools-* / sandbox / braze-engine / braze-bench |
| "harness as optimization target distinct from model weights" | La tesis del Paper 1 |

La descomposición en 5 componentes es una taxonomía útil para
organizar el follow-up (mapea 1:1 al workspace).

## Donde braze es metodológicamente más fuerte

Su compuerta de aceptación opera sobre **estimaciones puntuales, sin
estadística**: `J_train(H') > J_train(H) ∧ J_dev(H') ≥ J_dev(H)`. Con
el piso de ruido que braze midió (~2,9 pp/ítem en la suite
discriminante), esa compuerta acepta ruido — tolerable quizás en
régimen frontier con 100 tareas; en régimen SLM (donde braze midió
palancas "obvias" neutras o dañinas: plan en prosa, stencil,
edit-fence) sería una máquina de falsos positivos. braze compensa la
escala del loop (224 subagentes, 7 días) con rigor por update:
pre-registro, McNemar exacto + Holm, piso de ruido, pass^k, y
`sequential.rs` (corte secuencial anytime-valid).

**Ángulo publicable propio**: *meta-harness optimization under noise*
— la maquinaria estadística de braze (pre-registro + e-values
secuenciales como compuerta de aceptación) es exactamente lo que un
meta-harness automatizado necesita para operar en régimen ruidoso/SLM.
Ellos no pueden escribir ese paper con su compuerta; braze sí.

## Escepticismo justo

- Costo del meta-loop brutal vs ~54 updates aceptados, sin intervalos:
  varios updates posiblemente ruido que el dev-set no filtra (guarda
  contra overfitting, no contra varianza).
- `R_meta` lo construye un agente (congelado después, bien), pero
  rúbrica+VLM optimizando contra rúbrica+VLM es acoplamiento; lo
  rescata el human study.
- Sin búsqueda en árbol ni rollback estadístico: un solo harness
  activo, greedy — el registro `L` mitiga repetir intentos, no
  decisiones erradas ya aceptadas.

## Acciones

1. **Cita para la cola del follow-up** (junto a zhu2026babeltele,
   walkinglabs2026, dex2026wsff): clave sugerida `luo2026autodesign`.
   Pasarla por `/verify-refs` con las demás.
2. Si llega la revisión EMSE: entra al mismo párrafo de Related Work
   que dsh + CompInt como evidencia de que "harness engineering" ya
   tiene formalización y resultados a escala industrial (posterior al
   congelamiento del manuscrito — 13-ago vs tag del 29-jul).
3. Discusión del follow-up: el contraste de compuertas de aceptación
   (puntual vs pre-registrada/secuencial) como el gap que el régimen
   SLM vuelve crítico.

## Contexto de la lectura

Llegó el mismo día que la revisión de ferrumox/fox (audit § 11) y dos
días después de adoptar los fixtures de rabbit — tres fuentes externas
independientes convergiendo sobre piezas que braze ya tenía (harness
como objeto de optimización, oráculos de referencia, honestidad de
medición). La convergencia independiente es señal de problema real y
encuadre correcto.
