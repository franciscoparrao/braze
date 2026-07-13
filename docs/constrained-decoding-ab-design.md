# Diseño pre-registrado: A/B de constrained decoding vs escalera de rescate

Fecha: 2026-07-12
Estado: **CERRADO — RECHAZADO, con su única iteración corrida y
también negativa.** Mecanismo implementado tal como pre-registrado
(PLAN.md § "Prompt-tools + constrained decoding": 926 tests, smoke con
la firma `rescues=0` en C). Sweep original de 1.045 corridas disparó la
cláusula de iteración (ni adoptaba ni rechazaba en los términos
estrictos). La iteración pre-declarada (`oneOf` por tool con el schema
real en vez de `arguments` genérico) se implementó y corrió: 380
corridas más, mecanismo verificado limpio (`schema_fail` 99→0 en
llama3.2:1b), **pero el pass rate empeoró en las tres filas** (−13.7pp
a −41.1pp según la comparación, todos los ICs Newcombe fuera de cero).
Veredicto final: RECHAZAR sin ambigüedad — la capa de harness sigue
siendo el tradeoff correcto, tener acceso al decoder no lo cambia.
Detalle completo (dos tablas, mecanismo, lectura por tarea) en
`docs/sweep-constrained-decoding-2026-07-12.md`. Ninguna regla de este
documento se modificó después de correr ningún sweep. Sigue la
disciplina de pre-registro
del planner (PLAN.md § split) y del explorador
(`docs/explorador-aislado-ab-design.md`): el criterio se escribe ANTES del
sweep. Origen: la revisión de `JustVugg/colibri` (grammar-forced speculative
drafts, su issue #48) hizo explícito el espectro *constrained decoding (capa
de inferencia) ↔ rescate/reparación (capa de harness)*. braze contribuye la
segunda; este A/B mide si la primera — disponible en Ollama vía structured
outputs (`format` con JSON schema) — la complementa, la subsume, o la
empeora, para los modelos más débiles del proyecto.

## La hipótesis

Para los modelos chicos más débiles en tool-calling, una parte de los fallos
es **sintáctica**: emiten la llamada en una gramática rota o ajena
(`<tool_call>` malformado, JSON desnudo, prosa) que la escalera de rescate
repara *después* de que el modelo la rompió — y lo que no matchea ningún
rung se pierde como texto. El constrained decoding hace la sintaxis
**imposible de romper antes**: el decoder solo puede emitir tokens que
satisfacen el schema del envelope.

Predicción si la hipótesis es cierta: en los executors débiles, el brazo
constrained mejora pass rate sobre baseline Y sobre el brazo prompt-tools
sin constraint (la mejora es del constraint, no del cambio de modalidad),
con `schema_validation_failures + rescued_tool_calls ≈ 0` como verificación
del mecanismo.

Predicción alternativa (la que mataría la palanca): los fallos dominantes de
los débiles son **semánticos** (tool equivocada, args equivocados, plan
equivocado), no sintácticos — y el JSON forzado además les quita el espacio
de narración que los ayuda a pensar (el "format tax" documentado en la
literatura de structured outputs). Entonces constrained ≤ baseline y la
conclusión publicable es: sin control del decoder, la capa de harness es el
tradeoff correcto — y CON control del decoder, tampoco cambia la ecuación.

Expectativa pre-declarada para el control (qwen2.5:3b, fine-tuneado para
function calling): baseline ya emite sintaxis nativa bien; si constrained lo
EMPEORA ahí, eso es el format tax medido, un hallazgo — no un fallo del A/B.

## Mecanismo mínimo a implementar (solo si se decide correr el A/B)

Modo `prompt_tools` + `constrained` en `OllamaBackend` — dos flags
independientes para que los brazos B y C compartan todo salvo el constraint:

1. **Prompt-tools (brazos B y C)**: el request a `/api/chat` va SIN el campo
   `tools`; el inventario se renderiza en un addendum del system prompt
   (nombre, summary, input_schema por tool) con las instrucciones del
   envelope. Es la modalidad que la escalera de rescate ya anticipa — el
   parser de vuelta reusa la infraestructura existente.
2. **Constraint (solo brazo C)**: `format` = JSON schema del envelope:
   ```json
   { "oneOf": [
     { "properties": { "action": {"const": "tool_call"},
                       "reasoning": {"type": "string"},
                       "name": {"enum": ["<tools reales>"]},
                       "arguments": {"type": "object"} },
       "required": ["action", "name", "arguments"] },
     { "properties": { "action": {"const": "final_answer"},
                       "reasoning": {"type": "string"},
                       "text": {"type": "string"} },
       "required": ["action", "text"] }
   ]}
   ```
   El campo `reasoning` opcional está DESDE EL DISEÑO: la literatura del
   format tax dice que quitar todo espacio de pensamiento es el modo de
   falla #1 de constrained decoding — dárselo dentro del schema es la
   mitigación mínima, no una iteración posterior.
3. **Parse de vuelta**: el content (JSON garantizado en C, probable en B) se
   parsea al envelope → `ToolCall` con id sintetizado (mismo generador que
   el rescate) o texto final. Lo que no parsee (posible en B, imposible en
   C salvo bug de Ollama) cae a la escalera de rescate normal — B mide
   exactamente "prompt-tools con la red de seguridad actual".
