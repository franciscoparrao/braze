# Descomposición de varianza: ¿manda el harness o el modelo?

Fecha: 2026-08-30
Script: `scripts/variance_decomposition.py` (reproducible, no corre modelos)
Datos: `docs/variance-decomposition-2026-08-30.json`
Motivación: propuesta 3 de "Stop Comparing LLM Agents Without Disclosing the
Harness" (arXiv 2605.23950v1), la única de su lista que braze no cumplía.
Estado: **CERRADO**, con limitaciones fuertes (§ Limitaciones).

## Por qué esta era la pregunta barata

De las cuatro propuestas del paper, braze ya cumplía tres: divulgación del
harness (`metadata.backend_specs` y `suite_fingerprint` en cada sweep),
protocolo harness-first (el bench fija el harness y compara modelos) y
experimentos swap-harness (`+ablate:`, McNemar pareado). Faltaba la
descomposición de varianza — y **se computa sobre sweeps ya archivados,
sin correr un solo modelo.**

## Diseño

Subdiseño factorial completo extraído del corpus de `default.toml`:

**{qwen2.5:3b, qwen2.5:7b, qwen3.5-coder} × {base, +lead, +plan, +plan+lead}
× 19 tareas comunes**, 3.573 corridas de 14 archivos de sweep.

Filtros, todos por reglas que el proyecto ya aplicaba: un solo
`suite_fingerprint`; exclusión de los archivos marcados en su nombre como
`contaminated`/`partial`/`diagnostic`/`smoke`; `run_error` fuera del
denominador; y solo tareas presentes en las 12 celdas, para que el bloque
`task` no arrastre la comparación.

Pass rate por celda:

| modelo | base | +lead | +plan | +plan+lead |
|---|---|---|---|---|
| qwen2.5:3b | 0.737 | 0.939 | 0.628 | 0.930 |
| qwen2.5:7b | 0.803 | 0.947 | 0.821 | 0.947 |
| qwen3.5-coder | 0.994 | 0.947 | 0.950 | 0.941 |

## Resultado

| fuente | η² | η² sin la tarea |
|---|---|---|
| tarea (bloque) | 0.300 | — |
| **config×tarea** | **0.301** | **0.430** |
| residuo (interacción triple) | 0.158 | 0.226 |
| modelo×tarea | 0.093 | 0.133 |
| config de harness | 0.051 | 0.072 |
| modelo | 0.049 | 0.071 |
| modelo×config | 0.048 | 0.068 |

### 1. Entre modelo y harness hay EMPATE, no dominancia

`config/modelo = 1.02×` con todas las tareas, **0.73× exigiendo n≥5 por
celda**. El orden de magnitud es el mismo y el cociente no es estable, así
que lo honesto es "comparables", no "el harness gana".

**Esto NO replica la forma fuerte de la tesis del paper** ("la varianza del
harness supera con creces la del modelo"). En este corpus, con estos tres
modelos, no ocurre.

### 2. Lo que domina es la INTERACCIÓN harness×tarea (η²=0.30)

Seis veces el efecto principal del harness, tan grande como la dificultad
de las tareas, y estable ante el filtro de sensibilidad (0.301 → 0.309).

La lectura: **no existe "el mejor harness" en promedio.** Una palanca ayuda
mucho en unas tareas y estorba en otras, y al promediar sobre la suite esos
efectos se cancelan — por eso el efecto principal parece modesto. Es
exactamente lo que la serie de sweeps del proyecto viene mostrando caso a
caso (el task-list que suma solo, resta con lead; el gate que recupera 9 y
rompe 10), ahora cuantificado en una sola cifra.

Es también un **matiz al paper externo**: la pregunta "¿harness o modelo?"
está mal planteada si la respuesta depende de la tarea más que de ambos
factores juntos.

### 3. El ranking de modelos SÍ se invierte al cambiar el harness

| config | ranking |
|---|---|
| base | coder > 7b > 3b |
| +lead | **7b > coder > 3b** ← inversión |
| +plan | coder > 7b > 3b |
| +plan+lead | **7b > coder > 3b** ← inversión |

Esto sí replica el paper, y es su advertencia más práctica: un benchmark que
no publica su harness puede estar reportando un ranking que se da vuelta con
otra configuración.

### 4. El efecto del harness escala inversamente con el modelo

| ejecutor | rango que el harness mueve |
|---|---|
| qwen2.5:3b | **31.1 pp** |
| qwen2.5:7b | 14.5 pp |
| qwen3.5-coder | 5.3 pp |

Gradiente monótono y limpio. **Es la tesis del Paper 1 —el harness como
variable que compensa la escala del modelo— cuantificada por primera vez en
una sola cifra**, con datos que ya estaban archivados. Y coincide con el
piloto de memoria del 30-ago por el lado contrario: las palancas rinden
donde el modelo es débil, y se vuelven irrelevantes (o estorban: el coder
pierde 4.7 pp con `+lead`) donde el modelo ya satura.

## Limitaciones

Son fuertes y ninguna es cosmética:

- **14 commits distintos del binario.** Los sweeps se corrieron a lo largo
  de semanas y el harness cambió entre ellos por versión, no solo por
  configuración. Parte de lo que se atribuye a `config` y al residuo es
  deriva de versión. Un diseño limpio exigiría re-correr las 12 celdas con
  un solo binario — que es caro, y era justamente lo que este análisis
  quería evitar.
- **n desigual por celda-tarea** (4 a 46). Los pass rates tienen precisión
  muy distinta y no se ponderaron. El filtro n≥5 es todo el margen que el
  corpus permite: con n≥10 no queda ninguna tarea completa.
- **Tres modelos de dos familias** (Qwen 2.5 ×2, Qwen 3.5-coder), todos vía
  Ollama. No hay Gemma ni gpt-oss en el diseño cruzado porque nunca se
  corrieron con `+plan`/`+lead` sobre esta suite.
- **Una sola suite** (`default.toml`), que además está saturada para modelos
  fuertes — el techo del coder (0.994 en base) comprime su rango y contribuye
  al gradiente del hallazgo 4. Ese hallazgo debe re-verificarse en la suite
  discriminante antes de citarse en un paper.
- η² es descriptivo. No hay tests de significancia; con n=1 por celda del
  diseño factorial no hay grados de libertad para un F sin asumir el residuo
  como error puro, y aquí el residuo es la interacción triple, no ruido.

## Consecuencias

1. **Para el Paper 1**: el hallazgo 4 es la cifra que le faltaba a la tesis
   central. Requiere el re-run en la suite discriminante para ser citable.
2. **Para reportar resultados**: el hallazgo 3 justifica que braze publique
   siempre su `backend_spec` — cosa que ya hace, y ahora con evidencia propia
   de por qué importa.
3. **Para elegir en qué trabajar**: el hallazgo 2 dice que buscar "la mejor
   configuración de harness" en promedio es perseguir un efecto que se
   cancela. Lo que rinde es identificar QUÉ palanca sirve para QUÉ clase de
   tarea — que es, en retrospectiva, lo que el proyecto ya venía haciendo
   sweep a sweep sin haberlo enunciado.
