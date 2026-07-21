# Textos listos para las 2 OSF Registrations — 2026-07-21

Ensamblados verbatim desde los design docs (punteros resueltos). Al
crear cada registration en osf.io/registries (template
"Preregistration" o el genérico), incluir la línea de procedencia tal
cual — la registration archiva con timestamp independiente un criterio
que ya vivía en git, y debe decirlo.

---

## Registration 1 — gemma4:e4b solo baseline

**Title**: gemma4:e4b solo baseline — pre-registered framing criterion
(braze harness-vs-scale paper)

**Provenance note (incluir en la registration)**: This registration
archives, with an independent timestamp, a criterion whose design
document (docs/gemma4-e4b-solo-baseline-design.md) is dated 2026-07-13 in
its content but whose FIRST git commit (fedbc3e) is 2026-07-18 --- five
days AFTER the deciding sweep ran (2026-07-13). Neither this
registration's timestamp nor the git history can demonstrate that the
criterion preceded the results; the criterion is declared, not provably
pre-registered. Mitigating context, not proof: the measurement
concluded against the paper's original thesis and forced its framing
revision. This registration exists to archive the text verbatim and to
mark the project's adoption of a register-before-run gate for all
future criteria.

**Hypothesis**:

El headline del paper (`\S\ref{sec:curve}`) es que un executor de 1B
con lead (89%) supera a los baselines sin asistir de 3B (68%) y 7B
(80%). `gemma4:e4b` es el modelo que abre el turno en TODO arm `+lead`
del paper — pero su propio pass rate en solitario, sobre la misma
suite, nunca se midió. Si `gemma4:e4b` solo ya saca un número alto en
esta suite, gran parte de la ganancia atribuida al mecanismo de "lead
proactivo" podría ser, en los hechos, "enrutar más trabajo a un modelo
más capaz" — una explicación mucho menos interesante que "el harness
compensa la escala".

Predicción si la hipótesis "es principalmente capacidad del lead" es
cierta: `gemma4:e4b` solo (baseline arm, sin lead, sin planner, harness
default) cae **cerca o por encima** del 89% del compuesto 1B+lead — es
decir, el compuesto no le agrega mucho al techo que el propio
`gemma4:e4b` ya alcanzaría actuando solo.

Predicción alternativa (la que confirma el framing actual del paper):
`gemma4:e4b` solo cae **claramente por debajo** de 89% — el compuesto
1B+lead logra algo que ni el 1B solo (19%) ni el `gemma4:e4b` solo
logran por separado, evidencia de sinergia genuina entre "el lead abre,
el executor de 1B ejecuta" y no solo de sustitución de capacidad.

**Design**:

Un solo arm nuevo, mismos parámetros que el resto de la curva de
escala (`docs/sweep-curva-multiescala-2026-07-10.md`): suite
`crates/braze-bench/suites/default.toml` (19 tasks), 5 repeticiones
($n{=}95$), temp 0.2, Nitro, `--no-ollama-stop`, un sweep a la vez.

| Arm | Backend spec | Qué mide |
|---|---|---|
| `gemma4:e4b` solo | `ollama:gemma4:e4b` (sin `+lead:`, sin `+plan:`, sin `+ablate:`) | Techo de capacidad del propio modelo lead, aislado del compuesto |

No hace falta re-correr ningún arm existente — este es un punto de
referencia nuevo que se superpone sobre la Fig.~1 ya generada, no
reemplaza ninguno de los 16 cells del sweep original.

**Analysis plan**:

Sea $X$ el pass rate (Wilson 95% CI) de `gemma4:e4b` solo, $n{=}95$.

- **Revisar el framing** de "+70pp a 1B / harness compensa una orden
  de magnitud de parámetros" (abstract, intro, contribuciones,
  `\S\ref{sec:curve}`) si $X \geq 80\%$ **O** si el intervalo Newcombe
  95% del delta (1B+lead $-$ $X$) no excluye claramente el cero a
  favor del compuesto. En ese caso, agregar explícitamente: "part of
  the 1B+lead gain reflects `gemma4:e4b`'s own standalone capability on
  this suite ($X$\%)" y reencuadrar la contribución como "el lead
  proactivo enruta eficientemente hacia un modelo capaz" en vez de
  "el harness compensa la escala" como mecanismo distinto de "usar un
  modelo más grande en algún punto del loop".
- **Confirmar el framing actual** si $X \leq 70\%$ con el intervalo
  Newcombe 95% del delta (1B+lead $-$ $X$) excluyendo cero a favor del
  compuesto — evidencia de que el compuesto logra algo que ni el 1B
  solo (19%) ni `gemma4:e4b` solo logran por separado. `gemma4:e4b`
  solo se cita entonces como evidencia a favor, no como amenaza.
- **Zona intermedia** (70-80%, o CIs solapados sin exclusión clara):
  reportar honestamente como "parcialmente atribuible a la capacidad
  del lead, parcialmente a la composición" — matizar el lenguaje de
  "compensa una orden de magnitud" independientemente del resultado
  puntual (ver Fase 4 del plan, ítem "Abstract").
- Reportar además $X$ contra los baselines sin asistir de 3B (68%),
  7B (80%) y el techo qwen3.5-coder (98%) — posiciona a `gemma4:e4b`
  en su propio tier de capacidad para el lector, independiente del
  criterio adopt/reject de arriba.

No hay cláusula de iteración: es una medición de un punto de
referencia, no una palanca a ajustar — el resultado, sea cual sea, se
reporta y se usa para calibrar el framing, no se itera.

---

## Registration 2 — external bare-lead baseline

