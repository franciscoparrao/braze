# Pre-registro: ¿el serving stack mueve el pass rate de gpt-oss:20b?

Fecha: 2026-09-07
Estado: **PARCIALMENTE CORRIDO (2026-09-13), DIFERIDO a Nitro 32GB.** Brazo
local COMPLETO y válido (82/102); brazo zen INVÁLIDO por OOM-kill de RAM a
46/102 (el muro de 14GB). Sin veredicto H1/H0. Desenlace y desviaciones en
`docs/ab-serving-stack-2026-09-13-DIFERIDO.md`. Retomar re-corriendo solo el
brazo zen sobre 32GB, mismo diseño.
Disparador: hallazgo colateral del A/B pareado Spark-vs-gptoss
(`docs/ab-spark-vs-gptoss-analysis-2026-09-07.md`): gpt-oss:20b marcó
**72/102** en la suite discriminante servido por llama-server (fork
`a698f1c`, `--jinja`, parsing Harmony server-side, vía backend zen) contra
su **59/102** histórico servido por el LocalBackend in-process
(Study 2 del Paper 2, misma suite `3daaf2e779f06c8f`, mismo timeout 900 s,
misma temperatura 0.2). +13 tareas (+22% relativo) para el MISMO modelo.

## Por qué no basta el dato del disparador

La comparación 72-vs-59 está confundida por al menos tres factores:
1. **Binario de braze distinto** (ad35de8+ de septiembre vs el de agosto;
   entre medio entraron P1.1/L-5 y otros cambios del engine).
2. **Grading distinto** (dual vs pre-dual — aunque en los brazos de hoy
   strict = functional, así que el techo de este factor es bajo).
3. **Contexto distinto**: el brazo llama-server corrió con `-c 8192`
   mientras Study 2 usó 32.768 — y en la discriminante el input acumulado
   promedio supera los 20k tokens, así que 8192 pudo TRUNCAR rondas tardías
   en el brazo que aún así ganó (sesgo en contra del ganador, pero sesgo).

El A/B limpio congela el binario y el contexto, y varía SOLO el stack.

## Hipótesis

**H1.** A binario, suite, semilla, contexto y presupuesto idénticos, el
camino llama-server (parser Harmony de llama.cpp + plantilla jinja del fork,
vía backend zen) rinde MÁS pass rate que el camino LocalBackend in-process
(plantilla Harmony propia de braze + parser del backend).

**H0.** No hay diferencia: el delta histórico de +13 se debe al binario de
braze (el harness mejoró entre agosto y septiembre) o al contexto truncado,
no al stack.

Prior honesto: dirección a favor de llama-server por el disparador, pero
confundida; H0 es perfectamente plausible. Ninguno de los dos desenlaces es
malo: H1 redirige la infraestructura del laboratorio; H0 acredita el
trabajo de harness de septiembre y desconfunde el registro.

## Diseño

Dos brazos, **ambos ejecutados EN NITRO** (mismo nodo para inferencia Y
oráculo `cargo check` — hoy el brazo zen corrió el bench desde la máquina
de trabajo; eso también se congela), **secuenciales** (un residente a la
vez), **mismo binario de braze** compilado en Nitro al commit que registre
este documento.

| brazo | serving | comando (apéndice) |
|---|---|---|
| `local` | LocalBackend in-process, GGUF canónico, Harmony de braze | `--backends local:~/models/gpt-oss-20b-MXFP4.gguf` |
| `zen` | llama-server fork `a698f1c` `--jinja`, mismo GGUF, backend zen | server en localhost:8090 + `--backends zen` |

Constantes en ambos brazos: `discriminating.toml` (34 tareas, fingerprint
`3daaf2e779f06c8f`), 3 repeticiones, `--seed 42`, `--temperature 0.2`,
`--task-timeout-secs 900`, **contexto 32.768 y output cap 12.288 en ambos**
(paridad con Study 2; si el KV a 32k no cabe con `-ngl 8`, se baja `-ngl`
hasta que quepa y se registra el valor — la paridad de contexto manda sobre
la paridad de offload, porque el confound #3 es de contexto). GPU layers:
mismo número en ambos brazos (`BRAZE_LOCAL_GPU_LAYERS` = `-ngl`).

Nota sobre la semilla: `--seed 42` fija seed+rep en ambos, pero las
implementaciones de sampling difieren entre stacks, así que la semilla NO
sincroniza los muestreos — el pareo por (tarea, repetición) es pareo de
diseño, no de trayectoria. Se declara para que nadie lea las celdas como
gemelas.

## Métricas

| métrica | rol |
|---|---|
| pass rate (funcional = estricta, verificar que siga así) | primaria |
| McNemar exacto sobre (tarea, repetición) + sign test por tarea (cluster-aware) | inferencia primaria, con la lección del Study 2: se reportan ambos niveles |
| rondas, tokens, wall time | secundarias |
| schema_fail, rescates, fuente del parse (server-side vs escalera) | mecanismo: si H1 gana, debería verse en MENOS fallos de ensamblaje de tool calls en el brazo zen |
| truncaciones/errores de contexto | control del confound #3 |

**MDE declarado**: el piso de ruido de la suite es ~20% de celdas bajo
prompts idénticos; con ~102 pares y ese piso, un McNemar a α=0,05 exige una
asimetría neta de ≥11 pares (~11 pp). El delta del disparador (+13 tareas)
está justo sobre ese umbral: si el efecto real es la mitad, este diseño NO
lo verá — y eso se reportará como "indetectable a n=3", no como ausencia.

## Criterio comprometido antes de correr

- **zen − local ≥ +11 pares netos con McNemar p<0,05 y sign test
  concordante → H1.** El laboratorio adopta llama-server como camino de
  referencia para gpt-oss y se abre issue para portar la diferencia al
  LocalBackend (o deprecarlo para Harmony).
- **Diferencia con p≥0,05 o niveles discordantes (par vs tarea) → H0
  operativo.** El delta histórico se atribuye a binario/contexto; el
  LocalBackend retiene su rol y el hallazgo colateral se cierra como
  "confound de registro, no efecto de stack".
- **local > zen significativo → H1 invertida** (sorpresa): se investiga el
  brazo zen de hoy por truncación de contexto antes de publicar nada.
- Sin iteración: es una medición, no un loop de tuning.

Cláusula anti-racionalización: si sale H0 no se re-corre con más
repeticiones "para encontrar el efecto" sin un pre-registro nuevo que
declare el n y el MDE actualizados.

## Amenazas a la validez, anotadas antes

- Los samplers difieren entre stacks aun a semilla igual (arriba); el pareo
  es de diseño.
- `-ngl` bajo por el KV de 32k puede hacer el brazo zen más lento que hoy;
  la velocidad NO es endpoint de este A/B.
- El fork `a698f1c` no es llama.cpp mainline; si H1 gana, la atribución es
  "este binario de llama-server", no "llama.cpp" en general.
- Costo estimado: ~9 h por brazo (gpt-oss mayormente en CPU) → ~18-20 h de
  Nitro exclusivo. Lanzar con Nitro ocioso y sin sweeps concurrentes.