4. **Un solo tool call por respuesta** (el envelope es un objeto, no un
   array): puede subir `avg_rounds` en los brazos B/C. Se mide, no se
   corrige — es parte del costo real de la modalidad.
5. **Gates**: `+ablate:prompt-tools` (brazo B) y `+ablate:constrained-tools`
   (brazo C, implica prompt-tools). Off por default en todos los
   composition roots; warning H-13-style si se usan fuera de Ollama.
6. **Sin eventos nuevos**: el mecanismo se verifica con columnas existentes
   (`schema_fail`, `rescues`, `rounds`, tokens).

## Brazos y executors

Por executor, 3 brazos sobre `suites/default.toml` (19 tareas, 5 reps,
temp 0.2, Nitro, `--no-ollama-stop`, seeds pareados):

| Brazo | Qué aísla |
|---|---|
| A: baseline | tool-calling nativo + escalera de rescate (el default actual) |
| B: `+ablate:prompt-tools` | el cambio de modalidad (nativo→prompt) sin constraint, rescate activo |
| C: `+ablate:constrained-tools` | el constraint sobre B — la palanca en cuestión |

Executors (el gradiente débil→tuneado):

- **llama3.2:1b** — el caso histórico (18% en el sweep 2026-07-04): genérico
  chico con tools nativas soportadas pero frágiles.
- **gemma4:e2b** — driver diario actual, familia sin fine-tune de function
  calling.
- **qwen2.5:3b** — control tuneado: donde el format tax se vería más limpio.
- **gemma3:1b** (fila exploratoria, solo brazos B/C): Ollama lo RECHAZA en
  nativo (HTTP 400 "does not support tools") — el brazo A no existe. Si B/C
  lo vuelven corrible, es la demo de "constrained/prompt tools desbloquea
  modelos que la API nativa excluye". Se reporta pero queda FUERA del
  criterio de adopción (no tiene baseline contra el cual comparar).

Total: 3 executors × 3 brazos × 19 × 5 = 855 + gemma3:1b 2 × 95 = **1.045
corridas** (~2-3h de Nitro con estos tamaños).

## Criterio pre-registrado

- **Adoptar** (promover `constrained-tools` a knob de config documentado
  para modelos débiles, sección en el paper) si, en al menos un executor
  débil (llama3.2:1b o gemma4:e2b):
  - C − A ≥ **+10pp** agregado, fuera del ruido (Newcombe 95% sobre el
    delta within-sweep), **Y**
  - C − B > 0 (la ganancia es del constraint, no del cambio de modalidad),
    **Y**
  - el mecanismo verifica: `schema_fail + rescues` de C ≈ 0.
- **Rechazar** (documentar como negativo en la discusión del paper: la capa
  de harness es el tradeoff correcto incluso con decoder controlable) si
  C ≤ A en ambos débiles, o si C > A pero C ≤ B (la mejora era la modalidad
  prompt, no el constraint — adoptar B sería otra decisión, anotarla).
- **Iterar UNA vez** (misma regla que el planner) solo si el modo de falla
  dominante es identificable y atacable. Candidata única pre-declarada: el
  `arguments: {"type": "object"}` genérico deja pasar args mal formados por
  tool → la iteración es el envelope con `oneOf` por tool usando su
  `input_schema` real. Nada más se itera.
- La fila qwen2.5:3b y la fila gemma3:1b se REPORTAN siempre (format tax y
  unlock, respectivamente) pero no participan del criterio adoptar/rechazar.

## Riesgos anotados

- **Format tax**: mitigado parcialmente con el campo `reasoning` en el
  envelope; si aún así C < B en todos lados, eso ES el resultado.
- **Streaming**: Ollama structured outputs compone con streaming, pero el
  gateo de fences del render TUI no aplica a un content 100% JSON —
  irrelevante para el bench (sin TUI), anotar si se promueve el knob.
- **Schema grande**: con las ~8 tools locales del suite el schema es chico;
  con `noise_tools` o gateways MCP el `enum` de nombres crece — fuera del
  alcance de este A/B (el suite corre con noise 0), anotar como interacción
  con la deferral C′.1 si se adopta.
- **Thinking models**: excluidos como executors (el `thinking` field de
  Ollama interactúa con `format` de maneras no documentadas) — los 4
  elegidos no lo son.
- **Comparabilidad**: A usa el parseo nativo de Ollama, B/C el textual — el
  system prompt DIFIERE entre A y B/C por construcción (addendum de tools).
  Por eso la atribución exige el sandwich A/B/C y no solo A/C.

## Conexión con el paper

Entra a la discusión como el experimento del espectro
inferencia-vs-harness: el rescate textual (nuestra contribución) funciona
contra cualquier API sin control del decoder; el constrained decoding
requiere ese control pero hace la prevención total. Cita fresca:
`JustVugg/colibri` #48 (grammar-forced drafts para function calling, 2026).
Si el A/B se corre, la tabla va en § ablations; si no, el diseño mismo se
cita como future work pre-registrado.

## Qué NO es este documento

No es un compromiso de implementación. Costo estimado: mecanismo M (modo
OllamaBackend + render del addendum + parser del envelope + 2 claves ablate
+ tests, ~medio día), sweep ~2-3h de Nitro. La decisión de gastarlo queda
para cuando la cola del manuscrito esté vacía — este diseño solo garantiza
que, si se corre, se corre pre-registrado.
