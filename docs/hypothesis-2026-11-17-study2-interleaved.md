# Hipótesis: re-corrida intercalada del A/B de `enable_project_memory` (Estudio 2 del Paper 2)

Fecha: 2026-11-17
Estado: registrado — este documento se commitea y se sube a GitHub ANTES de
lanzar el sweep; después del commit solo se agregan las secciones de
Resultados y Decisión.
Línea: paper2 (revisión para EMSE PROMPT-SE), sección C.1 del plan
`paper2/revision-plan-emse-prompt-se-2026-11-06.md`.
Pre-registro original: `docs/hypothesis-2026-08-04-project-memory-ab.md`
(commit `c84e1c3`).

## Por qué se repite

El Estudio 2 original corrió sus tres brazos en secuencia (baseline, después
empty, después seeded). El brazo baseline corrió a una mediana de 100 s por
ronda contra 58 y 61 s de los otros dos, también entre las corridas sin
timeout (89,7 frente a 53,8 y 54,8 s), y tuvo 38 timeouts contra 23 y 21. Con
brazos en secuencia, brazo y momento de la corrida quedan confundidos, y el
estudio no puede medir el efecto de la memoria de proyecto en ninguna
dirección (`docs/paper2-reanalysis-2026-11-06.md`; review blind b1, issues 1
y 2). Esta corrida repite el diseño con los brazos intercalados y con el
entorno registrado, para que el contraste vuelva a ser interpretable.

## Pregunta

Igual que el original: ¿la sección de memoria de proyecto que
`enable_project_memory` inyecta en el system prompt cambia el pass rate de
gpt-oss:20b en tareas de edición reales? Y, como pregunta de validez nueva:
¿desaparece la diferencia entre baseline y el control de mismo prompt cuando
los brazos se intercalan?

## Lo que esta corrida puede y no puede concluir

Igual que el original: mide el efecto del contenido inyectado en una sesión
(el brazo seeded) y el costo de la plomería del hook (el brazo empty); no
mide valor cross-sesión. Puede rechazar la palanca o aportar a favor, no
promoverla.

## Hipótesis

- **H1 (validez, se evalúa primero):** con brazos intercalados, el control de
  mismo prompt (`empty`) no difiere del `baseline`.
- **H2 (efecto):** `seeded` difiere de `baseline` en pass rate.
- **H3 (contenido):** `seeded` difiere de `empty` en pass rate.

**Prior honesto:** H1 se sostiene (el prompt es idéntico por construcción y la
deriva se reparte entre brazos); H2 y H3 nulos o levemente negativos, como en
el prior original. El +13 del original debería desaparecer.

## Diseño

