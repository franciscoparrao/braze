# Hipótesis: A/B de KV-cache cuantizado en el LocalBackend (f16 vs q8_0 vs q4_0)

Fecha: 2026-08-16
Estado: proposed — este documento se commitea ANTES de lanzar cualquier
sweep (registro git-only, convención del proyecto). **En cola**: corre
cuando Nitro quede ocioso (SC-retention va primero).
Línea: palancas del LocalBackend / economía de memoria; alimenta la
discusión pública sobre "KV q4_0 gratis" (posts de la escena local-LLM
que recomiendan `-ctk q4_0 -ctv q4_0` sin medir su costo en calidad
agéntica — origen de esta pregunta: crítica del 2026-08-16 a un post
sobre Qwen 3.8 27B en 8 GB).

## Pregunta

¿Cuánto cuesta, en pass rate y en fallos de tool-calling, cuantizar el
KV cache del LocalBackend — y es `q8_0` efectivamente gratis (la mitad
de la memoria KV sin costo medible) como para volverlo default?

Nadie que sepamos ha medido esto con oráculos objetivos (`cargo
check`) sobre tareas agénticas; la evidencia circulante es perplexity
y vibes. El LocalBackend + la suite discriminante + la maquinaria
estadística del proyecto lo permiten sin escribir código nuevo.

## Mecanismo bajo prueba

`BRAZE_LOCAL_KV_TYPE` (env-only, capa declarada en
`metadata.local_env` desde v9 L-1) fija el tipo del KV cache de
llama.cpp para K y V juntos; default `f16`. La cuantización del KV
degrada la atención de forma acumulativa con el largo del contexto —
y un loop agéntico es exactamente el régimen de contexto creciente
(5-7 rondas, 13-19k tokens de entrada acumulados en la suite
discriminante). La expectativa de la comunidad llama.cpp: K es más
sensible que V; q4_0 en K daña, q8_0 es seguro. Sin medición agéntica
publicada.

**Fuera de alcance declarado**: brazo mixto q8-K/q4-V (requeriría
separar el knob en dos — cambio de código; este A/B usa la palanca
existente tal cual). Si q4_0 daña y q8_0 no, el mixto queda como
seguimiento natural.

## Diseño

| | |
|---|---|
| Suite | `discriminating.toml` (34 tareas, oráculo `cargo check`, ~2,9 pp/ítem) |
| Ejecutor | `gpt-oss:20b` GGUF canónico (`~/models/gpt-oss-20b-MXFP4.gguf`), LocalBackend/Harmony, **en Nitro por SSH** (inferencia in-process del bench) |
| Brazos | `f16-a` (baseline) → `q8` → `q4` → `f16-b` (control A/A) — 4 invocaciones separadas de braze-bench, una por valor de `BRAZE_LOCAL_KV_TYPE` (env-only ⇒ no puede variar dentro de un sweep); mismo binario, misma suite, `--seed 42`, temp 0.2, 3 repeticiones, timeout 900 s |
| Orden | secuencial con `f16-b` AL FINAL — el par A/A queda separado al máximo en el tiempo, así su discordancia captura también deriva del nodo, no solo sampling |
| Total | 4 × 102 = 408 corridas |
| Env tier | el mismo del A/B de project-memory (`BRAZE_MAX_TOKENS=12288`, `BRAZE_LOCAL_FAMILY=harmony`; verificar en el smoke que `BRAZE_LOCAL_KV_TYPE` queda registrado en `metadata.local_env`) |

**El par f16-a/f16-b es el control del mismo-config**: aplica el
instrumento del Paper 2 — el piso de ruido de esta configuración se
mide DENTRO del experimento, no se importa. Todos los contrastes
tratados se leen contra ese piso.

## Hipótesis

- **H1 (daño q4, direccional)**: `q4_0` reduce el pass rate y/o
  aumenta `schema_validation_failures`/`rescued_tool_calls` más allá
  del piso A/A. Mecanismo esperado: atención degradada en contexto
  largo → tool calls malformadas y ediciones erradas en las rondas
  tardías.
