# Qwen3.5-9B-Kimi-k3-Distilled y Underdog-Saluki-27B en `default.toml` (2026-10-08/09)

**Conclusión.** Ninguno de los dos desplaza a los recomendados. El destilado
Kimi es el mejor de la familia qwen3.5 servida por Ollama (86 %, el más
rápido y sin fallos de infraestructura), pero queda bajo Spark-X2.5-4B y
gpt-oss:20b, que saturan esta misma suite. Saluki-27B IQ2 no es viable en
Nitro: 10 de 57 corridas agotaron el timeout de 300 s.

## Modelos

| | Kimi-distilled | Saluki-27B |
|---|---|---|
| Origen | `khazarai/Qwen3.5-9B-Kimi-k3-Distilled` (Unsloth sobre Qwen3.5-9B; hiperparámetros no publicados), GGUF de `mradermacher` | `ConwayResearch/Underdog-Saluki-27B-1.0`: Qwen3.8-27B denso a 2 bits (IQ2-mix sobre GSQ-RCO de ISTA-DASLab), publicado el 2026-10-08 |
| Datos de entrenamiento | `greghavens/kimi-k3-coding-and-debugging-traces`: ~3.960 filas, 582 trayectorias agénticas (CC-BY-4.0) | — (cuantización, sin entrenamiento) |
| Archivo en Nitro | `hf.co/mradermacher/Qwen3.5-9B-Kimi-k3-Distilled-GGUF:Q4_K_M`, 6,4 GB, digest `ac04e341a48b` | `nitro:~/models/Underdog-Saluki-27B-1.0-IQ2-mix.gguf` (7,9 GB) registrado como `underdog-saluki:27b-iq2` |
| Arquitectura según Ollama | `qwen35`, 9,2B | `qwen35`, 26,9B, IQ2_XS |
| Carga en Nitro | entero en VRAM | 8,4 GB: 4,4 GB en VRAM, el resto en CPU |

