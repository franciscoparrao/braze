# Hipótesis: 2×2 Ornith-1.5 vs 1.0 × sampling — transfer, acoplamiento y sensibilidad

Fecha: 2026-08-19
Estado: proposed — commiteado ANTES de bajar el modelo o correr nada
(registro git-only). **En cola**: corre cuando Nitro libere (KV-quant
primero). Autorizado en conversación por el autor ("dale con eso").
Línea: rito de adopción de modelos + auditoría de transfer resistente a
contaminación + chequeo de sensibilidad de operating point
(lourie2026smallscale) + predicción de acoplamiento
(`docs/nota-agencia-propiedad-del-sistema-2026-08-19.md`).

## Preguntas (tres, una por eje del diseño)

1. **Transfer**: ¿las ganancias que Ornith-1.5 reporta en benchmarks
   públicos (TB2.1 47.0, SWE-V 70.6 con 9B) aparecen en nuestras
   suites — privadas por construcción (repo privado, jul-ago 2026),
   con oráculo `cargo check`, calibradas en la frontera (~2.9
   pp/ítem)? Ningún loop de task-generation pudo haberlas visto: si la
   capacidad es real, transfiere; si es benchmark-fitting, no.
2. **Acoplamiento**: ¿el modelo co-entrenado con su scaffold Y su
   currículo (1.5) muestra MÁS preferencias de ruta ajenas a nuestro
   harness (RouteMiss: brecha `passed` − `passed_strict`) que 1.0?
   Predicción de la nota de agencia: el acoplamiento es un costo de
   generalización observable.
3. **Sensibilidad de operating point**: ¿los veredictos (1.5-vs-1.0 y
   los absolutos) son estables entre nuestra temp 0.2 default y la 0.6
   que el vendor recomienda para coding? (La advertencia
   lourie2026smallscale, por fin medida en un caso propio.)

## Diseño

| | |
|---|---|
| Suite | `discriminating.toml` (34 tareas, oráculo `cargo check`) |
| Ejecutor | Ollama en Nitro (comparabilidad con los baselines de 1.0: default 95/95, M1-replicación, SC) |
| Brazos | 5 invocaciones: **A** 1.0@0.2 · **B** 1.5@0.2 · **C** 1.0@0.6 · **D** 1.5@0.6 · **E** 1.0@0.2 (réplica A/A, AL FINAL) |
| Reps | 3 por brazo, `--seed 42` |
| Total | 5 × 102 = 510 corridas (~5-8 h a ritmo ornith ~30-60 s) |
| Timeout | 900 s (regla del tope que no muerde) |
| Env | `BRAZE_OLLAMA_NUM_CTX=32768`, `BRAZE_MAX_TOKENS=12288` (caveat reasoning de la familia: presupuesto generoso, nota Ornith-1) |

**El par A/E es el control A/A** (config idéntica, separado al máximo
en el tiempo): piso de ruido in-sweep de discriminante+Ollama+ornith —
no existe medición previa de ESTE piso (el ~20% conocido es de
KV-host/LocalBackend/gpt-oss). Ningún contraste se interpreta por
debajo de él. MDE se declara al medirlo (lección M6).

## Instrumento (pasos previos, no cuentan como iteración)

1. Obtener GGUF de Ornith-1.5-9B (registry de Ollama si existe;
   si no, GGUF de HF + Modelfile). Registrar digest en la síntesis.
2. Smoke: 1-2 tareas con tools — verifica soporte de tool-calling vía
   Ollama (la lección gemma3:1b), formato XML parseado, y variación
   entre reps. Si 1.5 no soporta tools vía Ollama → se reporta
   no-ejecutable, sin sustitución.
3. Verificar `metadata.grading` (métrica dual activa) — la brecha
   passed/passed_strict es la métrica de acoplamiento.

## Hipótesis y priors honestos

- **H1 (transfer)**: B > A fuera del piso A/E. Prior: transfer parcial
  o nulo en nuestra familia (los claims públicos son extraordinarios y
  nuestro dominio es estrecho — Rust repair); un transfer pleno sería
  noticia mayor.
- **H2 (acoplamiento)**: brecha strict de 1.5 ≥ la de 1.0 (más
  RouteMiss bajo harness ajeno). Prior: direccional a favor, magnitud
  incierta.
- **H3 (sensibilidad)**: el SIGNO del contraste 1.5-vs-1.0 se mantiene
  entre 0.2 y 0.6; los absolutos pueden moverse. Prior: estable (el
  diseño pareado protege), pero es exactamente lo que nunca hemos
  verificado.

