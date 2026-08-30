# Pre-registro: ¿invoca el modelo una memoria que solo está señalizada?

Fecha: 2026-08-29
Diseño: `docs/distilled-memory-design-2026-08-29.md` § "Orden de trabajo", paso 1.
Estado al escribir esto: **ninguna corrida hecha.** Criterio comprometido antes de medir.

## Por qué este piloto existe

El Paper 2 midió la memoria **inyectada** y encontró que cae del lado
equivocado de la frontera de amortización. El diseño V2 responde moviendo el
contenido fuera del prompt: índice mínimo señalizado + detalle bajo demanda.

Ese movimiento introduce un modo de falla nuevo que el Paper 2 no podía
tener: **si el modelo nunca consulta, el índice es costo puro y la memoria
no existe en la práctica.** Este piloto mide eso y nada más. Es barato a
propósito: sin destilador, sin hook, sin crate nuevo, sin tocar
`braze-memory`.

## Hipótesis

**H1.** Un modelo chico, con un índice de memoria en el system prompt que
solo lista títulos y dónde consultarlos, **consulta el detalle** cuando la
tarea depende de una convención que solo está en la memoria.

**H0.** El modelo ignora el índice y resuelve la tarea con sus propios
supuestos.

## Diseño

- **Fixture**: proyecto Rust mínimo. `AGENTS.md` lleva el índice (títulos +
  la ruta donde vive el detalle), NUNCA el contenido. Las entradas completas
  viven en `project-memory/<id>.md` (NO `.braze/`: ese
  directorio es Irreversible para escrituras del modelo desde v8 Paquete 2, y
  mezclar la medición de invocación con una posible denegación de permiso
  violaría la regla anti-drift de no probar dos palancas a la vez).
- **El índice no ordena consultar.** Lista y señala. Una instrucción del
  tipo "debes leer la memoria" mediría obediencia, no invocación espontánea,
  y en producción el harness tampoco puede obligar por cada ronda sin volver
  a pagar el costo que el diseño evita.
- **Convenciones arbitrarias, no adivinables.** Si la convención fuera la
  idiomática (usar `tracing` en vez de `println!`), el modelo acertaría sin
  consultar y la métrica de cumplimiento no distinguiría nada. Las tres
  convenciones del fixture son deliberadamente contrarias a lo que un modelo
  elegiría por defecto.
- **3 tareas × 5 repeticiones × modelo.** Ejecutor: `gpt-oss:20b` en Nitro
  (el mejor local del proyecto) y, si el primero pasa, `ornith:9b`.
- Un fixture limpio por corrida.

## Métricas

| Métrica | Definición |
|---|---|
| `recall_invocation_rate` | **PRIMARIA.** Fracción de corridas donde el modelo leyó algún archivo de `.braze/memory/`. |
| `convention_compliance` | Fracción donde el archivo final respeta la convención arbitraria. |
| `compliance_given_recall` | Cumplimiento condicionado a haber consultado — separa "no consultó" de "consultó y no le sirvió". |

## Criterio de decisión, comprometido antes de correr

- **`recall_invocation_rate` < 20% → RECHAZAR** el diseño V2 en su forma
  actual y publicar el nulo. La memoria bajo demanda no funciona si el
  modelo no la pide; el índice sería costo puro por ronda, exactamente el
  fallo que el diseño existía para evitar.
- **20-60% → INDECISO.** El vehículo del índice se revisa (peso, redacción,
  posición) antes de construir nada. Un segundo piloto, no el crate.
- **> 60% → SEGUIR** al paso 2 del diseño (esquema + store + herramienta).

Cláusula anti-racionalización: si el resultado cae bajo 20%, **no** se
reinterpreta como "el índice necesita ser más explícito". Un índice que
tiene que ordenar la consulta es un índice que ya no es barato, y esa
variante sería una hipótesis nueva con su propio pre-registro.

## Amenazas a la validez, anotadas antes

- **n chico** (15 corridas por modelo): esto detecta un efecto grosero
  (0% vs 80%), no un matiz. Suficiente para una decisión de matar-o-seguir,
  insuficiente para publicar un número.
- **Una sola familia de tareas** (convenciones de código Rust). Un resultado
  positivo no generaliza a memoria de otro tipo.
- **El fixture es artificial**: la memoria es exactamente relevante para la
  tarea, que es el mejor caso posible. Un fracaso acá es fatal; un éxito acá
  es apenas condición necesaria.
- `braze run` es one-shot: mide la primera invocación, no el uso sostenido a
  lo largo de una sesión larga.
