# Piloto de invocación de memoria — resultados

Fecha: 2026-08-29
Pre-registro: `docs/hypothesis-2026-08-29-recall-invocation.md` (escrito ANTES de correr)
Diseño: `docs/distilled-memory-design-2026-08-29.md` § paso 1
Datos: `docs/pilot-recall-invocation-gptoss20b-2026-08-29.json`,
`docs/pilot-recall-invocation-ornith9b-2026-08-29.json`
Runner: `scripts/recall_invocation_pilot.py`
Estado: **CERRADO.**

## Resultado

| Ejecutor | n | `recall_invocation_rate` | `convention_compliance` | timeouts |
|---|---|---|---|---|
| gpt-oss:20b | 12 | **1.00** (12/12) | 1.00 | 3 |
| ornith:9b | 14 | **0.786** (11/14) | 0.643 | 1 |

**Veredicto según el criterio comprometido (>60% → seguir): SEGUIR al paso 2
del diseño.** Ambos ejecutores superan el umbral. La hipótesis H1 —un modelo
chico consulta una memoria que el system prompt solo señaliza— no se
rechaza.

## El hallazgo más fuerte no es la tasa: es el cruce

En ornith:9b, cruzando consulta contra cumplimiento:

| | cumplió | no cumplió |
|---|---|---|
| **consultó** | 9 | 2 |
| **no consultó** | 0 | **3** |

**Ninguna corrida cumplió la convención sin consultar** (0/3), y 9 de 11 la
cumplieron tras consultar. Esto es el *manipulation check* del fixture y
sale limpio: las convenciones son efectivamente inadivinables, así que la
consulta no es decorativa — es lo que produce el resultado correcto. Sin
este cruce, un `recall_invocation_rate` alto sería compatible con un modelo
que consulta por reflejo y resuelve por su cuenta.

## Estructura por tarea: donde el piloto se muerde la cola

ornith:9b, corridas completas:

| Tarea | recall | cumplimiento |
|---|---|---|
| `errors` | **2/5** | 2/5 |
| `logging` | 4/4 | 4/4 |
| `tests` | 5/5 | 3/5 |

La invocación NO es uniforme: se cae entera en `errors`. Y la causa más
probable es **un defecto de mi propio fixture**, no una propiedad del
modelo:

- prompt de `errors`: "…devuelva **el error apropiado del proyecto** si el
  string no es válido"
- prompt de `logging` / `tests`: "…**siguiendo las convenciones de este
  proyecto**"

Los tres prompts debían ser equivalentes en cuánto empujan a consultar, y no
lo son: "las convenciones de este proyecto" señaliza mucho más fuerte que
"el error apropiado". **Confound introducido por el diseño del fixture**, y
los datos lo delatan.

La lectura que sí sobrevive, y que importa para el diseño V2: **la
invocación parece depender más de cómo está redactado el prompt de la tarea
que del índice del system prompt.** Si eso se confirma, el índice no basta —
y una memoria cuya activación depende de que el usuario redacte bien su
pedido es mucho menos útil que una que se activa sola. Es la pregunta del
próximo piloto, no de éste.

## Brecha entre ejecutores

gpt-oss:20b consultó en el 100% de las corridas, incluidas las tres tareas;
ornith:9b, en el 78,6%, con toda la pérdida concentrada en la tarea de
señalización débil. La brecha es consistente con la tesis del proyecto (la
capacidad del modelo modula qué palancas del harness rinden) pero **con
n=12/14 y un confound identificado, no se reporta como efecto establecido**.

## Limitaciones

- **n chico por diseño** (5 reps × 3 tareas): decisión de matar-o-seguir, no
  un número publicable.
- **Censura no aleatoria**: los 3 timeouts de gpt-oss y el 1 de ornith son
  TODOS de `logging`, la tarea más larga (11-14 rondas vs 3-8). Esa celda
  queda con n=2 en gpt-oss. El timeout de 240s se mantuvo igual entre
  modelos por comparabilidad, aceptando la censura en vez de cambiar el
  protocolo a mitad de camino.
- **Mejor caso posible**: la memoria es exactamente relevante para la tarea.
  Un fracaso acá habría sido fatal; el éxito es apenas condición necesaria.
- **`braze run` es one-shot**: mide la primera consulta, no el uso sostenido
  ni el efecto del colapso ACI sobre una memoria consultada hace 5 rondas —
  que es la tensión central que el diseño V2 dejó abierta.
- Nada de esto mide la métrica del Paper 2 (`net_token_delta`). Que el
  modelo consulte no implica que la memoria amortice; son dos preguntas
  distintas y ésta era la barata.

## Qué sigue

1. **Piloto 2, antes de escribir código**: homogeneizar la señalización de
   los tres prompts y volver a medir `errors`. Si la invocación se recupera
   con el prompt neutro, el índice funciona; si no se recupera, el índice es
   insuficiente y el diseño V2 necesita otro vehículo de activación.
2. Solo después: esquema + store + `recall_memory` + índice con cap duro.

El paso 1 vuelve a ser barato y vuelve a poder matar una parte del diseño.
