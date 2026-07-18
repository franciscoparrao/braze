# EMSE pre-submission review R2 (2026-07-17) — issue checklist

Fuente: segunda ronda de `/paper-review-emse` sobre `paper/main.tex`
(no-blind, informada por la R1 del 2026-07-13 y su checklist). Review
completa:
`~/vault/journals/emse/reviews-generated/2026-07-17_19-00_braze-harness-paper.md`.
Análisis de respaldo de todas las reparaciones:
`docs/emse-r2-analysis-2026-07-17.md`.

**Veredicto R2**: Major Revision (segundo consecutivo, pero de carácter
distinto a R1: re-análisis, reframing y publicación del artefacto — sin
experimentos nuevos obligatorios).

**Estado tras esta sesión (2026-07-17)**: Issues 1–4 resueltos con
ediciones de análisis/texto/figuras; Issue 5 sigue diferido por decisión
explícita (acciones externas). Manuscrito recompilado limpio (26
páginas, 0 referencias indefinidas, 0 overfull).

## Issues críticos

### 1. Pooling cross-sweep/cross-commit del nulo de 3 vías — ✅ RESUELTO
La comparación headline (solo vs composite vs bare) pooleaba brazos de
archivos de sweep distintos, y el composite cruzaba la frontera de
hardening (`e9b841e` → `ec61f5e`), en tensión con la regla propia
"deltas solo within-sweep".

- [x] Definir qué licencia "within-sweep": nueva redacción del párrafo
      Reproducibility en \S{}setup — deltas solo entre brazos de un
      sweep multi-brazo O entre single-arm sweeps corridos como batch
      same-day/same-commit/same-node; brazos pooled requieren chequeo
      de homogeneidad + sensibilidad same-commit
- [x] Homogeneidad por slice de los 3 brazos pooled: composite 89.5% vs
      88.4% (Fisher p=0.85), solo p=1.00, bare p=0.85 — reportado en
      \S{}curve con cita al análisis
- [x] Sensibilidad same-commit del delta headline: −2.8pp [−8.8,+2.6]
      (vs pooled −2.5pp [−7.5,+2.5]) — point estimate se mueve 0.3pp,
      ninguna conclusión cambia; reportado en \S{}curve

### 2. CIs ignoran clustering por tarea — ✅ RESUELTO
n=95 = 19 tareas × 5 reps tratadas como i.i.d.; Wilson/Newcombe
anti-conservadores donde el argumento depende del ancho del intervalo.

- [x] Metodología: bootstrap por tarea (B=20.000, resampleo conjunto
      entre brazos para deltas) descrito en \S{}setup, con la
      distinción de estimandos (suite fija vs población de tareas)
- [x] Resultado clave reportado en \S{}curve: composite−solo
      cluster-boot [−7.4,+2.1] ≈ Newcombe — el nulo headline ES
      robusto al clustering (los brazos comparten patrón de fallo por
      tarea)
- [x] Resultado incómodo reportado en \S{}external: los deltas que
      involucran al bare loop NO son robustos (bare−solo [−17.5,+11.2];
      composite−bare [−13.3,+15.8]) — el claim "rules out >8pp" quedó
      restringido explícitamente a composite−solo; para el bare, la
      redacción pasó a "directional agreement with wide task-level
      uncertainty"

### 3. Recomendación práctica incoherente con los datos — ✅ RESUELTO
El paper recomendaba componer 1B+lead sin comparar el costo del
composite contra el lead solo; "at a fraction of the 7B's inference
cost" no estaba cuantificado (y resultó falso).

- [x] Costos extraídos de los power sweeps (mismo commit/día/nodo):
      composite 2.779 in / 342 out / 23.9s vs solo 2.910 / 371 / 25.2s
      → ×0.92–0.95: el composite NO es más barato que el lead solo
- [x] Composite vs 7B baseline (cross-sweep, orden de magnitud):
      ×1.16 input, ×4.96 output, ×2.52 wall — el claim "fraction of
      the 7B's inference cost" era falso; **retirado explícitamente**
      en \S{}discussion ("we withdraw it") y eliminado del abstract y
      de \S{}curve