| | |
|---|---|
| Suite | `crates/braze-bench/suites/discriminating.toml` (34 tareas, oráculo `cargo check`). La huella de la suite se registra en la metadata; si difiere de `3daaf2e779f06c8f` (la del original) se declara en el paper |
| Ejecutor | `gpt-oss-20b-MXFP4.gguf` (GGUF canónico), LocalBackend/Harmony, Nitro. SHA-256 `27cd6c432c7672cb812a92f611cf3ba7bbc35928262bb1e1253ff4ee6ae35901` (calculado el 2026-11-19; fecha de modificación del archivo: 2026-07-21, anterior al original, así que es el mismo archivo que usó el Estudio 2) |
| Brazos | `local:<gguf>` · `local:<gguf>+ablate:project-memory` (empty) · `local:<gguf>+ablate:project-memory-seeded` |
| Orden | `--interleave 20261117`: bloques (repetición, tarea) en orden repetición-mayor; dentro de cada bloque, los tres brazos en permutación aleatoria (SplitMix64 + Fisher-Yates, `crates/braze-bench/src/interleave.rs`) |
| Repeticiones | 3, `--seed 42` (semilla de cada repetición = 42 + rep, igual que el original), temperatura 0,2 |
| Total | 306 corridas |
| Timeout | 900 s por tarea (igual que el original) |
| Entorno | `BRAZE_OLLAMA_NUM_CTX=32768`, `BRAZE_MAX_TOKENS=12288`, `BRAZE_LOCAL_FAMILY=harmony`, `BRAZE_LOCAL_KV_OFFLOAD=host` (como el original). Todo queda en `metadata.local_env` |
| Colocación en GPU | Auto-fit de llama.cpp (`common_fit_params`), como el original. Se probó fijar las capas por entorno y no sirve: el auto-fit pone 25 capas en GPU **y** manda a RAM los tensores MoE que no caben, y `BRAZE_LOCAL_GPU_LAYERS=25` sin ese reparto no carga (medido el 2026-11-19). La colocación se neutraliza de otra forma: el modelo se carga una sola vez por invocación y los tres brazos lo comparten (caché de capacidad 1, misma clave), así que no puede diferir entre brazos. Se lanza con la VRAM de la RTX 3050 libre (≤ 100 MiB en uso) y con `RUST_LOG=braze_model=info`, que deja en el log la colocación efectiva (`gpu_layers`, `layers_source`, `kv_placement`); el log se archiva con los resultados. Medida previa con VRAM libre: 25 capas, KV en host, margen 1024 MiB |
| Invocación | Una sola invocación de `braze-bench` con los tres brazos en `--backends`. El original corrió tres invocaciones separadas, una por brazo (cada JSON lista un solo backend), y cada una cargó el modelo con su propio auto-fit |
| Binario | `braze-bench --release --features local-cuda`, compilado en Nitro en un clon limpio (`~/braze-study2`) del commit de este pre-registro, con llama.cpp vía `llama-cpp-2`. El commit queda en `metadata.braze_git_commit`. El harness es posterior al del original (`c84e1c3`): la suite no cambió (`git diff c84e1c3` vacío), pero el engine sí, así que esta corrida no es una réplica exacta del original y se compara consigo misma |
| Nodo | Nitro reiniciado el 2026-11-19 (driver NVIDIA 595.99.02 consistente), sin procesos de usuario pesados, carga promedio < 1 al lanzar. La sesión de escritorio que arranca con el sistema queda abierta y ociosa |
| Registro | `metadata.run_order = "interleaved"`, `interleave_seed`, y `run_log` con inicio y fin de cada fila |

## Comando de lanzamiento

```
cd ~/braze-study2
export LD_LIBRARY_PATH=$(ls -d target/release/build/llama-cpp-sys-2-*/out/build/bin)
export BRAZE_OLLAMA_NUM_CTX=32768 BRAZE_MAX_TOKENS=12288 \
       BRAZE_LOCAL_FAMILY=harmony BRAZE_LOCAL_KV_OFFLOAD=host \
       RUST_LOG=braze_model=info
G=$HOME/models/gpt-oss-20b-MXFP4.gguf
target/release/braze-bench crates/braze-bench/suites/discriminating.toml \
  --backends "local:$G,local:$G+ablate:project-memory,local:$G+ablate:project-memory-seeded" \
  --repetitions 3 --seed 42 --temperature 0.2 --task-timeout-secs 900 \
  --interleave 20261117 \
  --output ~/study2-interleaved/sweep-pm-ab-interleaved-2026-11-19.json \
  > ~/study2-interleaved/sweep.log 2>&1
```

Si se interrumpe, el mismo comando con `--resume`.

## Métricas y análisis

- **Primaria:** pass rate por brazo (el `passed` oficial, timeout = falla,
  como en el original). McNemar exacto sobre celdas (tarea, repetición),
  Holm entre los dos contrastes contra baseline. Las diferencias se cuentan
  en celdas (102 por brazo).
- **Secundarias, registradas ahora:**
  - Por tarea (cluster-aware): test de signo exacto y Wilcoxon de rangos con
    signo exacto por enumeración de signos, ceros descartados, sobre los 34
    conteos por tarea. Se reportan además las variantes de scipy con
    aproximación normal.
  - Timeouts como censura: proporción de timeouts por brazo (Fisher entre
    pares de brazos), y mediana de segundos por ronda por brazo, con y sin
    las corridas que tocaron el tope.
  - Costo: tokens de entrada, rondas y tiempo de pared por corrida, por brazo.
  - Deriva: segundos por ronda a lo largo del `run_log` (una mediana por
    tercio del sweep y por brazo).