## Métricas y análisis

Primaria: pass rate (dual: `passed` oficial Y `passed_strict`),
McNemar exacto pareado por (tarea, rep) para B−A y D−C, Holm entre los
2 contrastes de transfer; tests nivel-tarea (sign/Wilcoxon). H2: tasa
de RouteMiss por brazo (pares passed=true ∧ strict=false), comparación
1.5-vs-1.0 pareada. H3: interacción descriptiva (tabla 2×2) + signo de
contrastes. Secundarias: schema_fail, rescues, rondas, tokens, pass^3.

## Criterios de decisión, pre-registrados

1. **Piso primero**: discordancia A/E define el piso; MDE derivado y
   declarado antes de leer B/C/D.
2. **Transfer real** (B−A ≥ +3 tareas, p<0.05 Holm, fuera del piso, y
   replicado en signo en D−C): 1.5 entra al lineup como candidato
   (sucesión de 1.0 se decide con default.toml + pass^k después — el
   rito completo); sus claims ganan soporte independiente EN NUESTRO
   DOMINIO (así se redacta: no valida sus leaderboards).
3. **Transfer nulo** (|B−A| dentro del piso en ambas temps): hallazgo
   de transfer acotado — "las ganancias públicas de 1.5 no aparecen en
   suites frontera-calibradas fuera de su distribución"; 1.0 retiene
   su lugar; alimenta el ángulo suites-privadas-como-auditoría.
4. **Regresión** (B−A ≤ −3, p<0.05, ambas temps): hallazgo fuerte
   contra el loop de task-generation; se reporta con la matriz
   completa.
5. **H2 se lee siempre** (es descriptiva-comparativa, no de
   promoción): cualquier dirección alimenta la nota de agencia.
6. **H3**: si el signo de 1.5-vs-1.0 FLIPEA entre temps → los
   veredictos del proyecto a 0.2 ganan un caveat medido; se abre (con
   pre-registro aparte) la re-evaluación del default de temp del
   bench. Si estable → el diseño pareado queda defendido con datos.
7. **Sin iteración de tratamiento**; infra >10% invalida el sweep
   (repetir una vez, completo).
8. Este experimento NO decide palancas de harness ni default de
   modelo por sí solo — es auditoría + rito parcial.

## Caveats declarados de entrada

- Dominio estrecho (Rust compile/edit repair): un nulo aquí NO refuta
  capacidad en otros dominios; se redacta como transfer acotado, no
  como fraude.
- Un solo par de modelos, una familia; el resultado habla de ESTE loop
  de self-improvement.
- Ollama no bit-exacto; el pareo y el A/A lo absorben.
- Los claims públicos de 1.5 quedan FUERA del alcance: no los
  reproducimos ni los auditamos — auditamos su implicación de
  generalización.

## Desviación de instrumento (2026-08-20, ANTES de correr): orden INTERCALADO

El § Diseño fijó 5 invocaciones (una por brazo, 3 reps cada una). Se
cambia a **15 invocaciones de 1 repetición en orden round-robin**
(A-s42, B-s42, C-s42, D-s42, E-s42, A-s43, …): mismo diseño, mismos
510 datos, distinto ORDEN de ejecución. Motivo: el incidente del A/B
KV-quant del 2026-08-20 mostró que correr brazo-por-brazo **confunde
deriva temporal del nodo con tratamiento** (timeouts crecientes
f16a 6/5/2 → q4 4/17/17, con el A/A muriendo por OOM al final). El
round-robin reparte cualquier deriva por igual entre los cinco
brazos. Es desviación de INFRAESTRUCTURA/orden, decidida por
metodología antes de existir ningún dato del 2×2 — no toca
tratamiento, suite, seeds, temperaturas ni análisis.

Consecuencias operativas declaradas: (i) sin `--no-ollama-stop` (hay
dos modelos y no caben residentes en 14 GB: 5,6 GB × 2 + KV) → cada
invocación recarga su modelo (~20-30 s, 15 recargas ≈ 8 min de
overhead, aceptable); (ii) el quant de 1.5 es **Q4_K_M, idéntico al
de 1.0** (verificado con `ollama show`), así el contraste es
modelo-vs-modelo y no quant-vs-quant; (iii) el alias local del modelo
nuevo es `ornith:9b-1.5` (copia de `hf.co/ornith-ai/Ornith-1.5-9B-GGUF:Q4_K_M`),
digest a registrar en la síntesis.

