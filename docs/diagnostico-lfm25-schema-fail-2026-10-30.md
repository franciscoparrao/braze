# Diagnóstico: los 241 fallos de esquema de LFM2.5-24B-A2B

Fecha: 2026-10-30. Datos: `docs/sweep-lfm25-24b-a2b-q40-discriminating-2026-10-20.{json,log}`
(suite `discriminating.toml`, huella `3daaf2e779f06c8f`, commit `5084d4a`, 3
repeticiones, servido por llama-server vía backend `zen`) y
`docs/sweep-lfm25-24b-a2b-q40-default-2026-10-17.{json,log}`.

## Conclusión

Casi todos los fallos de esquema son un solo error de forma. LFM2.5 manda el
argumento `command` de `shell_exec` como texto (`"ls -la"`), y el esquema pide una
lista de argumentos (`["ls", "-la"]`). El 70 % de esos comandos se puede convertir
a lista sin cambiar su significado. Eso motiva una palanca nueva, `argv-coerce`,
apagada por defecto, que se evalúa con el A/B pre-registrado de abajo.

## Evidencia (verificada con los archivos citados)

- **Totales.** Barrido discriminante: 102 corridas, 59 pasan, 241 fallos de esquema
  según el JSON. Barrido `default`: 57 corridas, 57 pasan, 17 fallos de esquema. Las 7
  corridas con fallos pasan igual.
- **Forma del fallo.** El log del barrido discriminante registra 173 fallos de
  esquema con su mensaje. 164 son `shell_exec` con `"…" is not of type "array"`. 8
  más son `shell_exec` con texto multilínea o escapado, y 1 es un `write_file` sin
  `path`. En el barrido `default`, los 10 registrados son del mismo tipo que los 164.
- **El modelo no aprende del mensaje.** De los 172 fallos de `shell_exec`
  registrados, 52 son el primer intento de su turno y 120 son reintentos
  (`attempt` ≥ 2). Tras recibir el esquema en el mensaje de reparación, LFM2.5 vuelve
  a mandar el texto.
- **Cuántos se pueden partir sin shell.** De 172 comandos, 121 se parten igual que
  en un shell: `ls` (61), `find` (28), `pwd` (8), `sed` (6), `cat` (5) y otros.
  Los otros 51 usan pipes, `&&`, `;` o comodines sin comillas. Sin shell harían otra
  cosa, así que no se deben partir.
- **Asociación con el resultado.** Las corridas con al menos un fallo de esquema
  pasan 15 de 54 (28 %). Las corridas sin fallos pasan 44 de 48 (92 %). De las 39
  corridas que fallan con fallos de esquema, 23 terminan por timeout y 9 por exceder
  el máximo de rondas.

## Lo que no está demostrado

- **Causalidad (inferido, probable).** Que los fallos de esquema causen las fallas
  es probable, pero no está demostrado. Las tareas más difíciles podrían producir
  a la vez más fallos de esquema y más fallas. El dato apunta en las dos
  direcciones. Las habilidades sin fallos de esquema (`localizacion` 13/15, `piso`
  6/6) están entre las más fáciles, y `techo` (0/6) acumula 25. Pero
  `escalera_borrado` acumula 43 y pasa 11/15. El A/B de abajo es la prueba.
- **Brecha de conteo (sin explicar).** El JSON suma 241 fallos y el log registra
  173. No investigué la diferencia. Hipótesis no verificada: el contador del JSON
  incluye fallos que no emiten la línea de log al nivel con que se capturó.

## La palanca

`crate::rescue::coerce_string_to_argv` (braze-engine), activada con
`Engine::with_argv_coercion_enabled(true)` o `+ablate:argv-coerce` en el bench.
Cuando el esquema declara un campo como arreglo de textos y el modelo manda un
texto, lo parte en argumentos solo si `split_command_without_shell` lo acepta.

El partidor rechaza, fuera de comillas, `| & ; < > ( ) $ ` * ? [ ] { } ~ #`, `\` y
saltos de línea, y dentro de comillas dobles, `$`, `` ` `` y `\`. Si rechaza, el
error de esquema llega al modelo como antes. `shell_exec` sigue sin shell: la
palanca no amplía lo que el modelo puede ejecutar ni pasa por otro control de
permisos. El comando partido pasa por el mismo clasificador y la misma política
que una lista escrita por el modelo.

## Pre-registro del A/B (fijado antes de correr)

- **Hipótesis.** En LFM2.5-24B-A2B, `argv-coerce` sube la tasa de éxito en la
  suite discriminante.
- **Brazos.** `zen:lfm25-24b-a2b-q40` frente a `zen:lfm25-24b-a2b-q40+ablate:argv-coerce`,
  en el mismo barrido. Suite `discriminating.toml` (34 tareas), 3 repeticiones,
  semilla fija, mismo servidor llama-server que el barrido del 2026-10-20.
- **Validez.** El barrido no cuenta si un brazo supera 2 % de fallas de transporte.
- **Chequeo de mecanismo.** En el brazo tratado, los fallos de esquema de
  `shell_exec` con `"…" is not of type "array"` deben bajar al menos 60 %. Debe
  aparecer el log `command string split into argv`. Si el mecanismo no se verifica,
  el resultado de tasa de éxito no se interpreta.
- **Criterio de adopción.** Activar la palanca por defecto si (1) McNemar exacto
  pareado por (tarea, repetición) da p < 0,05 a favor del tratamiento, y (2) ninguna
  habilidad baja más de 2 pasadas en el brazo tratado.
- **Control de daño.** Un segundo barrido corto con `qwen3.5-9b` (Q3_K_M, la
  configuración de `scripts/braze-qwen-nitro.sh`) en los dos brazos. No debe
  bajar más de 2 pasadas en total. Ese modelo casi no produce este fallo, así que se
  espera un empate.
- **Si no se cumple.** La palanca queda opcional y se reporta el nulo.
- **Iteración permitida (una).** Si el mecanismo se verifica pero la tasa no
  sube, se permite una sola iteración: mejorar el mensaje de reparación para los 51
  comandos con sintaxis de shell. El mensaje diría que `shell_exec` no tiene shell
  y que el comando debe partirse en llamadas separadas.
