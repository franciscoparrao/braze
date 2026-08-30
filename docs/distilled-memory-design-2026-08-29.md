# Memoria destilada (V2): diseño

Fecha: 2026-08-29
Estado: **DISEÑO — nada implementado.** Pre-registro obligatorio antes de correr.
Pedido del autor: que "guarda la conversación", "restaura la conversación" y
"actualización automática de contexto ante eventos importantes" sean
características core de braze.

## Qué ya existe (y por qué dos tercios del pedido están resueltos)

| Pedido | Estado |
|---|---|
| "Guarda la conversación" | **Resuelto, y mejor que un comando**: `SessionStore::append` escribe cada evento al rollout log según ocurre. No hay que guardar porque nunca hay nada sin guardar. |
| "Restaura la conversación" | **Resuelto**: `--resume <id>`, más `load_and_repair`, que sintetiza resultados de error para las tool calls huérfanas si el proceso murió a mitad de ronda. |
| "Actualización automática ante eventos importantes" | **Esto es lo que falta.** |

Corolario que responde la pregunta que originó el pedido ("al cambiar de
modelo, ¿cómo entiende el nuevo el contexto previo?"): **el contexto nunca
está en el modelo.** `Engine` reconstruye el `Vec<Message>` desde el event
log en cada turno (`engine/context.rs:475`), y el rebuild de `/model`
preserva `SessionId` y store (`main.rs:1413-1414`). Cambiar de modelo es
cambiar el destinatario de una carta que se redacta de cero cada vez.

Lo que `braze-memory` captura hoy es pobre a propósito: archivos tocados
por `write_file`/`edit_file` y tareas marcadas como completadas.
Determinístico, cero llamadas al modelo, off por default. Sin decisiones,
sin razones, sin errores resueltos — el destilado semántico no existe.

## Las tres restricciones duras

Cualquier diseño que las ignore ya está refutado por evidencia propia.

### R1 — La frontera de amortización (Paper 2, 586 corridas)

Un loop agéntico **re-envía su contexto en cada ronda**. Una sección de
memoria inyectada al system prompt cuesta `tamaño × rondas` y solo se paga
si ahorra suficientes rondas. Medido: break-even entre **1 y casi 2 rondas
completas**, y la memoria procedimental realista falla ese umbral **por un
factor de 3 a 6** en tareas frescas.

El patrón de la síntesis de 3 tareas es el que más obliga: el playbook
ahorró rondas **solo en la tarea que el modelo ya tenía memorizada** (donde
es redundante) y no ahorró nada donde el modelo tenía que razonar de verdad.

Números concretos a respetar: el playbook que **falló** costaba
**~200-270 tokens/ronda**. Ese es el orden de magnitud prohibido.

### R2 — Superficie de inyección (K-3, v8)

`render_project_memory_section` NO renderiza `objective`/`notes` aunque
existan en el archivo, porque `.braze/memory.json` **lo escribe el propio
modelo** (y un repo ajeno puede traerlo clonado): renderizarlos era un canal
de inyección persistente al system prompt con prioridad sobre todo lo demás.
Hay un test que lo fija con `"IGNORE ALL PREVIOUS INSTRUCTIONS"` de carga.

El destilado semántico es **exactamente ese tipo de texto libre**. La
decisión K-3 dejó explícita la condición de reingreso: "cuando V2 los llene
por un canal curado, se reintroducen junto con su decisión de confianza
explícita". Este documento es ese V2 y le debe esa decisión.

### R3 — El Paper 2 está congelado

`braze-memory` es el artefacto del Study 2 del Paper 2 ("the production
form", `paper2/main.tex:395`), con paquete de submission listo y deadline
2026-09-28. **El hook V1 no se toca.** V2 es un mecanismo aditivo con su
propio flag; el brazo `+ablate:project-memory` del paper sigue midiendo lo
que midió.

## Diseño

### Canal: híbrido — índice fijo + detalle bajo demanda

Es el patrón que braze ya usa como firma arquitectónica (carga diferida de
herramientas: nombres en contexto, schema completo bajo demanda) y el que el
propio sistema del autor usa en Claude Code (`MEMORY.md` como índice de una
línea, archivos leídos solo cuando hacen falta).

- **Índice** (al system prompt, cada ronda): una línea por entrada, solo
  título y tipo. Le recuerda al modelo que la memoria existe — sin esto, el
  riesgo real es que un modelo chico nunca invoque la herramienta.
- **Detalle** (bajo demanda): herramienta `recall_memory(id)` que devuelve
  la entrada completa como tool result.

### El presupuesto del índice es la decisión de primer orden

**El índice cae bajo R1 igual que el playbook fallido**, porque también se
re-envía en cada ronda. Si el índice pesa lo mismo que el playbook, hereda
su fracaso.

Presupuesto: **≤ 50 tokens totales**, cerca de un orden de magnitud bajo los
~200-270 que fracasaron. Concretamente: **máximo 8 entradas × ~6 tokens**
(título recortado a ~30 caracteres + marcador de tipo). Cap duro en el
render, no una recomendación. Entradas por encima del cap no se listan;
siguen siendo consultables si el modelo pide el índice completo por
herramienta.

Corolario incómodo, y hay que aceptarlo: **el índice debe ser
deliberadamente insuficiente.** Su trabajo no es informar, es señalizar que
hay algo que consultar.

### El costo del recall NO es cero

Un tool result también entra al contexto y **se re-envía en las rondas
siguientes**. El costo real de una consulta es `tamaño × rondas restantes
del turno`, no `tamaño × 1`.

Lo mitiga el colapso ACI (backlog 2), y esto está **verificado**:
`tactical_full_observation_indices` (`history.rs:274`) decide qué
observaciones quedan completas **solo por recencia y presupuesto de bytes,
sin discriminar por herramienta**. Un tool result de `recall_memory`
colapsa como cualquier otro una vez que hay 5 observaciones más nuevas.

Pero eso destapa una tensión que el diseño debe resolver, no heredar:

- **Si colapsa** (comportamiento actual): la memoria consultada se evapora a
  una línea a las 5 rondas — justo en los turnos largos, que son los que más
  la necesitarían. El costo se contiene; el beneficio también.
- **Si se exime**: vuelve a pagar `tamaño × rondas restantes`, que es
  exactamente la aritmética de R1 que este diseño existe para esquivar.

Hay precedente estructural para la exención (`NEVER_CLEAR_TOOLS`,
`history.rs:35`, hoy vacío a propósito) pero gobierna otro mecanismo —el
clearing de `durable_events`, no el colapso táctico—, así que eximir del
colapso pediría una palanca nueva.

**Decisión V2: dejar que colapse**, sin exención. Razones: es el
comportamiento existente (nada nuevo que justificar), mantiene el costo del
lado correcto de R1, y si el modelo necesita la entrada de nuevo puede
volver a consultarla — que es precisamente la ventaja de tener la memoria
como herramienta y no como bloque fijo. **Una re-consulta es la señal
observable de que el colapso dolió**, y por eso se instrumenta:
`recall_repeat_rate` entra al piloto como métrica secundaria.

### Confianza: dos niveles, que es la respuesta a K-3

| Nivel | Quién lo escribe | ¿Va al índice del system prompt? | ¿Consultable por `recall_memory`? |
|---|---|---|---|
| `proposed` | El modelo destilador | **No** | Sí, marcado como no verificado |
| `confirmed` | El autor lo aprueba/edita | Sí | Sí |

Esto satisface literalmente la condición de K-3: el canal curado es la
promoción `proposed → confirmed`, y la decisión de confianza explícita es
que **solo lo confirmado alcanza el system prompt**. Lo propuesto sigue
siendo útil (el modelo puede consultarlo) sin ganarse la prioridad
estructural que hacía peligroso el texto libre.

Defensas que se heredan de V1 y NO se re-discuten: colapso de `\n` y `ESC`
en todo campo escrito por el modelo, caps de longitud por campo, y
`project_key` verificado contra el proyecto pedido (K-7).

### Esquema de la entrada

Campos tipados y acotados, no prosa libre — reduce la superficie de R2 y
hace el índice barato:

```
id            corto, estable
kind          decision | error_resolved | constraint | reference
title         ≤ 60 chars — lo único que llega al índice
body          ≤ 500 chars — el "qué"
why           ≤ 300 chars — el "por qué" (lo que el autor pide y V1 no tiene)
trust         proposed | confirmed
provenance    session_id + evento que la disparó + modelo destilador
at            timestamp
```

`why` es el campo que justifica todo el ejercicio: es lo que el
`context_manager.py` del autor guarda y lo que `braze-memory` V1 no captura.

### Disparadores: qué es un "evento importante"

Del event log, en orden de valor esperado:

1. **Escalación al lead** (`AgentEvent::EscalationToLead`) — el instante en
   que el chico falló y el grande lo rescató. Es *exactamente* el momento que
   el Paper 2 quería capturar, y ahora es un evento tipado en el log.
2. **Pre-compactación** — antes de que la ventana táctica se pliegue y el
   detalle se pierda. Análogo directo al `pre-compact` del sistema del autor.
3. **Racha de fallos seguida de éxito** — la firma de un error resuelto.
4. **`TaskCompleted`** — ya usado por V1.

### Quién destila

El modelo, con curaduría explícita (decisión del autor, 2026-08-29). Dos
precisiones que el diseño necesita:

- **Destila el lead, no el executor**, cuando hay `--lead` configurado. El
  modelo fuerte es el que tiene criterio para separar lo que importó de lo
  que pasó; y en el disparador 1 el lead ya está en contexto.
- **La llamada de destilación no va en el camino crítico del turno.** Se
  encola como el saver de V1 (v8 K-8: `on_event` no hace I/O de disco bajo
  el `HOOK_TIMEOUT` de 250ms). Una destilación lenta no debe demorar una
  respuesta al usuario.

## Cómo se mide (pre-registro, antes de escribir código)

La disciplina del proyecto exige criterio comprometido antes de correr.

**Métrica primaria**, heredada del Paper 2 para que sea comparable:
`net_token_delta` y `round_reduction`. La memoria destilada V2 debe mostrar
`net_token_delta < 0` en tareas **frescas** — que es exactamente donde el
playbook del Paper 2 falló por 3-6×.

**Métrica de mecanismo, y la más informativa**: `recall_invocation_rate`.
Si el modelo nunca invoca `recall_memory`, el índice es costo puro y el
diseño está muerto por una razón distinta a la del Paper 2. Esta métrica se
mide **primero**, en un piloto barato, porque puede matar el diseño antes de
gastar un sweep completo.

**Caveat de medición, heredado del análisis de hoy**: `default.toml` está
saturada (gpt-oss:20b 57/57, ornith:9b 95/95). No queda espacio donde ver el
efecto. Medir en la **suite discriminante v2** (34 tareas, 2,9 pp/ítem).

**Criterio de rechazo comprometido**: si `recall_invocation_rate` < 20% en
el piloto, o si `net_token_delta ≥ 0` en tareas frescas con el índice al
presupuesto, **se rechaza y se publica el nulo** — igual que el stencil, la
palanca de verificación y el edit-fence. El proyecto tiene tres nulos
limpios publicados; un cuarto es un resultado, no un fracaso.

## Qué NO hace este diseño

- No toca el hook V1 ni su render (R3).
- No inyecta `objective`/`notes` (R2 sigue vigente para V1).
- No promete mejorar el pass rate. El Paper 2 no encontró señal de pass rate
  en ninguna configuración; la apuesta acá es de economía de contexto, no de
  capacidad.

## Orden de trabajo propuesto

1. Piloto de `recall_invocation_rate` con memoria estática hecha a mano — sin
   destilador, sin hook, sin escribir el crate. Mide lo único que puede matar
   el diseño temprano: si el modelo consulta o no.
2. Solo si pasa: esquema + store + `recall_memory` + índice con cap duro.
3. Solo entonces: el destilador y sus disparadores.

El paso 1 es barato y puede ahorrar los otros dos.
