# Plan de revisión — Paper 2 (IST), tras /paper-review-ist

Fuente: `~/vault/journals/ist/reviews-generated/2026-09-08_11-00_paper2-amortization.md`
(Major Revision; 5 críticas, 3 reject-level). Manuscrito: `paper2/main.tex` (elsarticle).

Todos los reject-level son **reframe/prosa** — no requieren sweeps nuevos.
Dos de los medios se fortalecen con **análisis de datos ya commiteados** (sin
compute nuevo). Uno (Issue 4) admite una medición barata opcional, ahora
factible en Nitro (zen-ready).

---

## Tier 1 — Reject-level (hacer primero; solo prosa, sin datos nuevos)

### Issue 1 — Encuadre de scope para IST
**Crítica:** el centro de masa es la economía de tokens de un loop agéntico;
el único criterio formal de entrada de IST es "componente claro de SE / mejora
de la práctica de desarrollo". Riesgo #1 de desk-reject (7 días).
**Dónde:** Introduction §1 (l. ~113-140), gancho de §2 "The harness as an
engineering object" (l. ~270).
**Qué:**
- Reencabezar la intro con la decisión del practitioner: *"¿debe un harness de
  coding-agent cargar una sección de memoria?"* — antes de la mecánica ML.
- Posicionar la condición de amortización como **criterio de aceptación de
  diseño** que practitioners y harness-optimizers aplican.
- Adelantar/reforzar el marco "harness engineering como práctica de SE" al
  primer o segundo párrafo (hoy vive en §2).
**Esfuerzo:** reescritura del arranque de intro + 1-2 oraciones. Sin datos.
**Nota para cover letter (no .tex):** justificar Research Article (no Short
Communication) por la sustancia (formalización + 2 estudios + réplica +
instrumento). Va en `/paper-submit`.

### Issue 2 — Validez de constructo de la métrica bajo prefix caching
**Crítica:** la contribución nombrada es una frontera de *tokens*, pero §3
admite que el KV caching vuelve c×R un costo de contabilidad/ocupación, no de
recómputo, y Study 1 no muestra cambio confiable de wall-time en las fresh
tasks. El costo que duele (conductual) solo aparece en el 9B y NO es lo que
tarifa la Eq. 1. El lente de medición del EIC (Staron) presiona acá.
**Dónde:** §3 (Amortization Condition), 2º párrafo (l. ~330-353); framing de
Results §5.1 y del abstract donde se titula el net-tokens.
**Qué:**
- Elevar el caveat de §3 de hedge a **scoping preciso**: qué costo operativo
  gobierna la frontera y bajo qué supuestos de serving.
- Reconciliar explícitamente que la cantidad tarifada (tokens) y el daño
  observado (conductual, solo 9B) son parcialmente disjuntos: Eq. 1 = criterio
  de aceptación en tiempo de diseño para el costo de contabilidad/ocupación; el
  riesgo conductual es el operativo bajo caching.
- **Opcional (fortalece):** cuantificar el costo de ocupación KV (MiB que ocupa
  la sección durante el turno) para hacer concreto el "occupancy cost" en vez
  de argumentarlo. Dato derivable de la config, sin sweep.
**Esfuerzo:** afinar prosa en §3 + 1 oración en §5.1/abstract. Sin datos (u
opción de 1 número derivado).

### Issue 3 — El 3-6× descansa en el piloto exploratorio
**Crítica:** el 3-6× y los números content-rich vienen del piloto M1, cuya
pre-registración los autores mismos declaran NO verificable (§4.5); el peso
confirmatorio está en Study 2 (content-thin) + réplica. La condición
exploratoria se declara en §4.5 pero es invisible en abstract y contribuciones.
**Dónde:** abstract (l. ~66-100, el "$3$--$6$"), contribución 2 (§1, l.
~176-199), §5.1.
**Qué:**
- Donde sea que aparezca el 3-6× / content-rich fuera de §4.5, **agregar el
  calificador "exploratorio"** y que el peso confirmatorio recae en Study 2 +
  réplica.