- **Efecto mínimo detectable:** con ~23 celdas discordantes, el McNemar
  detecta a α = 0,05 una diferencia de unas 11 celdas (≈11 pp). Efectos más
  chicos quedan fuera de alcance y un nulo se reporta acotado por ese valor.

## Criterios de decisión

1. **Validez de la corrida (antes de leer cualquier contraste):**
   - Fallos de infraestructura (`HarnessError`/`CircuitOpen`) quedan fuera
     del denominador; si superan el 10 % del total, la corrida es inválida y
     se repite una vez completa antes de leer nada.
   - Deriva entre brazos: si la mediana de segundos por ronda de un brazo
     difiere en más de un 20 % de la de otro, se declara y el contraste
     afectado se reporta junto con ese dato.
   - Huecos en el `run_log`: un intervalo de más de 10 min entre el fin de una
     fila y el inicio de la siguiente se declara (suspensión del nodo u otra
     causa); con el intercalado afecta a los tres brazos por igual.
2. **H1, control de mismo prompt:** si `empty − baseline` da McNemar p < 0,05,
   la corrida no sirve para medir la palanca (con prompt idéntico, una
   diferencia significativa es infraestructura); se reporta así y no se leen
   H2/H3 como efecto. El umbral de "más de 2 celdas" del original se abandona:
   con un piso de ruido de ~20 % de celdas que cambian entre corridas, era
   incumplible por diseño, y se declara como desviación respecto del original.
3. **H2/H3, efecto:** señal a favor si `seeded − baseline` ≥ +3 celdas con
   Holm p < 0,05 **y** `seeded − empty` > 0. Daño si `seeded − baseline` ≤ −3
   con Holm p < 0,05. Si no, nulo (lo esperado), acotado por el efecto mínimo
   detectable.
4. **Sin iteración:** una sola pasada. Si la corrida se interrumpe se reanuda
   con `--resume` (el checkpoint conserva las filas), y la interrupción se
   declara.

## Consecuencia para el paper

- Si H1 se sostiene: la corrida intercalada reemplaza al Estudio 2 original
  como evidencia confirmatoria; el original queda en el paper como el caso
  que motivó el intercalado.
- Si H1 falla: el Estudio 2 sale del claim confirmatorio y el paper se apoya
  solo en la réplica de ornith:9b; ambas corridas se reportan.
- Cualquiera sea el resultado, se reporta.

## Una causa candidata del artefacto original (inferida, no verificable)

El original no fijó `BRAZE_LOCAL_GPU_LAYERS`, así que cada una de sus tres
invocaciones decidió cuántas capas poner en GPU según la VRAM libre al cargar
el modelo (`crates/braze-model/src/local/fit.rs`). Si al cargar el baseline la
VRAM estaba más ocupada, ese brazo corrió con menos capas en GPU y más lento
en todas sus filas, que es el patrón observado. La metadata no registró las
capas efectivas, así que esta explicación no se puede confirmar ni descartar
con los datos del original. Esta corrida la neutraliza por diseño (capas
compartidas por los tres brazos en una sola carga, brazos intercalados) y
registra la colocación efectiva en el log.

## Riesgos anotados

- Duración: el original tomó del orden de días de cómputo. Con el
  intercalado no hay forma de cortar antes un brazo, y no se usa
  `--sequential-stop`.
- El caché de modelo del LocalBackend tiene capacidad 1 con clave (GGUF,
  contexto, capas GPU): los tres brazos comparten la carga, así que
  intercalar no multiplica la memoria.
- Los mismos de la suite que en el original (`expect_max_rounds` bajos,
  colinealidad del seed con el prompt).

## Resultados

(pendiente)

## Decisión

(pendiente)
