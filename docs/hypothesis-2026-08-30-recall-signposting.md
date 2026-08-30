# Pre-registro piloto 2: ¿basta el índice, o hace falta que el usuario lo pida?

Fecha: 2026-08-30
Antecedente: `docs/pilot-recall-invocation-2026-08-29.md` (piloto 1, CERRADO)
Diseño: `docs/distilled-memory-design-2026-08-29.md`
Estado al escribir esto: **ninguna corrida hecha.**

## Por qué

El piloto 1 pasó su umbral (gpt-oss 12/12, ornith 11/14) pero destapó un
defecto de su propio fixture: los tres prompts no señalizaban igual. Los de
`logging`/`tests` decían "siguiendo las convenciones de este proyecto"; el
de `errors` solo "el error apropiado del proyecto". La invocación se cayó
entera justo en `errors` (2/5 contra 4/4 y 5/5).

Eso deja sin responder la pregunta que decide el diseño V2. El índice del
system prompt está pensado para activar la consulta **por sí solo**: en
producción el usuario escribe "arregla este bug", no "arregla este bug
siguiendo las convenciones de este proyecto". Si la invocación depende de la
coletilla, el índice no cumple su función y el diseño necesita otro vehículo
de activación.

## Hipótesis

**H1.** Con el índice presente en el system prompt, un prompt **neutro**
(sin ninguna alusión a convenciones ni al proyecto) activa la consulta con
tasa comparable a un prompt señalizado.

**H0.** La consulta depende de la señal en el prompt del usuario; el índice
por sí solo no la activa.

## Diseño: A/B pareado, la coletilla como única diferencia

Mismo fixture del piloto 1 (`scripts/recall_invocation_pilot.py`), mismas
tres convenciones arbitrarias, mismo índice en `AGENTS.md`.

| Brazo | Prompt |
|---|---|
| `neutral` | "Agrega un test unitario para la función `add` de src/lib.rs." |
| `signposted` | …misma frase + ", siguiendo las convenciones de este proyecto." |

**Los brazos se intercalan** dentro de cada repetición, no se corren en
bloques: si Nitro se degrada a mitad de la sesión, debe afectar a ambos por
igual. (El piloto 1 no tenía este problema porque no comparaba brazos.)

Ejecutor: **ornith:9b**. Es el brazo informativo — gpt-oss:20b saturó al
100% en el piloto 1 y no dejaría ver una caída. 3 tareas × 2 brazos × 5
reps = 30 corridas.

## Métrica primaria

`recall_invocation_rate` **del brazo neutro**. El brazo señalizado es el
control que verifica que el fixture sigue produciendo consultas.

## Criterio de decisión, comprometido antes de correr

- **Neutro ≥ 60%** → el índice basta por sí solo. Seguir al paso 2 del
  diseño (esquema + store + `recall_memory`).
- **Neutro 40-60%** → el índice ayuda pero no alcanza. Antes de construir,
  un tercer piloto sobre el *vehículo* (peso, redacción o posición del
  índice), no sobre el contenido.
- **Neutro < 40%** → **el índice NO cumple su función.** El diseño V2 no se
  rechaza entero, pero su premisa central sí: habría que activar la consulta
  por otro medio (por ejemplo, que el harness la inyecte en la primera ronda
  del turno) y eso vuelve a pagar costo por ronda — es decir, vuelve a
  chocar con R1 y hay que rediseñar, no parchar.

Diagnóstico secundario, ya predicho: si en el brazo señalizado `errors`
sube claramente por encima de su 2/5 del piloto 1, queda confirmado que
aquel resultado fue el confound de redacción y no una propiedad de la tarea.

## Amenazas a la validez

- n=5 por celda: detecta un efecto grosero, no un matiz. McNemar pareado se
  reporta como referencia, no como prueba.
- Un solo ejecutor. El piloto 1 ya mostró que gpt-oss:20b se comporta
  distinto (saturado), así que esto no generaliza hacia arriba.
- La coletilla es UNA operacionalización de "señalizar". Otras redacciones
  podrían empujar más o menos.
- Sigue siendo el mejor caso posible: memoria exactamente relevante,
  convenciones inadivinables, tarea corta.
