# Registro de prueba de usabilidad — braze

**Fecha**: 2026-07-07
**Backend/modelo probado**: `ollama:qwen3.5-coder` en Nitro (`http://192.168.1.8:11434`)
**Commit de braze**: 9f21eb3 (previo al fix de este hallazgo)
**Modo**: TUI

## Registro

| # | Escenario | Qué esperaba | Qué pasó | Severidad | Sesión (`tools/braze_sessions.py show <id>`) |
|---|-----------|--------------|----------|-----------|-----------------------------------------------|
| 1 | Prompt libre: "¿Puedes revisar el hardware de este computador y hacer un informe en Markdown y guardarlo en esta carpeta?" | El modelo corre `lscpu`/`lsmem`/`lshw`, escribe el informe, y confirma con una respuesta de texto. | Corrió las 3 herramientas de shell y `write_file` correctamente (el archivo `hardware_report.md` quedó bien escrito), pero el turno terminó con `error: model's response had no text and requested no tool calls` — la ronda siguiente al `write_file` volvió completamente vacía. | Bloqueante (el error hace pensar que la tarea falló cuando en realidad se completó) | (sesión no capturada por id en este registro — ver hallazgo U-1 abajo) |
| 2 | Revisión del contenido de `hardware_report.md` una vez generado. | Un informe correcto y completo a partir de la salida real de `lscpu`/`lsmem`/`lshw`. | Buena estructura y buen cruce de fuentes (compara RAM de `lsmem` vs. `lshw`), pero con 3 defectos de contenido — ver U-2/U-3/U-4 abajo. | Molesto (el informe es usable pero no confiable tal cual) | — |
| 3 | Los 3 `shell_exec` de solo lectura (`lscpu`/`lsmem`/`lshw`) para una sola petición del usuario. | Al ser comandos de introspección sin efectos secundarios, esperaba que no todos pidieran confirmación individual. | Cada uno pidió confirmación por separado (ninguno está en la lista segura del clasificador) — 3-4 confirmaciones para una tarea de solo lectura. | Molesto | — |

## Notas generales

El mensaje de error no distingue entre "no se hizo nada" y "se hizo el trabajo pero la ronda de cierre vino vacía" — para un modelo chico/local (Qwen en Nitro) que a veces no genera una ronda de resumen tras una tool call, este segundo caso es genuinamente común y no debería verse como una falla total del turno.

## Hallazgos que ameritan seguimiento

- **U-1**: una ronda vacía (sin texto, sin tool calls) inmediatamente después de que el turno ya despachó al menos una tool call exitosa terminaba el turno entero con `EngineError::EmptyModelResponse`, descartando el trabajo real ya hecho (en este caso, un `write_file` que sí llegó a disco). **Fix aplicado el mismo día** — ver PLAN.md § "Fix U-1": ese caso ahora recibe la misma ronda de resumen sin tools que ya existía para el agotamiento de `MAX_TURN_ITERATIONS` (`attempt_tools_free_summary_round`), en vez de fallar de inmediato. Un turno cuya primera ronda viene vacía (sin ningún progreso) sigue fallando igual que antes.
- **U-2**: `hardware_report.md` terminaba con *"Informe generado automáticamente el `$(date)`"* — el modelo copió la sintaxis de shell `$(date)` tal cual en vez de ejecutarla o de omitir la afirmación. Alucinación visible y verificable en el propio entregable final, no en un paso intermedio. Sin fix — no investigado si es reproducible con otros prompts/modelos.
- **U-3**: el informe lista `/dev/nvme0n1` y `/dev/ng0n1` como dos discos separados ("Secundario"), pero `/dev/ng0n1` es típicamente la interfaz de carácter genérica del *mismo* NVMe físico, no un segundo disco — probable duplicado de un mismo dispositivo contado dos veces a partir de la salida de `lshw`. Sin fix — no confirmado contra la salida cruda de `lshw` de esa sesión (no se guardó).
- **U-4**: el informe no menciona núcleos/hilos ni frecuencia de la CPU, pese a que `lscpu` (la fuente que sí corrió) reporta ambos — se quedó solo con el nombre del modelo, omitiendo el dato más específico de esa herramienta. Sin fix.
- **U-5**: `lscpu`/`lsmem`/`lshw` no están en la lista segura de `DefaultClassifier::is_safe_shell_command` (`crates/braze-permissions/src/classifier.rs`) — a diferencia de `ls`/`pwd`/`wc`/`whoami`/`date`/`which`, quedan clasificados Irreversible y piden confirmación individual pese a ser comandos de solo-lectura/introspección sin argumentos de ruta (sin la superficie de ataque que sí justifica el chequeo de `cat`/`head`/`tail`/`grep`/`find`). Candidato natural para sumar a la lista segura sin argumentos — no implementado, no se decidió si vale la pena ampliar el alcance del clasificador por esto.