Instalación de Saluki: `ollama pull hf.co/…:IQ2-mix` falla ("not a valid
quantization scheme") y `:latest` baja el mmproj y luego falla ("file does not
exist"). Funciona bajar la GGUF con `curl` y registrarla con un Modelfile de
una línea (`FROM ./Underdog-Saluki-27B-1.0-IQ2-mix.gguf`).

Smoke previo (verificado en los rollouts): los dos hacen `read_file` nativo,
sin rescate textual, y responden bien en 2 rondas. Saluki tarda 38 s en
caliente en esa tarea trivial; Kimi, entre 7 y 15 s.

## Sweep de tres brazos (medición válida)

`default.toml` (huella `6694b4c073476ffa`), 3 repeticiones, seed 7, temp 0,2,
timeout 300 s, Ollama 0.32.1 en Nitro, binario `e57e710`.
Datos: `sweep-saluki27b-kimi-qwen35-default-2026-10-09.{json,log}`.

| | `qwen3.5:9b` (control) | Kimi-distilled | Saluki-27B |
|---|---|---|---|
| Pass [IC 95 %] | 39/53 = 74 % [60, 84] | **49/57 = 86 % [75, 93]** | 40/57 = 70 % [57, 80] |
| Estricto | 39/53 | 47/57 | 40/57 |
| pass^3 | 70,6 % | 73,7 % | 57,9 % |
| Mediana por tarea | 25,5 s | **15,2 s** | 106 s |
| Tokens de salida promedio | 291 | 169 | 282 |
| Causas de fallo | 6 archivos, 5 `ModelBackendError` (XML), 3 texto, 3 `HarnessError` | 5 archivos, 2 texto, 1 tool | **10 timeout**, 3 tool, 2 texto, 2 archivos |
| McNemar vs control | — | 1 vs 7, p = 0,070, p_holm = 0,14 | 6 vs 3, p = 0,51 |

### Control: el parser XML de Ollama

Los 5 `ModelBackendError` del control son `XML syntax error … element
<function> closed by </parameter>`: el parser server-side de Ollama falla
con el formato de tool calls de qwen3.5 (clase de bug #1/#17 del proyecto).
Cinco seguidos **abrieron el circuit breaker**, que descartó 3 corridas más
como `HarnessError` (por eso el denominador es 53). Sin esos fallos, el
control queda en 39/48 (81 %).

Hallazgo de harness: el breaker clasifica un error de parseo server-side de
la salida del modelo como fallo de transporte. Con un modelo de parser
frágil, eso pierde corridas que podían pasar.

### Kimi-distilled

- Cero fallos de infraestructura en 114 corridas (este sweep y el del
  08-oct). Ollama sirve este GGUF con la plantilla Jinja del archivo, no con
  el parser XML de qwen3.5 (inferido por el contraste de errores, casi
  seguro).
- Fallos sistemáticos: `multi_step_sum_two_files` 0/3 (el control también),
  `error_recovery_edit_wrong_filename` 1/3.
- 2 RouteMiss en `edit_file_basic` (resuelve con `read_file` + `write_file`
  en vez de `edit_file`), por eso estricto = 47.
- La diferencia contra el control no es significativa, y de los 7 pares a
  favor del brazo, al menos 4 son fallos de infraestructura del control.

### Saluki-27B

10 de sus 17 fallos son timeouts, en tareas de varias rondas
(`multi_step_*`, `edit_file_function_body`, `error_recovery_edit_*`,
`distractor_*`). Excluyéndolos queda en 40/47 (85 %): la capacidad
condicional se parece a la del Kimi, pero el throughput con la mitad de las
capas en CPU lo hace inutilizable en Nitro. Es el mismo patrón que
gemma-4-12B (2026-07-21). Su afirmación de superar al modelo completo
("Underdog Bench", 88 vs 84 de 120) no se puede evaluar aquí.

## Sweep del 08-oct (dos brazos, binario viejo)

`sweep-kimi-k3-distill-vs-qwen35-9b-default-2026-10-08.{json,log}`. Mismo
diseño, pero el binario de braze-bench era del 2026-09-28 (`d3ae344-dirty`,
12 commits detrás de HEAD: sin la regla de prompt "shell → tools" ni la capa
de rarezas por modelo). Kimi 49/57, control 46/57 (2 `ModelBackendError`
XML), McNemar 1 vs 4, p = 0,375. Mismo sentido que el sweep válido.

## Contra Spark-X2.5-4B y gpt-oss:20b

Misma suite (huella idéntica), pero otro día, otro binario y otro stack de
serving:

| Modelo | `default.toml` | Mediana por tarea | Stack | Fuente |
|---|---|---|---|---|
| Spark-X2.5-4B | **57/57** | 5,7 s | llama-server (fork `a698f1c`), GPU completa | `sweep-spark-x25-default-r3-2026-09-06.json` |
| gpt-oss:20b | **57/57** (LocalBackend) / 98,9 % (Ollama, n=95) | 41 s / 13 s | Harmony in-process / Ollama | CLAUDE.md § LocalBackend; sweep del 13-jul |
| Kimi-distilled | 49/57 | 15,2 s | Ollama, GPU completa | este documento |

- **Verificado:** en esta suite, Spark y gpt-oss no fallan ninguna corrida;
  Kimi falla 8 de 57, 3 de ellas en un ítem que reprueba siempre.
- **Inferido (probable):** en la suite discriminante Kimi quedaría también bajo
  Spark (70/102) y gpt-oss (72/102). Un modelo que no satura la suite fácil
  difícilmente iguala a los que sí la saturan en la difícil. No está medido.
- **Supuesto:** que el stack de serving no explique toda la brecha. Spark
  corre en llama-server con su plantilla jinja y gpt-oss por Harmony
  in-process; Kimi pasa por Ollama. El A/B del 07-sep mostró que el stack
  puede mover +13 tareas en gpt-oss.

## Qué se puede concluir del Kimi-distilled

1. No reemplaza a Spark-X2.5-4B (más chico, más rápido, satura la suite) ni
   a gpt-oss:20b como recomendado del proyecto.
2. Si hay que usar la familia qwen3.5-9B por Ollama, conviene más que
   `qwen3.5:9b`: es ~1,7× más rápido, gasta ~40 % menos tokens de salida y
   no tropieza con el parser XML de Ollama. La ventaja en pass rate no es
   significativa.
3. La destilación sobre 582 trayectorias agénticas no muestra señal de
   capacidad con n=57. Sí muestra estilo: menos rondas y menos tokens.
4. No apareció el sesgo hacia el shell que se temía por el origen de las
   trazas (ls, grep, cat): estricto = 47 de 49 aprobadas.
5. Procedencia opaca (hiperparámetros no publicados, solo métricas internas):
   si se cita, declararlo, como con ornith:9b.

## Pendientes

- Si se quiere cerrar la comparación: Kimi en `discriminating.toml` ×3, o
  Kimi por LocalBackend (ChatML/qwen) para sacar a Ollama de la ecuación.
- Revisar la clasificación del circuit breaker para errores de parseo
  server-side.