- **H2 (q8 gratis, nulo esperado)**: `q8_0` es indistinguible de
  `f16` en todas las métricas primarias.

**Prior honesto**: H1 probable pero de magnitud incierta (el efecto
podría quedar bajo el MDE — ver abajo); H2 es lo esperado. Un doble
nulo ("hasta q4 es gratis en este régimen") sería noticia y matizaría
nuestra propia crítica pública — se publica igual.

## Métricas

Primaria: pass rate por brazo; McNemar exacto sobre pares (tarea,
repetición) vs `f16-a`, Holm entre los 3 contrastes
(q8, q4, f16-b); **además tests a nivel tarea** (sign/Wilcoxon sobre
los 34 conteos por tarea — la lección R3a: las repeticiones de una
tarea no son pares independientes). Secundarias/mecanismo:
`schema_validation_failures`, `rescued_tool_calls`, rondas,
input_tokens, walltime, pass^k. El ahorro de memoria no se
instrumenta: es aritmética de llama.cpp (q8 ≈ ½, q4 ≈ ¼ del KV f16) y
no está en disputa; lo que está en disputa es el precio en calidad.

## MDE declarado (lección M6 del Paper 2)

Si el piso A/A de esta config se parece al medido en la misma suite
vía KV-host (~20% de celdas, ~21 pares discordantes), un McNemar
exacto a α=0.05 exige asimetría neta ≥ ~11 pares (~11 pp) — a 3
repeticiones, un daño menor que eso es invisible. Este A/B es un
*screening*: puede confirmar daño grande o acotar el daño a <11 pp;
no puede excluir daño fino. Se declara y no se persigue significancia
inflando n a posteriori.

## Criterios de decisión, pre-registrados

0. **Gate de instrumento (se evalúa primero, con el smoke y f16-a)**:
   si las repeticiones dentro de un brazo son COPIAS idénticas
   (determinismo greedy — la trampa v9 L-9), se aplica la cláusula de
   instrumento ANTES de leer brazos tratados: `BRAZE_LOCAL_TEMP>0` y/o
   semillas por repetición, re-lanzando desde f16-a. Es iteración de
   instrumento (una vez), no de tratamiento.
1. **Piso A/A**: la discordancia f16-a vs f16-b define el piso. Ningún
   contraste tratado se interpreta por debajo de él.
2. **q8 nulo Y q4 daño** (≤ −3 tareas, p<0.05 Holm, fuera del piso, y
   dirección consistente en nivel-tarea): `q8_0` se vuelve
   **default recomendado** del LocalBackend en la doc (mitad de KV
   gratis), `q4_0` queda documentado como NO-gratis con el número —
   el resultado que la escena local-LLM no tiene.
3. **Doble nulo**: se documenta "KV-quant sin costo medible hasta q4
   en este régimen (tareas ≤~19k tokens, MDE ~11 pp)" — con el MDE
   como cota honesta, no como absolución.
4. **q8 daña**: f16 queda default, hallazgo fuerte contra toda la
   práctica circulante; pedir replicación antes de generalizar.
5. **Sin iteración de tratamiento**; fallos de infraestructura fuera
   del denominador, >10% del total invalida el sweep (repetir una
   vez, completo).

## Comandos exactos (por brazo, en Nitro por SSH; fecha al correr)

```
# brazo f16-a (y f16-b idéntico, output -b):
BRAZE_MAX_TOKENS=12288 BRAZE_LOCAL_FAMILY=harmony \
  braze-bench crates/braze-bench/suites/discriminating.toml \
  --backends "local:~/models/gpt-oss-20b-MXFP4.gguf" \
  --repetitions 3 --seed 42 --task-timeout-secs 900 \
  --output docs/sweep-kv-quant-f16a-<fecha>.json

# brazo q8 / q4: idéntico con
BRAZE_LOCAL_KV_TYPE=q8_0   # → docs/sweep-kv-quant-q8-<fecha>.json
BRAZE_LOCAL_KV_TYPE=q4_0   # → docs/sweep-kv-quant-q4-<fecha>.json
```