- No es posible pre-registración verificable retroactiva (la propia review lo
  nota) → el camino es etiquetado honesto.
**Esfuerzo:** calificadores en ~3 lugares. Barato. Sin datos.

---

## Tier 2 — Fortalecer con análisis de datos ya commiteados

### Issue 5 — Mecanismo "reminder, not teacher" aseverado informalmente
**Crítica:** §6.1 fundamenta el claim central (el modelo *sigue* el playbook
pero no converge más rápido) en "preserved sessions", sin N, sin esquema de
codificación, sin proxy cuantitativo.
**Dónde:** §6.1 (disc-anticorr, l. ~836-856).
**Qué:** respaldar con análisis de las trazas ya commiteadas (no sweeps
nuevos): declarar N trayectorias inspeccionadas + un **proxy cuantitativo**
—p.ej. rondas-hasta-primer-edit-correcto con vs sin playbook en fresh tasks, o
conteo de "playbook seguido pero sin ronda ahorrada". Agregar 1 oración/tablita.
**Esfuerzo:** análisis ligero de los JSON de runs existentes + 1 oración. Medio.

### Issue 4 — Piso de ruido: calibración de suite vs no-determinismo de serving
**Crítica:** §5.3/Discussion atribuyen el ~20% de flips a la suite cerca de la
frontera, pero §7 admite que el stack no es bit-exact bajo seed fijo. Son causas
distintas con generalidad distinta (si es no-determinismo de serving, la
aritmética de MDE es del stack, no de la suite).
**Dónde:** §5.3 (l. ~799-805), §6.3 (disc-control), Threats §7.
**Qué:**
- **Prosa (mínimo):** desenredar las dos explicaciones, reconocer el
  no-determinismo del stack como co-causa, y acotar la lección "todo A/B necesita
  control same-prompt in-sweep" para que no sobre-atribuya el mecanismo de
  frontera.
- **Opcional (cierra el issue):** medir el flip rate a **temp 0** (sampling
  determinista) o en la suite saturada `default.toml` bajo el mismo stack —
  separa suite-cerca-de-frontera de no-determinismo. **Ahora factible en Nitro
  (zen-ready).** ~1 sweep chico.
**Esfuerzo:** prosa (barato) o prosa + 1 sweep chico (medio, ya factible).

---

## Tier 3 — Menores (baratos)

- **Abstract estructurado:** reformatear a Context/Objective/Method/Results/
  Conclusion — los 3 peers de IST usan ese formato. Prosa. (abstract §)
- **MDE a-priori de Study 1:** agregar 1 oración con la MDE / justificación de
  n=20. (§4.3 method-m1)
- **Procedencia de ornith:9b:** 1 oración de por qué esos dos ejecutores son
  representativos del régimen sub-10B. (§4.3)
- **Higiene de bib:** completar `pages` en `chhikara2025mem0`, `shinn2023reflexion`,
  `yang2024sweagent` (refs.bib) — warnings de bibtex.
- **Contribución 3 (A→B→H):** suavizar a "diseño + instanciación parcial" (el
  tutor vivo es future work). (§1 contribución 3)

---

## Secuencia sugerida

1. **Tier 1 completo** (Issues 1-3): reescritura de prosa, alto impacto para
   sobrevivir triage + review. Sin compute. ~1 sesión.
2. **Tier 3** de paso (baratos, mientras se toca cada sección).
3. **Issue 5** (análisis de trazas existentes) + **Issue 4 prosa**.
4. **Decisión:** ¿correr el sweep opcional de Issue 4 (temp 0 / default) en
   Nitro para cerrar la atribución del piso de ruido con dato? Es el único
   compute nuevo y es opcional.

Ninguna de las 5 críticas exige re-hacer los estudios; el 80% es prosa.
