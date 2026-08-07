# Cinco técnicas para mejorar braze — roadmap comprometido (2026-08-06)

Estado: comprometido para seguirse **al pie de la letra** (pedido del autor,
2026-08-06). Cada técnica entra por el pipeline del framework de disciplina
(`docs/research-discipline-framework-2026-07-16.md`): idea → pregunta →
hipótesis → implementación mínima → ablación → benchmark → decisión. Ninguna
se promueve sin su propio experimento; los priors quedan declarados ANTES de
correr nada.

Ranking por (impacto en cuellos reales × costo × medibilidad con la
maquinaria existente).

---

## 1. Inferencia anytime-valid (e-values / SPRT secuencial) — PRIMERA

**Cuello que ataca**: el costo de reloj de los sweeps (13.5h por brazo en el
A/B de project-memory; 27h para que un gate dijera "detente"). Hoy se corre
n fijo y se analiza al final porque el McNemar clásico no permite mirar antes.

**Técnica**: e-processes (Ramdas et al.) — martingalas de apuesta sobre los
pares discordantes que dan p-values válidos bajo monitoreo continuo. Parar
cuando el criterio pre-registrado ya está decidido, sin inflar α. Complemento
SPRT con efecto declarado para poder **aceptar H0 temprano** (el e-process
solo rechaza).

**Experimento mínimo**: retrodicción offline sobre los sweeps existentes en
`docs/` — ¿cuántas horas habría ahorrado cada sweep bajo un e-process con los
mismos criterios? Cero GPU.

**Predicción (prior, declarado antes de correr la retrodicción)**: 30-50% de
ahorro mediano sin cambiar ninguna decisión histórica.

**Si valida**: el e-process entra al bench como criterio de corte de primera
clase (opt-in, `--sequential-stop`), ANTES de los pilotos de round-economics.

## 2. Teoría de respuesta al ítem (IRT) para las suites

**Cuello**: `discriminating.toml` se construyó a mano ("2.9pp por ítem"). IRT
(Rasch/2PL) lo formaliza: dificultad y discriminación por tarea, habilidad
por modelo, estimadas de las corridas históricas que ya están en `docs/`.

**Experimento mínimo**: ajustar 2PL sobre la matriz (tarea × modelo × pass)
histórica; seleccionar subset por información de Fisher; verificar que el
subset reproduce el ranking de brazos de los sweeps históricos completos.

**Predicción**: un subset de 12-15 tareas reproduce el ranking de la suite de
34 en los sweeps históricos. Combinada con la #1: costo por A/B cae 3-4×.

**EJECUTADA 2026-08-07 — resultados en `docs/irt-suites-2026-08-07.md`.**
La reducción de suite **NO se adopta**: k=12 da Spearman medio 0,949 pero el
ranking exacto solo se preserva en 6/13 sweeps, y la técnica #1 ataca el
mismo cuello (costo de reloj) sin perder información. Lo que SÍ sale: (a) una
corrección de nomenclatura — `default.toml` tiene **19 ítems**, no 57 (el
"57/57" son corridas, 19×3); (b) el diagnóstico de discriminación como
chequeo rutinario de salud de banco — `read_file_basic` tiene a=0,44 y
anti-correlaciona con el tamaño del modelo (7B 12% vs 3B 46%), y resultó
estar midiendo los errores de transporte de Ollama 0.30.7, no capacidad. Un
ítem con a≈0 detecta contaminación de banco sin leer una transcripción.
Autocorrección de método: el primer ajuste (JML) degeneró con `a` en el tope
en 18/19 ítems; se rehízo con MML.

## 3. Rescate de ediciones por alineamiento con certificado de unicidad

**Cuello**: la región estructuralmente ineditable (hallazgo U+1D62,
`docs/roam-metrics-memoria-2026-07-28.md` § 7). `first_divergence` nombra el
carácter; el modelo igual no puede emitirlo.

**Técnica**: alineamiento de secuencias (Needleman-Wunsch; Hirschberg si pesa
la memoria) entre `old_string` y el archivo. Aceptar el match si (a) el
alineamiento es ÚNICO dentro de distancia k, y (b) las discrepancias caen en
la clase de caracteres que el modelo demostradamente no puede emitir. La
edición se aplica con el texto DEL ARCHIVO, no del modelo. Se loguea como
peldaño nuevo de la escalera de rescate (filosofía validada por el resultado
del stencil: la recuperación harness-side le ganó al control del decoder).

**Experimento mínimo**: replay del fallo de roam (preservado) + tests de
mutación para acotar falsos matches.

**Predicción**: convierte el rechazo honesto de 4 rondas en edición exitosa
de 1, con cero falsos positivos a distancia k=2 sobre la clase conocida.

**EJECUTADA 2026-08-07 — implementada y verificada.** Peldaño 4 de la
escalera en `edit_file.rs`. El certificado quedó en cuatro cláusulas (solo
borrados / solo no-ASCII / match único / acotado a 8 borrados y ≥40 chars),
más la direccionalidad: el peldaño corrige DÓNDE matchea, nunca CON QUÉ se
reemplaza. Replay del caso real de roam: recuperado. Cuatro mutaciones que
acotan falsos positivos (omisión ASCII, sustitución, dos regiones
candidatas, `old_string` corto): las cuatro rechazadas, archivo intacto en
todas. Se apaga con `strict` (`+ablate:strict-edit`), o sea es ablacionable
por el bench. Nunca silencioso: el resumen de éxito advierte cuántos
caracteres faltaban y que `new_string` probablemente también los omite.
Prior cumplido; la predicción de "distancia k=2" quedó corta — el caso real
necesitaba 3 borrados, y el límite se fijó en 8. 1.125 tests verdes.

## 4. Predicción conformal para la escalación al lead

**Cuello**: `lead_turns`/`failure_threshold` son heurísticas reactivas fijas.
Bandits/RL (Paper 3) es la solución cara y lejana.

**Técnica**: calibración conformal de un score de no-conformidad (schema
failures, rondas vacías, conteo de rondas) sobre corridas logueadas →
"escala cuando P(éxito local) < τ" con garantía de cobertura
distribution-free. Sin RL, sin entrenamiento.

**Experimento mínimo**: calibrar sobre los sweeps con lead existentes,
evaluar en holdout: mismo pass rate del compuesto con menos llamadas al lead
que la heurística actual.

**Predicción**: reducción de llamadas al lead a igual pass rate. Además
genera los features que el framework ya pedía guardar para bandits futuros.

## 5. Diseños factoriales fraccionados (DoE) para screening de knobs

**Cuello**: el gate de metaheurísticas — NSGA-II sobre 15 knobs es carísimo
antes de saber qué knobs importan.

**Técnica**: Plackett-Burman o fraccionado resolución IV — efectos
principales de los 15 knobs en ~32 corridas. Alimenta el factorial de
round-economics con los knobs que sobrevivan.

**Experimento mínimo**: screening sobre `discriminating.toml` (o el subset
IRT de la #2) con un modelo no saturado.

**Predicción**: ≤4 knobs concentran los efectos principales detectables.

---

**Descartadas por ahora** (con razón): beta-binomial jerárquico para
flakiness (pass^k ya cumple ese rol), selección submodular para compactación
(necesita embeddings; prior débil tras el nulo del lead-summary).

**Orden de ejecución comprometido**: 1 → (2 y 3 en paralelo, son
independientes) → 4 → 5. La #5 además está gateada por el ordenamiento ya
registrado en el framework (round-economics antes que metaheurísticas).
