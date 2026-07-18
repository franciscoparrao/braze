# EMSE pre-submission review (2026-07-13) — issue checklist

Fuente: `/paper-review-emse` sobre `paper/main.tex`, protocolo completo
(3 personas independientes + comparación contra 3 peers reales del
corpus EMSE + auditoría visual de las 3 figuras). Review completa:
`~/vault/journals/emse/reviews-generated/2026-07-13_16-34_braze-harness-paper.md`.

**Veredicto**: Major Revision. El aparato empírico (pre-registro,
Wilson/Newcombe CIs, disclosure de threats to validity) es sólido; los
issues son huecos de diseño concretos y accionables, no problemas
estructurales del proyecto.

**Adenda 2026-07-13 (post Fase 1+3)**: los tres brazos indistinguibles
(`gemma4:e4b` solo, compuesto `braze`, loop bare) se replicaron a
$n{=}285$ pooled (`docs/power-increase-2026-07-13.md`) — el nulo se
confirmó con más precisión (semiancho ~3.5pp vs ~6-7pp original),
mismo veredicto cualitativo. Paper actualizado con los números pooled.

## Issues críticos (bloquean o debilitan la claim central)

### 1. Sin baseline de harness externo — ✅ RESUELTO (2026-07-13)
Toda comparación del paper era `braze` contra sí mismo. La claim del
título ("the harness compensates the model") es sobre harnesses en
general, evidenciada por exactamente un harness.

**Resultado**: loop bare lead+executor (implementado desde cero, sin
ninguna palanca de `braze` salvo la composición lead+executor) = 84/95
= 88.4% [80,93]% Wilson — **estadísticamente indistinguible** tanto del
compuesto completo de `braze` (85/95, 89.5%; delta Newcombe 95% CI
$[-8.2,+10.3]$pp) como de `gemma4:e4b` solo (87/95, 91.6%; delta
Newcombe 95% CI $[-12.1,+5.7]$pp). El criterio pre-registrado disparó
"LA COMPOSICIÓN BASTA" sin ambigüedad — tres mediciones independientes
(helper solo, compuesto de `braze`, loop bare) caen en la misma banda
88-92%. Detalle completo: `docs/external-harness-baseline-design.md`.
Sweep crudo: `docs/sweep-external-bare-lead-2026-07-13.json`.

- [x] Decidir alcance mínimo del baseline externo — resuelto como
      ablación interna (`BareLeadExecutor`, ver
      `docs/external-harness-baseline-design.md`), no framework de
      terceros
- [x] Adaptar/implementar un runner mínimo compatible con la suite de
      `braze-bench` (19 tasks) para ese harness externo —
      `crates/braze-bench/src/bare_lead_baseline.rs` + flag `--external`
- [x] Correr el baseline externo al menos en la escala más informativa
      (1B) — `docs/sweep-external-bare-lead-2026-07-13.json`
- [x] Reportar la comparación en el paper (nueva
      \S\ref{sec:external}, entre \S\ref{sec:curve} y
      \S\ref{sec:mechanism}; también actualizado abstract,
      contribuciones, \S\ref{sec:threats} y conclusión)
- [ ] **Futuro (no en este plan)**: versión más fuerte con un
      framework genuinamente de terceros — candidato encontrado:
      **Pi** (pi.dev, Earendil Inc., MIT) — soporta Ollama, tiene modo
      scriptable (`pi -p`, `--mode json`, RPC stdin/stdout) apto para
      shell-out no interactivo. Ya citado en el paper
      (`\citep{pi-dev}`) como candidato no implementado. Detalle
      completo, caveats (flujo de permisos no integrado) y plan de
      wiring en `docs/external-harness-baseline-design.md` §
      "Candidato futuro para comparación genuinamente externa: Pi
      (pi.dev)"

### 2. Falta el baseline solo de `gemma4:e4b` — ✅ RESUELTO (2026-07-13)
El "1B+lead (89%) supera a 3B (68%) y 7B (80%)" nunca se comparó contra
lo que `gemma4:e4b` solo (sin executor 1B) saca en la misma suite.

**Resultado**: `gemma4:e4b` solo = 87/95 = 91.6% [84,96]% Wilson —
**estadísticamente indistinguible** del compuesto 1B+lead (85/95 =
89.5% [82,94]%; Newcombe 95% CI del delta $[-10.9, +6.6]$pp, cruza
cero). El criterio pre-registrado disparó "REVISAR FRAMING" sin
ambigüedad. Detalle completo:
`docs/gemma4-e4b-solo-baseline-design.md`. Sweep crudo:
`docs/sweep-gemma4-e4b-solo-2026-07-13.json`.

- [x] Agregar `gemma4:e4b` como fila baseline (sin lead/planner, es el
      propio lead) al sweep de curva de escala, mismo suite, $n{=}95$