**Title**: Bare lead+executor external baseline — pre-registered
criterion (braze harness-vs-scale paper)

**Provenance note (incluir en la registration)**: This registration
archives, with an independent timestamp, a criterion whose design
document (docs/external-harness-baseline-design.md) is dated 2026-07-13 in
its content but whose FIRST git commit (fedbc3e) is 2026-07-18 --- five
days AFTER the deciding sweep ran (2026-07-13). Neither this
registration's timestamp nor the git history can demonstrate that the
criterion preceded the results; the criterion is declared, not provably
pre-registered. Mitigating context, not proof: the measurement
concluded against the paper's original thesis and forced its framing
revision. This registration exists to archive the text verbatim and to
mark the project's adoption of a register-before-run gate for all
future criteria.

**Hypothesis**:

El headline del paper es que el compuesto lead+executor de `braze`
(1B+lead, 89.5%) rescata dramáticamente al 1B de su baseline (19%). Pero
`braze` no es solo "lead abre, executor ejecuta" — es esa composición
MÁS rescate textual, compactación de observaciones, deferral de tools,
post-edit check, etc. (Tabla de levers del paper). ¿Cuánto del +70pp le
corresponde a la composición lead+executor en sí, y cuánto a la
ingeniería adicional específica de `braze`?

Predicción si "la composición basta" es cierta: un loop mínimo
lead+executor, sin ninguna otra palanca de `braze`, sobre la MISMA
composición (mismo modelo lead `gemma4:e4b`, mismo modelo executor
`llama3.2:1b`, mismo `lead_turns=3`), alcanza un pass rate **cercano** al
89.5% del compuesto completo de `braze` — es decir, la ingeniería
adicional de `braze` no agrega mucho más allá de la composición cruda.

Predicción alternativa (la que justificaría la ingeniería de `braze`
como valor agregado real, no solo la composición): el loop bare cae
**sustancialmente por debajo** de 89.5% — evidencia de que el rescate
textual, la compactación, o el manejo de errores de `braze` son los que
realmente cierran la brecha, no la mera presencia de un lead.

Tercer punto de referencia (ya medido, Fase 1): `gemma4:e4b` solo
alcanza 91.6% en esta suite — un techo de capacidad que ni el compuesto
de `braze` ni (presumiblemente) el loop bare deberían superar de forma
sostenida.

**Design**:

Un solo arm nuevo, misma suite y convención que el resto del paper:
suite `crates/braze-bench/suites/default.toml` (19 tasks), 5
repeticiones ($n{=}95$), temp 0.2, Nitro, `--no-ollama-stop`.

| Arm | Spec | Qué mide |
|---|---|---|
| bare lead+executor | `--external "bare-lead:ollama:llama3.2:1b+lead:ollama:gemma4:e4b"` | Composición lead+executor sin ninguna otra palanca de `braze` |

Escala elegida: **1B** — la fila más informativa (mayor gain relativo
del compuesto de `braze` en el paper, +70pp) y la que hace la pregunta
de este documento más nítida. Si el cómputo de Nitro lo permite después,
extender a 3B/7B/coder queda como trabajo futuro, no bloqueante para
este criterio.

No hace falta re-correr ningún arm existente de `braze` — el compuesto
1B+lead (85/95, 89.5%) y `gemma4:e4b` solo (87/95, 91.6%) ya están
medidos (Fase 1, `docs/gemma4-e4b-solo-baseline-design.md`).

**Analysis plan**:

Sea $Y$ el pass rate (Wilson 95% CI) del loop bare, $n{=}95$. Comparado
contra el compuesto completo de `braze` (85/95, 89.5% [81.7,94.2]%) vía
delta Newcombe 95% CI (compuesto `braze` $-$ $Y$):

- **La composición basta** (la ingeniería adicional de `braze` no es lo
  que cierra la brecha) si $Y \geq 79\%$ **Y** el intervalo del delta no
  excluye claramente el cero a favor del compuesto de `braze` (es decir,
  no hay evidencia de que `braze` le gane al loop bare fuera de ruido).
  En ese caso, el paper debe reportar explícitamente que gran parte del
  headline es atribuible a la composición lead+executor per se, no a la
  ingeniería específica de `braze` — sección nueva o expansión de
  `\S\ref{sec:curve}`/`\S\ref{sec:discussion}`.
- **La ingeniería de `braze` agrega valor real** si $Y \leq 69\%$ **Y**
  el intervalo del delta excluye cero a favor del compuesto de `braze`
  (`braze` claramente por encima del loop bare, fuera de ruido). En ese
  caso, el paper gana su primera comparación genuinamente externa
  positiva: `braze` no es solo "cualquier lead+executor," su ingeniería
  específica mide una diferencia real.
- **Zona intermedia** (69-79%, o CIs solapados sin exclusión clara):
  reportar como resultado mixto — parte de la ganancia es composición,
  parte es ingeniería — sin forzar una lectura binaria que los datos no
  sostienen.
- Reportar además $Y$ contra `gemma4:e4b` solo (91.6%) y contra el
  baseline sin asistir del 1B (19%) — posiciona el loop bare en el mismo
  espacio de referencia que el resto de la curva, independiente del
  criterio adopt/reject de arriba.

**Sin cláusula de iteración**: a diferencia de constrained-decoding, este
documento no propone una palanca a ajustar tras un resultado ambiguo —
es una medición de un punto de comparación, igual que el baseline de
`gemma4:e4b`. El resultado, cualquiera que sea, se reporta como está.

---

Tras crear ambas: pegar los 2 links en el \todo de paper/main.tex
(~línea 704) y actualizar el "Estado" de cada design doc a "CERRADO —
registrado en OSF" con su ID.