## Smoke de instrumento (2026-08-20): PASA, con un hallazgo de costo

Modelo obtenido: `hf.co/ornith-ai/Ornith-1.5-9B-GGUF:Q4_K_M` → alias
`ornith:9b-1.5`, digest **803aeaf6af02**, 6,6 GB (vs 5,6 GB del 1.0
con el MISMO quant Q4_K_M — diferencia de arquitectura/vocabulario,
anotada). Gates: (1) tool-calling vía Ollama **funciona** (7 y 10 tool
calls, sin HTTP 400); (2) métrica dual activa
(`grading: functional-primary+strict-secondary/2026-08-12`, campo
`passed_strict` presente); (3) repeticiones **varían**.

**Hallazgo de costo, declarado antes de medir**: 1.5 tardó **557 s y
703 s** en la tarea de smoke (8 y 10 rondas), contra los ~30-60 s por
tarea que 1.0 promedia en esta clase de suite: **~10-20× más lento**,
consistente con un reasoning model que piensa mucho más largo. Dos
consecuencias que se declaran ahora, no después:

1. **La estimación del § Diseño (5-8 h) queda obsoleta**: el sweep
   real cuesta ~35-40 h (los 6 brazos de 1.5 dominan). Se corre igual,
   resumible por invocación.
2. **El timeout de 900 s censurará más al brazo 1.5 que al 1.0.** Se
   mantiene idéntico para ambos (es entorno, no tratamiento): si 1.5
   pierde tareas por agotar el presupuesto de reloj, eso **es un
   resultado** sobre su viabilidad local-first bajo presupuesto fijo,
   no un artefacto — y se reportará como tal, con la tasa de timeouts
   por brazo en la síntesis. La lectura del transfer (H1) se hará
   sobre `passed` con los timeouts declarados, y además restringida a
   las tareas que ambos brazos completaron, como análisis de
   sensibilidad.

## INCIDENTE (2026-08-20): OOM sostenido — sweep INVÁLIDO y una atribución mía corregida

El sweep se detuvo tras ~8 invocaciones al detectar que **el OOM
killer mató `llama-server` de Ollama** (`ollama.service: Failed with
result 'oom-kill'`, proceso de 12,9 GB de VM) — y no una vez: **8
eventos "Out of memory" en 6 horas**. El síntoma que lo destapó fue
del mundo físico: el escritorio de Nitro dejó de responder (swap
thrashing), reportado por el autor. Los 8 JSONs escritos
(A-s42/s43, B-s42/s43, C-s42/s43, D-s42, E-s42) quedan **inválidos
como medición** y se conservan solo como evidencia del incidente.

**Corrección de una atribución mía, hecha antes de que nadie la
cuestione**: el "hallazgo de costo" del § Smoke — *"1.5 es 10-20×
más lento (557-703 s/tarea), consistente con un reasoning model que
piensa más largo"* — **está confundido con swap thrashing**. El smoke
corrió con la memoria ya comprometida, así que esos tiempos NO
separan "el modelo piensa más" de "el nodo estaba paginando". La
afirmación se retira hasta poder medirla en un nodo sano; lo único
firme es que 1.5 (6,6 GB + KV de 32k) **no cabe en este nodo con la
sesión gráfica abierta**.

**Causa raíz estructural**: 14 GB totales − ~2,5-3 GB de escritorio
(gnome-shell + Chrome abiertos desde ago-17) deja ~11 GB para un
modelo de 6,6 GB cuyo KV a 32k lo lleva a ~12,9 GB de VM. No cabe. Es
la MISMA causa raíz del incidente KV-quant del mismo día (f16b muerto
por OOM): **este nodo está saturado para los experimentos actuales**,
y ambos incidentes son síntomas de eso, no casualidades independientes.

**Condiciones para re-correr** (cualquiera de las dos, declaradas
antes de reintentar): (i) RAM ampliada (2×32 GB en evaluación por el
autor) — la solución de fondo; o (ii) nodo **headless** durante los
sweeps (sesión gráfica cerrada, ~2,5-3 GB liberados) más swap limpio,
que sería suficiente para 1.5 en este contexto. Bajar
`BRAZE_OLLAMA_NUM_CTX` NO es opción: cambiaría el entorno respecto de
los baselines de 1.0 con los que se compara.
