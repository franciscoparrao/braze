# ¿La trampa entre palancas es del género o es nuestra? — 2026-07-27

**Veredicto: es nuestra.** La precondición sí es universal, pero la
combinación que produce la trampa no existe en ningún otro harness abierto
que se revisó.

## Por qué se hizo esta revisión

El 2026-07-26 se encontró en vivo una falla que no es de capacidad del
modelo sino de **composición de dos palancas del harness, cada una correcta
por separado**:

- el **colapso ACI** reduce observaciones viejas a una línea salvo las
  últimas 5, y
- el **nudge de llamada repetida** responde a una re-petición idéntica con
  *"ya llamaste a esto, el resultado no ha cambiado, usá el que ya tenés"*.

Cuando la primera borró el contenido del contexto, la segunda se negó a
devolverlo. Contra roam, gpt-oss:20b leyó `lib.rs` en 3 páginas, la página 1
se colapsó, pidió releerla y el nudge se la negó **cuatro veces** hasta
abandonar el turno con el plan correcto en la mano.

Eso planteó la pregunta que decide si el hallazgo es ciencia o mantenimiento:
**¿es un hallazgo sobre diseño de harnesses, o sobre el nuestro?** La
respuesta se obtiene leyendo harnesses ajenos, no midiendo más el propio.
Costo: ~20 minutos, cero cómputo.

## Qué se revisó

Código fuente de `HEAD`, clonado el 2026-07-27:

| Harness | Colapso de observaciones | Guard de repetición | Semántica al disparar |
|---|---|---|---|
| **SWE-agent** | ✅ `LastNObservations` | ❌ | — |
| **Aider** | ✅ `ChatSummary` | ❌ | — |
| **OpenHands** | ✅ condenser | ✅ `StuckDetector` | **detiene el agente** |
| **braze** | ✅ colapso ACI | ✅ nudge | **niega y afirma posesión** |

### La precondición ES universal

Los tres harnesses ajenos gestionan el contexto borrando o comprimiendo
observaciones viejas. El de SWE-agent
(`sweagent/agent/history_processors.py`) es idéntico al nuestro hasta en el
default:

> `LastNObservations` — "Elide all but the last n observations […] Elided
> observations are replaced by `Old environment output: (n lines omitted)`"

con `n = 5`, el mismo número. Aider comprime por resumen
(`ChatSummary.too_big`/`summarize`) en vez de elidir, pero la consecuencia
para el modelo es la misma: **el contenido que vio hace varias rondas ya no
está**.

### Pero la combinación no

- **SWE-agent y Aider no tienen guard de repetición.** Lo que en una
  búsqueda textual parece serlo, no lo es: el `blocklist` de SWE-agent
  rechaza comandos *no soportados por el entorno* (herramientas
  interactivas), `filter_duplicates` de `action_sampler.py` deduplica
  *candidatos de best-of-n*, y `collapse_repeats` de Aider es parseo de
  udiff. Ninguno actúa sobre "el modelo re-emitió la misma llamada".

- **OpenHands sí tiene ambas, pero con otra semántica.** Su `StuckDetector`
  (`conversation/stuck_detector.py`) reconoce explícitamente
  `_is_stuck_repeating_action_observation`, o sea el patrón exacto. Sin
  embargo, al disparar hace:

  ```python
  if is_stuck:
      logger.warning("Stuck pattern detected.")
      self._state.execution_status = ConversationExecutionStatus.STUCK
      continue
  ```

  **Corta el loop.** No le niega la llamada al modelo, y sobre todo no le
  afirma que ya tiene un resultado que el condenser borró.

Nuestra trampa requiere las dos cosas juntas —**negar la re-lectura Y
afirmar posesión**— y esa semántica no aparece en ningún otro.

## Qué sobrevive del hallazgo

**No es un bug del género.** Es nuestro, y como tal ya está arreglado (el
nudge ahora devuelve el resultado cacheado en vez de negarse).

Lo que sí sobrevive es más modesto pero real: una **advertencia de diseño**.
La precondición es universal, así que cualquier harness que agregue un guard
de repetición de estilo "negar" sobre una gestión de contexto que elide
observaciones **hereda la trampa por construcción**. Es un aviso accionable
para quien diseñe la próxima combinación, no el descubrimiento de un fallo
extendido.

## Hipótesis NO testeada (y por qué no se afirma)

Si el condenser de OpenHands elide el contenido y el modelo lo re-pide
legítimamente, su `StuckDetector` probablemente clasifique eso como atasco y
**termine la conversación** — la misma interacción subyacente con una
manifestación distinta, y peor en resultado: mata la sesión en vez de gastar
cuatro llamadas.

Es una predicción plausible desde la lectura del código, **no una
medición**. Verificarla exige correr OpenHands con un archivo lo bastante
grande como para forzar condensación y una tarea que requiera releerlo.
Barato, pero no hecho — y afirmarlo sin correrlo sería exactamente el error
que este proyecto pasó el día anterior evitando.

## Valor de método

El test costó ~20 minutos y cero cómputo, y evitó invertir **~19 horas** de
Nitro en un factorial 2×2 sobre un eje que no tenía la generalidad que se le
suponía. Es el mejor retorno por unidad de esfuerzo de toda la sesión.

La lección transferible: **antes de medir en profundidad si un fenómeno de
tu sistema generaliza, revisá si el fenómeno siquiera puede existir en los
sistemas a los que querés generalizarlo.** Es más barato leer código ajeno
que medir el propio.
