# Nota: el campo "harness como variable" ya existe — mapeo del 2026-08-25

Búsqueda de bibliografía reciente (25-ago). Hallazgo de fondo: lo que
veníamos tratando como un arco de cuatro papers **es un campo con
varios frentes**, y varios títulos formulan nuestra propia tesis.
Ninguno estaba en el bib del proyecto.

## Los trabajos

| ref | qué es | por qué nos toca |
|---|---|---|
| **HarnessOpt-Bench** — arXiv 2608.06301 (Ursekar, Shanker, Maurya, Yasser, Kalmath, Chatrath, Xue; 6-ago) | benchmark de LLMs *como optimizadores de harness*: reciben harness semilla + feedback graduado + presupuesto fijo, editan y nominan candidato | **Matiza el ángulo del Paper 3**: puntúa en held-out inaccesible durante la búsqueda, con entorno auditado y diseño explícito para evaluación estocástica. Hallazgo propio suyo: *"optimizer models separate more than the coding harnesses they act through"* |
| **Stop Comparing LLM Agents Without Disclosing the Harness** — arXiv 2605.23950 | posición (Binding Constraint Thesis + protocolo de descomposición de varianza) | El título **es** nuestra crítica de atribución. LEÍDO 2026-08-29 → `nota-lectura-binding-constraint-2026-08-29.md`. **Corrección: AutoHarness NO es de ellos**, lo citan como trabajo de terceros junto con ADAS, Meta-Harness y AHE |
| **Measuring Harness-Induced Belief Divergence in Multi-Step LLM Agents** — arXiv 2607.04528 (5-jul) | mide divergencia de creencias inducida por el harness | Su frase — *"harness design is an experimental variable in agent evaluation, not an implementation detail"* — es literalmente la tesis del Paper 1, publicada por terceros |
| **Harness-Bench** — arXiv 2605.27922 | mide efectos de harness across models en workflows realistas | Competidor directo del encuadre del Paper 1; hay que ver si su diseño mide interacción θ×H |
| **Position: Coding Benchmarks Are Misaligned with Agentic SE** — arXiv 2606.17799 (Tessl) | posición | LEÍDO 2026-08-29. Su Tabla 1 (Opus 4.6: 79,8→58,0 según harness, modelo fijo) es la tesis del Paper 1 en datos ajenos. Cita AI21 (200k corridas) y Anthropic sobre ruido de infraestructura |
| **SkillsBench** — arXiv 2602.12670 | cómo funcionan las *agent skills* across tasks | LEÍDO 2026-08-29. +16,6 pp de skills curadas (rango +4,1 a +25,7); "skill efficacy is a property of a specific agent stack, not a universal constant". Su diseño de 3 condiciones es el modelo para el pre-registro de call-time skills |

## Lectura estratégica, sin autoengaño

1. **Ya no somos los únicos** que tratan el harness como variable
   experimental — y algunos lo dicen en el título. El encuadre del
   Paper 1 pierde parte de su novedad de framing, aunque conserva la
   suya: la **curva harness-vs-escala en régimen SLM local** con
   pre-registro por palanca, que ninguno de estos hace.
2. ~~**Prioridad de lectura**~~ — **LAS SEIS LEÍDAS al 2026-08-29.**
   Orden original: (a) Harness-Bench y belief-divergence,
   porque pueden solaparse con el Paper 1 y hay que citarlos o
   diferenciarse antes de la revisión; (b) Stop Comparing, por el
   ángulo de atribución; (c) HarnessOpt-Bench ya leído en abstract y
   ya incorporado como matiz al Paper 3.
3. **Riesgo concreto para el Paper 1**, que está EN REVISIÓN en EMSE:
   si un revisor conoce Harness-Bench o belief-divergence y el
   manuscrito no los cita, se lee como desconocimiento del campo. Son
   de mayo y julio de 2026; el manuscrito se congeló el 29-jul. Hay
   que decidir si se mencionan en una eventual revisión — no se puede
   tocar ahora, pero conviene tener el párrafo listo.

## Acciones

1. Al bib del follow-up: `ursekar2026harnessopt`, y tras leerlos, los
   otros cinco. `/verify-refs` en su momento.
2. ~~**Leer Harness-Bench y belief-divergence en profundidad**~~ —
   HECHO 2026-08-25, con corrección a favor de Belief Divergence
   (controlan en el diseño; no propagan a la decisión). Ver
   `nota-lectura-harnessbench-belief-2026-08-25.md`.
3. ~~Preparar el párrafo de Related Work~~ — **HECHO 2026-08-27**,
   redactado en LaTeX al final de esa misma nota. Pendiente: crear
   las claves de bib y pasarlas por `/verify-refs` antes de usarlo.

## Adenda 2026-08-27: el mapeo se amplía más allá de los papers

Dos trabajos que **no son papers de evaluación** y que igual caen del
lado del hueco: **Apache Maka** (workspace de agentes en incubación
ASF, con un harness de eval declarativo casi isomorfo a `braze-bench`
y sin control de varianza documentado) y **Cordis** (formalismo de
composabilidad dinámica de DeepSeek-AI + Peking University, 92 pp.,
que motiva con *self-evolving agent harnesses* y no mide ninguno).

Consecuencia para el Paper 3: el hueco no es de la literatura de
benchmarks sino del campo entero, y se formula mejor por **tres
poblaciones** (papers de evaluación / infraestructura en producción /
formalismos de arquitectura) que por una sola escala. Detalle,
matices y qué NO hacer con esto en
`docs/nota-lectura-maka-cordis-2026-08-27.md`.
