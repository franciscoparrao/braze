# Nota: el campo "harness como variable" ya existe — mapeo del 2026-08-25

Búsqueda de bibliografía reciente (25-ago). Hallazgo de fondo: lo que
veníamos tratando como un arco de cuatro papers **es un campo con
varios frentes**, y varios títulos formulan nuestra propia tesis.
Ninguno estaba en el bib del proyecto.

## Los trabajos

| ref | qué es | por qué nos toca |
|---|---|---|
| **HarnessOpt-Bench** — arXiv 2608.06301 (Ursekar, Shanker, Maurya, Yasser, Kalmath, Chatrath, Xue; 6-ago) | benchmark de LLMs *como optimizadores de harness*: reciben harness semilla + feedback graduado + presupuesto fijo, editan y nominan candidato | **Matiza el ángulo del Paper 3**: puntúa en held-out inaccesible durante la búsqueda, con entorno auditado y diseño explícito para evaluación estocástica. Hallazgo propio suyo: *"optimizer models separate more than the coding harnesses they act through"* |
| **Stop Comparing LLM Agents Without Disclosing the Harness** — arXiv 2605.23950 | posición + AutoHarness (síntesis automática de harness) | El título **es** nuestra crítica de atribución (nota agencia-como-propiedad-del-sistema). Prioridad de lectura alta |
| **Measuring Harness-Induced Belief Divergence in Multi-Step LLM Agents** — arXiv 2607.04528 (5-jul) | mide divergencia de creencias inducida por el harness | Su frase — *"harness design is an experimental variable in agent evaluation, not an implementation detail"* — es literalmente la tesis del Paper 1, publicada por terceros |
| **Harness-Bench** — arXiv 2605.27922 | mide efectos de harness across models en workflows realistas | Competidor directo del encuadre del Paper 1; hay que ver si su diseño mide interacción θ×H |
| **Position: Coding Benchmarks Are Misaligned with Agentic SE** — arXiv 2606.17799 | posición | Conecta con "benchmark-capacidad ≠ confiabilidad agéntica" |
| **SkillsBench** — arXiv 2602.12670 | cómo funcionan las *agent skills* across tasks | Toca D′ (skills explicit-only) |

## Lectura estratégica, sin autoengaño

1. **Ya no somos los únicos** que tratan el harness como variable
   experimental — y algunos lo dicen en el título. El encuadre del
   Paper 1 pierde parte de su novedad de framing, aunque conserva la
   suya: la **curva harness-vs-escala en régimen SLM local** con
   pre-registro por palanca, que ninguno de estos hace.
2. **Prioridad de lectura**: (a) Harness-Bench y belief-divergence,
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
2. **Leer Harness-Bench y belief-divergence en profundidad** — son los
   dos que pueden solapar con el Paper 1.
3. Preparar el párrafo de Related Work actualizado para la eventual
   revisión del Paper 1 (no se toca el manuscrito congelado).
