# Nota: Command Code (CommandCodeAI) — la técnica del Paper 2, vendida sin medir

Fecha: 2026-08-20. Fuente: github.com/CommandCodeAI (org y repo
`command-code`, TypeScript, ~3,7k estrellas, npm `command-code`;
liderado por Ahmad Awais, autor conocido del ecosistema JS; producto
previo del equipo: BaseAI, "framework de agentes serverless con
memoria").

## El claim

Dos frases del material público:

- *"the best coding harness for **open models**"* — nuestro nicho
  declarado, palabra por palabra.
- *"the first coding agent that **learns and adapts to your coding
  preferences over time**"* vía `taste-1`, una "meta neuro-symbolic
  AI": *"every accept, reject, and edit becomes a signal that shapes
  your taste profile"*, perfil portable con `npx taste push/pull`, y
  el eslogan **"Rules decay. Taste compounds."**

Ese eslogan es, literalmente, la hipótesis central del Paper 2: que la
memoria acumulada entre sesiones **amortiza**.

## Lo que NO publican (y es lo que decide la pregunta)

- **Mecanismo**: no dicen si el perfil de taste se inyecta al prompt,
  se recupera bajo demanda, se compila a reglas o se usa para
  fine-tuning. "Neuro-symbolic" sin especificar no es un mecanismo, es
  una etiqueta.
- **Costo**: ni tokens por ronda, ni tamaño del perfil, ni si se
  re-envía por ronda.
- **Evidencia**: cero benchmarks, cero evaluación, cero ablación del
  contenido del perfil contra un control.

## Por qué esto vale para el Paper 2

El paper abre diciendo que la memoria procedimental "suena
obviamente buena" y es *"exactamente el tipo de técnica plausible que
se adopta sin medir"*. Command Code es la **evidencia comercial** de
esa frase: 3,7k estrellas y una tesis de producto construida sobre un
claim que nadie ha medido públicamente.

**Precisión importante — el paper NO refuta este producto.** Nuestro
nulo es de memoria *inyectada por prompt* en ejecutores *locales
pequeños* (20B MoE y 9B denso), donde el token de entrada se paga en
wall-clock del usuario. Command Code apunta a "open models" pero su
material menciona proveedores API (Anthropic, OpenAI, DeepSeek,
GLM/Kimi): régimen distinto, donde el costo es dinero y el ejecutor es
más capaz. Lo que sí aplica es **estructural**: cualquier memoria
re-enviada por ronda cuesta `c × R` (Ec. 1), y su beneficio debe
comprarse en rondas ahorradas. La contribución del paper frente a este
producto no es "no funciona", es **"esta es la condición que tendría
que demostrar que cumple, y aquí está el instrumento (control del
mismo prompt) para demostrarlo sin engañarse"**.

Y el caso refuerza el punto metodológico central: sin un brazo de
control con prompt idéntico, un producto así **no puede distinguir**
si su taste-profile aporta contenido o si simplemente perturbar el
prompt mueve la aguja. Es exactamente el error que nuestro Study 2
cometió y atrapó (+13 → +4).

## Acciones

1. **Related Work del Paper 2, si llega revisión**: cita de
   practicante que ancla la motivación ("la comunidad la adopta sin
   medir") con un caso concreto y con tracción. Clave sugerida:
   `commandcode2026`.
2. **Audit de runtimes § 11**: entra como peer de nicho (compite en
   "harness para modelos abiertos"), junto a goose/aider/kimi-code/pi.
3. **NO adoptar nada de su diseño sin medirlo** — es precisamente la
   lección que el proyecto viene aplicando a sus propias palancas.