Smoke previo (no cuenta como iteración: chequeo de instrumento): 1
tarea × f16 y × q4_0 — verifica que el env queda en
`metadata.local_env`, que el backend arranca con KV cuantizado, y que
las repeticiones varían (gate 0).

## Riesgos anotados

- El binario con feature `local` debe compilarse EN Nitro (regla del
  proyecto: no benchear con la máquina de trabajo cargada; receta de
  build en el design doc del LocalBackend).
- q4_0 podría fallar de carga con Harmony/MXFP4 en alguna combinación
  de llama.cpp — el smoke lo caza; si no carga, el brazo se reporta
  como no-ejecutable, no se sustituye por otro quant.
- 408 corridas × ~60-150 s ≈ 7-17 h de Nitro: correr de noche;
  script resumible por brazo (patrón de los sweeps de agosto).
- CPU puro (config del 57/57) para reproducibilidad; el efecto del KV
  cuantizado con offload GPU parcial queda fuera de alcance (anotar
  si algún día se mide: `BRAZE_LOCAL_KV_OFFLOAD` existe).

## Relación con otras líneas

- **Paper 2 / round-economics**: el KV es el otro término de la
  economía de contexto — el paper midió el precio de los tokens
  re-enviados; esto mide el precio de comprimir su cache.
- **Crítica pública 2026-08-16**: si H1 confirma, el proyecto tiene el
  número que la recomendación `-ctk q4_0 -ctv q4_0` de la escena
  omite; si doble nulo, la crítica se matiza con datos propios.
- **LocalBackend como laboratorio**: tercera demostración del
  argumento estratégico (sampler/KV access que ningún harness
  API-bound tiene) tras stencil y ablaciones por token.

## Resoluciones de instrumento (2026-08-18, del smoke, ANTES de leer ningún brazo)

El smoke pre-registrado cazó tres cosas; se resuelven aquí, antes de
la medición:

1. **Contradicción interna del pre-registro**: el tier decía "el mismo
   del pm-ab" y los riesgos decían "CPU puro con GPU_LAYERS=0". Se
   resuelve a favor del tier pm-ab (autofit; aunque con el binario
   `local` sin CUDA el resultado es CPU igual — la GPU queda para un
   futuro build `local-cuda`).
2. **Gate 0 (L-9) DISPARÓ**: repeticiones idénticas con semilla fija
   (sampler determinista del LocalBackend). Cláusula aplicada como
   estaba pre-registrada: **semillas por repetición** —
   `BRAZE_LOCAL_SEED` es por-proceso, así que cada (brazo, rep) corre
   como invocación separada: 12 invocaciones, seeds 42/43/44,
   pareo por (tarea, índice-de-rep) en el análisis. Verificado en
   smoke: seeds distintas → trayectorias distintas.
3. **`BRAZE_LOCAL_REASONING=low` para los 4 brazos**: a reasoning
   medio (default) el canal analysis de Harmony quema el presupuesto
   de reloj en CPU y la tarea de smoke muere a 900 s en TODAS las
   condiciones (timeout-floor: el instrumento no mide); a low, la
   misma tarea PASA en 567 s. La desviación aplica idéntica a los 4
   brazos (no toca el tratamiento KV) y se decide por conducta del
   instrumento, no por resultados. Consecuencia declarada: el
   baseline f16-a de este A/B no es comparable con el 57.8% del
   pm-ab (que corrió a medium) — el A/B es autocontenido y su
   referencia es f16-a.

Estimación de duración corregida: ~400-600 s/corrida × 408 ≈ 45-65 h
(la del pre-registro, 7-17 h, era optimista — el ritmo real del
pm-ab en este hardware siempre fue ~10 min/corrida).