- [x] "Practical implication" reescrito: la recomendación por defecto
      es "run the most capable small model you can serve, alone"; la
      composición queda para regímenes no medidos (lead que no puede
      servir turnos completos, asimetrías de pricing, presupuestos de
      memoria), nombrados como tales

### 4. Framing "monotonic decay" sobrevivía al hallazgo que lo socava — ✅ RESUELTO
- [x] Abstract reescrito: pinned-ceiling primario ("pins the composite
      at the lead's own ceiling regardless of executor scale"), decay
      como corolario
- [x] Contribución 1 reescrita en el mismo orden lógico
- [x] \S{}curve: primer párrafo de resultados retitulado ("The
      composite sits at the lead's own ceiling; the lead's gain decays
      with scale as a corollary"); eliminada la frase "decays exactly
      as a capability-transfer story would predict"
- [x] Caption de Fig. 1 alineado con la misma lectura

### 5. Artefacto no público — ⏸ DIFERIDO (decisión explícita, sin cambio)
Igual que en R1: URL del repo, DOI de Zenodo y los 2 IDs de OSF
Registrations siguen como `\todo`, por decisión del usuario de diferir
las acciones externas. Ejecutables en una sesión futura sin más
redacción (`/zenodo`; textos OSF listos en los dos design docs).

- [ ] Correr `/zenodo` → DOI
- [ ] Crear las 2 OSF Registrations a mano (textos preparados)
- [ ] Publicar el repo y completar la URL

## Issues menores

- [x] **Swing 98→86 del baseline coder entre sweeps** — investigado y
      EXPLICADO: los 13 fallos del baseline coder en planner-ab
      incluyen 10 fallos de transporte (requests que nunca llegaron,
      wall<1s, o streams caídos) del mismo evento de red que contaminó
      el brazo 3B task-list. Neto de transporte: 96.5% vs 97.9% de la
      curva — consistentes. Documentado en \S{}planner y \S{}threats.
      **Consecuencia sustantiva (hallazgo de esta ronda, más allá de lo
      que pidió la review): el "+10pp on the strongest executor" del
      rescate del planner NO sobrevive la exclusión de transporte
      (+1.4pp [−4.5,+7.9]); el único gain agregado demostrable del
      planner es el 3B task-list (+12pp, re-run limpio). El paper, el
      abstract, la Contribución 4, \S{}planner, \S{}discussion,
      la Conclusión y la Fig. 2 se corrigieron: el rescate del ceiling
      se reporta como "harm eliminated", no como gain.**
- [x] **TOST/equivalencia** — nota metodológica en \S{}setup:
      "statistically indistinguishable" queda definido como CI del
      delta cruzando cero, sin margen de equivalencia pre-declarado —
      failures to detect, no demostraciones de equivalencia
- [x] **Abstract ~450 palabras** — reescrito a ~270 palabras,
      conservando el hedge de familia de modelos (compromiso de R1)
- [x] **Captions** — Fig. 1: eliminados los dos "see Section~X"
      literales (ahora \ref{} reales), números del solo actualizados al
      pooled n=285 (91.2% [87.4,94.0]); Fig. 2: eliminada la referencia
      a un "supplementary note" inexistente, números actualizados a los
      corregidos por transporte
- [x] **"n=15 single skill"** — corregido en \S{}setup: n=15 para los
      skills de 3 tareas, n=35 para single_tool (7 tareas)
- [x] **SE framing en la Introducción** — nuevo párrafo argumentando la
      relevancia SE (decisiones de diseño del harness como decisiones
      de diseño de software hoy argumentadas por intuición; los 5
      skills como primitivas de workflows agénticos; harness+bench como
      infraestructura para investigación empírica)
- [x] **Grayscale Fig.1/Fig.2** — verificado por luminancia: vermillion
      #D55E00 (≈119) vs amber #E69F00 (≈162) son distinguibles en
      escala de grises; además codifican en figuras distintas con
      leyendas propias. Decisión: no cambiar (la colisión real, azul,
      ya se resolvió en la ronda anterior)

## Cambios en figuras (regeneradas e inspeccionadas visualmente)

- `paper/R/fig1_curva.R`: banda/línea del solo ahora pooled n=285
  (sweep original + power; homogeneidad citada en comentario)
- `paper/R/fig2_rescate.R`: exclusión de fallos de transporte para
  todos los brazos del sweep AB y su re-run (criterio en comentario y
  en `docs/emse-r2-analysis-2026-07-17.md` § 4); la curva se deja cruda
- Ambos previews inspeccionados: Fig. 2 muestra ahora el coder
  user-role en +1.4pp (cruzando cero) y task-list 3B como único gain

## Verificación

- `make` limpio: 26 páginas, 0 referencias indefinidas, 0 overfull
- Sin restos de claims retirados: `grep` de "+10pp"/"fraction of the
  7B"/"Section~X"/"supplementary" solo encuentra el uso benigno de
  "fraction" en Related Work y la retractación deliberada en
  \S{}discussion

---

# Ronda BLIND b1 (2026-07-17, noche) — issue checklist

Review blind independiente (subagente fresco, sin contexto de R1/R2 ni
de las reparaciones del mismo día; estado de información de un reviewer
externo real). Review completa:
`~/vault/journals/emse/reviews-generated/blind/2026-07-17_23-11_braze-harness-paper_b1.md`.
Análisis de respaldo: `docs/emse-r2-analysis-2026-07-17.md` § 6.

**Veredicto blind**: Major Revision (7 issues críticos; ~4 genuinamente
nuevos respecto a R1/R2). Convergencia con R1/R2 en artefacto, suite,
confound de familia y pre-registro auto-alojado → críticas robustas.

## Issues críticos del blind

### B-1. Artefacto inexistente (convergente con R1/R2) — ⏸ DIFERIDO
Sin cambio: decisión explícita del usuario (Zenodo/OSF/repo).

### B-2. Suite micro + saturación del nulo + circularidad — ✅ RESUELTO (parcial: claims acotados)
- [x] Interacción saturación-nulo reconocida en §threats (con el techo a
      91–98% hay pocos pp de headroom: el nulo de 3 vías está
      parcialmente construido en el instrumento)
- [x] Amenaza de circularidad (developer overfitting: la suite fue el
      target de regresión del desarrollo) nombrada en §threats
- [ ] Anclaje en suite externa (BFCL subset / SWE-bench slice) — sigue
      como future work declarado; requiere experimentos nuevos

### B-3. Tratamiento estadístico inconsistente — ✅ RESUELTO (nivel análisis/disclosure)
- [x] Statement de multiplicidad en §setup + nuevo párrafo
      "Multiplicity (conclusion validity)" en §threats
- [x] Segundo rater: limitación declarada en el párrafo de grader
      validation (hand-grading del autor, sin rater independiente)
- [x] Conteos de exclusión por brazo ahora en el texto (§planner: 10/2/8)
- [ ] Cluster-boot para TODOS los deltas (hoy: headline + restricción
      explícita del claim "rules out >8pp") — extensión posible en una
      pasada futura; el paper ya declara qué conclusiones son
      suite-condicionales

### B-4. Pre-registro: cobertura + deriva del criterio — ✅ RESUELTO
- [x] **Evaluación LITERAL del criterio del planner** (análisis § 6.1):
      target 12/30→17/30 DENTRO del Wilson del baseline; agregado
      +11.6pp [−1.0,+23.7] cruza cero; multi_step regresa 9→7;
      token cost 2×/4× cuenta en contra. **Veredicto literal: ningún
      delivery se adopta.** §planner reescrito con el veredicto literal
      + nota de desviación fechada (2026-07-17: se mantiene el lever
      opt-in en vez de removerlo; el hallazgo diagnóstico es la
      contribución). Abstract, Contribución 4, Fig. 2 caption y
      Conclusión corregidos ("largest movement... marginally crossing
      zero", no "demonstrable gain")
- [x] Cobertura del pre-registro explícita en §setup (curva y
      mechanism-A/B fueron exploratorios; el "pre-registered" del
      título refiere a los criterios de adopción)
- [x] **Título** — cambiado por decisión del usuario (2026-07-18) al
      sugerido por el blind: "Not All Scaffolding Helps: A
      Lever-by-Lever Study of Agentic Harnesses at Small-Model Scales,
      with Pre-Registered Adoption Criteria for Key Levers" — ya no
      implica pre-registro de todo el estudio; consistente con el
      párrafo de cobertura de §setup. Compila limpio (28 pp, título en
      3 líneas)

### B-5. Fig. 1: banda cross-sweep vs disciplina propia — ✅ RESUELTO
- [x] §curve: qualifier explícito (same-batch/within-discipline solo en
      1B; en 3B/7B/ceiling la yuxtaposición cruza sweeps y un commit —
      "visual context, not a tested contrast")
- [x] Caption de Fig. 1 con el mismo qualifier

### B-6. Mecanismo empty-response: explicación serving-layer no excluida — ✅ RESUELTO (rebaja + probe nuevo)
- [x] Probe token-level sobre datos existentes (análisis § 6.2): los
      turnos "vacíos" generan 44–619 tokens que no emergen en ningún
      canal usable; el 1B (sin canal de razonamiento) muestra la misma
      firma → reasoning-budget no explica ambos extremos; contribución
      serving/template no excluida (el fix user-role también la
      arreglaría)
- [x] §planner: mecanismo rebajado a "consistent with" + experimento
      discriminante nombrado como future work; §curve ajustado ("stops
      yielding usable output", no "goes silent"); nuevo párrafo
      "Mechanism claims (construct validity)" en §threats

### B-7. Parámetros de modelos no declarados — ✅ RESUELTO
- [x] Nueva Tabla de modelos en §setup (params/cuantización/rol,
      consultados al server: 1.2B Q8_0 · 3.1B/7.6B/9.7B Q4_K_M ·
      lead 8.0B total ~4B activos MoE — comparable al 7B)
- [x] num_ctx=8192 explícito documentado + chequeo de truncación
      (compaction del engine por debajo del límite; ~7.5K/request del
      full-inventory cabe; la extrapolación a 1.500 tools es el punto
      donde deja de caber)

## Menores del blind

- [x] §searchtools: descripción de la suite corregida (companion suite
      de 6 tareas × 15 reps, tool-search.toml — no "same 19 tasks")
- [x] CI del +12pp impreso en el texto (+11.6pp [−1.0,+23.7])
- [x] refs.bib: notas internas en español ELIMINADAS de los campos
      note (spbasic las tipeaba); movidas conceptualmente a
      docs/verify-refs-2026-07-13.md. Corradini→primario
      (liu2025tts, arXiv:2502.06703). Nuevas entradas:
      jimenez2024swebench, bfcl, wohlin2012experimentation,
      arcuri2011practical (DOIs de Wohlin y Arcuri verificados vía
      Crossref; los arXiv vía el propio registro previo). ✅ /verify-refs
      corrido 2026-07-18 sobre las 5 entradas: wohlin y arcuri VERIFIED
      por el tool (y sin retracciones, 4304/969 citas); liu2025tts y
      jimenez2024swebench verificadas manualmente vía DataCite (título
      y autores exactos) + OpenAlex is_retracted=false — el tool las
      reportó NOT_FOUND por un 400 de OpenAlex con el "?" del título
      (bug de query del tool, no alucinación); bfcl es recurso web (URL
      200, no indexable — falso positivo esperado). Hallazgo lateral:
      el registro OpenAlex del DOI de SWE-bench (W4387561453) tiene el
      título corrupto en OpenAlex (DOI/autores/año correctos) —
      anotado en el header de refs.bib
- [x] Citas colocadas: SWE-bench (intro), BFCL (threats),
      Wohlin+Arcuri (setup, repeticiones/intervalos), liu2025tts
      (related work)
- [x] Fig. 3 caption: outlier ~48K explicado (6 rondas + 5
      compactaciones re-pagando el catálogo); "§ ablations" →
      Section~\ref{sec:searchtools}; n=90 aclarado (6×15)
- [x] Abstract: caveat "(no equivalence margin was pre-declared)"
- [x] §threats reorganizado con tags de taxonomía estándar
      (construct/internal/external/conclusion validity) + 2 párrafos
      nuevos (Multiplicity; Mechanism claims)
- [x] Declarations: Funding + Conflicts of interest agregados
- [ ] RQ1–RQn explícitas — estructural, no aplicado (decisión de
      redacción mayor; anotado)
- [ ] IQR en panel (a) de Fig. 3 y marcadores open/filled en Fig. 2 —
      cosmético, no aplicado en esta pasada
- [x] Abstract "~450 palabras" del blind: dato desactualizado del
      reviewer (el abstract ya estaba en ~300 tras R2); descartado

## Verificación de la ronda

- `make` limpio: 28 páginas, 0 referencias indefinidas, 0 overfull,
  sin warnings de citas
- Convergencia/divergencia estudiada: las críticas convergentes
  (artefacto, suite, familia, pre-registro) quedan validadas como
  robustas; las divergentes (deriva del criterio, banda Fig. 1,
  mecanismo, refs) eran fallas reales omitidas en R1/R2 — el patrón
  esperado del modo blind del protocolo

---

# Ronda /tex-review (2026-07-18) — verificación mecánica post-ediciones

Pasada de razonamiento + verificación mecánica (~40 cifras recomputadas
desde los JSONs crudos) sobre el manuscrito ya revisado por R1/R2/blind.
36 de 40 cifras reproducen al dígito; 4 hallazgos, todos aplicados:

- [x] **Fix 1 [VERIFICADO]** §curve: delta pooled "−2pp [−8,+5]" era
      erróneo (propagado de docs/power-increase sin recomputar) y
      contradecía a su propio párrafo → corregido a −2.5pp [−7.5,+2.5]
- [x] **Fix 2 [VERIFICADO]** Mezcla de métodos de intervalo: los CIs de
      §constrained venían con corrección de continuidad (pipeline
      original) mientras el resto del paper usa Wilson sin CC.
      Recomputados sin CC: llama +12.6 [+0.4,+24.4] (¡el borde inferior
      cambia de signo según método! — ahora declarado como "marginal,
      method-sensitive" con ambos valores, y el veredicto anclado al
      mechanism-check que es method-insensitive); iteración
      [−25.3,−1.6] / [−30.7,−4.1] / [−30.4,−4.5]. §setup declara el
      método único (Wilson sin CC, uniforme). También corregida la
      paráfrasis del criterio (C−B solo requiere >0, no ≥10pp)
- [x] **Fix 3 [JUICIO]** Conclusión: primera oración propagada al
      framing pinned-ceiling ("lifts... by pinning the composite at
      the lead's own ceiling, so its apparent gain decays as executor
      baselines rise"); "identifiable mechanism" → "mechanisms with
      identified — and explicitly bounded — candidates"
- [x] **Fix 4 [VERIFICADO]** §mechanism: el conteo "8 of 95 reactive"
      de respuestas vacías era en realidad 7 empty + 1 timeout (el doc
      del sweep agrupó los 8 run_error) → corregido a "7 of 95
      reactive — an eighth reactive run-error is a timeout"

Verificado sin hallazgos: 8 niveles de la curva, 12 CIs/deltas
solo/composite/bare, 3 brazos mechanism (+latencias exactas
5.5/11.0/32.2s, 7/7 escalaciones, empty 5/0), 4 brazos searchtools
(+ratios 5.35×/6.07× vs "5.4×/6.1×"), planner completo, suma tabla
sweeps (5.102), latencias ceiling (19.8/23.7s), glob/grep 5/5→0/5,
schema 99→0, crossrefs (0 rotos), citas (15/15 resuelven, 0
huérfanas), 3 figuras renderizadas (ejes honestos).

Compilación final: 28 páginas, 0 overfull, 0 refs indefinidas.