- [x] Reportar el número explícitamente en \S\ref{sec:curve} (nuevo
      párrafo "A pre-registered solo baseline of the lead complicates
      the headline, honestly")
- [x] Actualizar Fig.~1 con esta referencia (banda IC 95% + línea
      punteada horizontal, `paper/R/fig1_curva.R`)
- [x] Revisar el framing en abstract, contribuciones, \S\ref{sec:curve},
      \S\ref{sec:threats}, \S\ref{sec:discussion}, Conclusión, y el
      TODO de título candidato — todos actualizados con el matiz de
      que el "1B beats 7B" es en gran parte la capacidad del propio
      `gemma4:e4b`, no una propiedad emergente del compuesto

### 3. Pre-registro auto-alojado (git commits propios)
El criterio pre-registrado vive en el propio historial git del autor,
no en un registro independiente. EMSE tiene un Open Science Review
Board dedicado — un reviewer de ese perfil lo va a cuestionar primero.

- [x] Decisión tomada (2026-07-13): usar OSF para los criterios nuevos
      de este plan de resolución (`gemma4-e4b-solo-baseline-design.md`
      y el próximo, `external-harness-baseline-design.md`)
- [ ] **Pendiente real**: el sweep de `gemma4:e4b` solo (Issue 2, ya
      corrido) se lanzó SIN esperar el registro OSF — no tengo
      credenciales de OSF en este entorno, así que el usuario debe
      crear la registration a mano con el texto ya armado en
      `docs/gemma4-e4b-solo-baseline-design.md` § "Registro externo".
      Hasta que eso se cierre, el paper debe citar el criterio como
      "committed to git, OSF registration pending" — no implicar
      registro externo completo
- [x] Agregar un párrafo en el paper justificando el mecanismo mixto
      (git + OSF a partir de este punto) para los criterios anteriores
      que quedaron solo en git (planner, explorador, constrained-decoding)
      — ✅ 2026-07-16, nuevo párrafo "Registry mechanism: git-only, then
      git+OSF" en \S\ref{sec:setup}, entre "Pre-registration" y
      \S\ref{sec:results}. Queda un `\todo` real dentro del párrafo: los
      IDs/links de las dos OSF Registrations (gemma4:e4b solo,
      bare-lead externo) siguen pendientes de que el usuario las cree a
      mano en osf.io/registries — el texto ya está preparado en ambos
      design docs.

### 4. Sin validación independiente del grader automático — ✅ RESUELTO (2026-07-13)
~4.000+ runs calificados por asserts scripteados; ya se documentó un
bug real en un assert anterior (aceptaba narración como respuesta
válida).

**Resultado**: `BRAZE_BENCH_KEEP_SESSIONS` implementado como flag real
(`crates/braze-bench/src/preserve.rs` nuevo — 141 tests verdes, clippy
limpio). 62 transcripciones preservadas y calificadas a mano (38 de
scale-curve: `llama3.2:1b` baseline + `+lead`, 19 tasks c/u; 24 de
tool-deferral: `qwen2.5:3b` deferred + full-inventory, 6 tasks × 2 reps
c/u) contra el veredicto automático: **62/62 (100%) de acuerdo**. Un
hallazgo cualitativo notable: el check dual texto+archivo atrapó una
confabulación real (el modelo escribió el string literal
`"int_a + int_b"` en el archivo pero su texto final afirmó "the
sum...is 30") que un check de solo-texto habría dejado pasar. Detalle
completo: `docs/grader-validation-2026-07-13.md`.

- [x] Samplear N=30-50 runs a través de arms/tasks (terminó en 62,
      diseño de muestra por arm completo en vez de sampling aleatorio)
- [x] Calificar a mano las transcripciones contra el veredicto
      automático
- [x] Reportar la tasa de acuerdo humano↔automático (nuevo párrafo
      "Grader validation" en \S\ref{sec:setup})
- [ ] Si aparece otro bug de assert, auditar su alcance en sweeps ya
      corridos y disclosurarlo igual que el anterior

### 5. Manuscrito incompleto
No es un problema científico, pero bloquea la submission formal.

- [x] Decidir título final (dos candidatos ya anotados en el `\thanks`)
      — ✅ 2026-07-16: "Not All Scaffolding Helps: A Pre-Registered,
      Lever-by-Lever Study of Agentic Harnesses at Small-Model Scales".
      Se descartó el título por defecto ("The Harness Compensates the
      Model...") porque el abstract actual lo contradice (el baseline
      `gemma4:e4b` solo empata con el compuesto); "Not All Scaffolding
      Helps" en cambio coincide casi textual con la frase de cierre del
      abstract y con el título del párrafo de discusión en
      \S\ref{sec:discussion} ("Not all scaffolding helps, and the sign
      depends on..."). `\thanks{\todo{...}}` removido del `\title`.
- [x] Completar afiliación + email del autor — ✅ 2026-07-17,
      Departamento de Ingeniería Informática, Universidad de Santiago
      de Chile; francisco.parra.o@usach.cl
- [ ] Correr `/zenodo` para empaquetar código + los 7 JSON de sweeps →
      obtener el/los DOI — decisión explícita 2026-07-17: diferido,
      no correr todavía
- [x] Confirmar el commit hash faltante del sweep
      `constrained-decoding` (Tabla de inventario de sweeps) — ✅
      2026-07-16, `acce118` (commit que agregó
      `docs/sweep-constrained-decoding-2026-07-12.json`)
- [x] Transcribir verbatim los 3 textos de pre-registro del Apéndice
      (criterio del planner en PLAN.md, diseño del explorador aislado,
      criterio de constrained-decoding) — ✅ 2026-07-16, con hash de
      commit y gloss en inglés para cada uno
      (\S\ref{app:prereg})
- [ ] Publicar el repo (o dejarlo listo para publicar) y completar la
      URL en "Artifact availability" — decisión explícita 2026-07-17:
      diferido, no publicar todavía

**Estado 2026-07-17**: de los 8 `\todo` originales, quedan 4, los
cuatro por decisión explícita de diferir (no por bloqueo técnico):
DOI Zenodo (x2 menciones), IDs de las 2 OSF Registrations, URL del
repo. El texto de las 3 acciones pendientes ya está preparado
(`docs/gemma4-e4b-solo-baseline-design.md` y
`docs/external-harness-baseline-design.md` § "Registro externo (OSF)"
para OSF; `/zenodo package` para el DOI) — son ejecutables en una
sesión futura sin más trabajo de redacción. El manuscrito compila
limpio (23 páginas) y está, en ese sentido, listo salvo por estas
acciones externas.

## Issues menores (mejoran el paper, no bloquean por sí solos)

Los 5 issues menores fueron resueltos el 2026-07-17. Manuscrito
recompilado limpio en cada paso (0 referencias indefinidas, 0
`Overfull \hbox`, 24 páginas).

- [x] Abstract: matizar el framing de escala ("+70pp at 1B... outperforms
      3B and 7B") para reconocer el confound de familia de modelo que
      ya se admite en Threats to Validity — ✅ agregada la cláusula
      "though scale and model family are not cleanly separated here
      (1B: Llama; 3B/7B: Qwen)" en la primera mención del abstract, sin
      `\S\ref{}` (el abstract no usa cross-refs de sección en ningún
      otro punto, se mantuvo esa convención)
- [x] "Compositional brittleness" (\S\ref{sec:searchtools}): ampliar el
      muestreo de transcripciones (hoy es un puñado de casos) o suavizar
      el lenguaje de certeza del mecanismo — ✅ suavizado: el párrafo
      ahora distingue explícitamente el probe `noisy_no_tool` (3
      condiciones × n=15 = 45 corridas controladas, el que realmente
      aísla el mecanismo) del probe `noisy_multi_step` (muestra
      manual de 5 transcripciones, ahora etiquetada como
      "corroborate... rather than independently establish" en vez de
      presentarse con la misma certeza que el probe bien-powered
- [x] Unificar la codificación de color entre figuras — ✅ la colisión
      real (verificada, no la descrita literalmente en este checklist):
      `#0072B2` (azul) significa "+lead" en Fig.~1 y "deferred
      (search_tools)" en Fig.~3 (ambas correctamente en el eje
      configuración-de-harness), pero también "Qwen 3.5 Coder" en
      Fig.~2 (eje identidad-de-modelo). Cambiado el azul de Fig.~2 a
      `#009E73` (verde-azulado, mismo set Wong colorblind-safe),
      liberando el azul para su único significado consistente. Figura
      regenerada (`Rscript paper/R/fig2_rescate.R`), inspeccionada
      visualmente vía el preview PNG.
- [x] Considerar dar a la investigación de constrained decoding
      (\S\ref{sec:ablations}) su propia subsección — ✅ nueva
      `\subsection{Constrained decoding: syntax-level control versus
      harness-level rescue}\label{sec:constrained}`; 7 referencias
      cruzadas migradas donde correspondía semánticamente (las que sí
      son sobre textual rescue quedaron en `sec:ablations`)
- [x] Citar/discutir explícitamente la literatura de mining studies
      sobre testing practice en frameworks de agentes reales (el Peer 1
      de la review) como la metodología de amplitud que este paper
      deliberadamente no intenta — ✅ nueva entrada `hasan2026testing`
      en refs.bib (metadata verificada contra la API de Crossref
      2026-07-17; texto completo pagado, no se citan hallazgos
      específicos que no se leyeron de primera mano) + nuevo párrafo
      "Breadth versus depth" en Related Work

## Referencia rápida a los peers usados en la comparación

1. "An empirical study of testing practices in open source AI agent
   frameworks and agentic applications" (2026) — DOI
   10.1007/s10664-026-10857-9
2. "Securing LLM-in-the-loop software for empirical study of risks,
   mitigations, and utility trade-offs in a safety-critical case"
   (2026) — DOI 10.1007/s10664-026-10820-8
3. "Which design decisions in AI-enabled mobile applications
   contribute to greener AI?" (2024) — DOI 10.1007/s10664-023-10407-7
