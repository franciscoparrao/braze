# Piso de ruido de `braze-bench`: cuánto difieren dos corridas idénticas

**El número que le faltaba al proyecto.** Sin él, ningún A/B sobre modelos
chicos se puede interpretar: no hay forma de saber si una diferencia de N
tareas es un efecto o es la máquina.

## Método

13 sweeps de `default.toml` con configuración **idéntica** (misma suite, 3
repeticiones, `--seed 42`, timeout 180s, mismo binario, misma GPU), variando
solo el modelo y el régimen de sampling.

La clave del diseño: el seed del bench es `base + repetición`, o sea **idéntico
entre corridas** para cada repetición. Así que dentro de una corrida las
repeticiones difieren, pero **entre corridas la única fuente de variación es el
no-determinismo de punto flotante de la GPU**. Cualquier diferencia observada es
ruido por construcción.

## Resultados

| | pass rate | discordancia entre pares | tareas inestables | walltime medio |
|---|---|---|---|---|
| **gemma4:e4b — greedy** (5 corridas) | 53-55 / 57 (sd 0.89) | mediana 2, **máx 5** | 5 de 57 | 7.6-12.1s |
| **gemma4:e4b — temp 0.2** (5 corridas) | 53-57 / 57 (sd 1.50) | mediana 4, **máx 5** | 7 de 57 | 7.5-10.2s |
| **gpt-oss:20b — greedy** (3 corridas) | 57-57 / 57 (sd **0.00**) | **0** en los 3 pares | **0** | 11.5-11.7s |

*Discordancia* = cuántos resultados `(tarea, repetición)` se dan vuelta entre
dos corridas idénticas. Es la métrica operativa.

## Reglas operativas

- **gemma4:e4b — una diferencia de ≤5 tareas entre dos brazos es
  indistinguible del ruido.** Con temperatura el ruido rutinario se duplica
  (mediana 2 → 4) aunque el máximo no cambie.
- **gpt-oss:20b es determinista en este suite**: 0 discordantes, sd 0.00.
  Cualquier diferencia observada es real.
- **El walltime es MUCHO más ruidoso que el pass rate en e4b**: ±30% entre
  corridas idénticas (7.6-12.1s de promedio). En gpt-oss, ±1%. Una comparación
  de velocidad en e4b necesita superar el 30% para significar algo.

## Tres hallazgos estructurales

**1. La inestabilidad está concentrada, no dispersa.** Las tareas que oscilan
son casi todas de `error_recovery` — donde una tool call falla y hay que
recuperarse, o sea las más ramificadas. **52 de 57 son roca.** Consecuencia
incómoda: la superficie realmente medible del suite es chica. Una palanca que
no toque `error_recovery` es casi invisible para `default.toml` en e4b.

**2. El ruido es propiedad del modelo, no del harness.** Mismo suite, mismo
binario, misma GPU: e4b oscila y gpt-oss no. Los logits de e4b empatan al nivel
del ruido de punto flotante (medido el mismo día: `"<eos>"=23.225 "<"=23.175
"The"=23.109`) y la GPU desempata distinto en cada corrida. gpt-oss no vive en
ese filo.

**3. Las afirmaciones de mecanismo sobreviven a este ruido; las de pass rate
no.** Es la distinción metodológica más útil que salió de esto. "Las rondas de
cero tokens pasaron de 5 a 0" es verificable directamente y no depende de
estadística. "El pass rate subió 3 puntos" es indistinguible del ruido. Diseñar
las palancas para que tengan una afirmación de mecanismo comprobable vale más
que perseguir puntos de pass rate.

## Qué implica retroactivamente

Aplicando el umbral a lo medido el mismo día:

| Afirmación previa | Veredicto |
|---|---|
| DRY mejora e4b (51 → 53) | **MUERTA.** Diferencia de 2, muy debajo de 5. Ruido. |
| La temperatura daña (51 → 43) | **SEÑAL.** 8 tareas, por encima del umbral. |
| La guarda de EOG sube el pass rate (~+3) | **Dentro del ruido.** Pero su afirmación de mecanismo (rondas de 0 tokens: 5 → 0) se sostiene. |
| gpt-oss 3.4× más rápido (41.4 → 12.1s) | **SÓLIDA.** Ruido de walltime en gpt-oss: ±1%. |
| KV placement medido: 1.95× (29.2 → 15.0s) | **Se sostiene** (muy por encima del ±30%), pero los márgenes son más finos de lo presentado: el 20.7 vs 15.0 (38%) apenas supera el ruido. |

## Nota de procedimiento: el primer intento se perdió entero

Los primeros 13 sweeps produjeron **JSONs vacíos** y nadie se enteró hasta el
análisis. Dos causas, ambas instructivas:

1. **Error de copia**: el script exportaba `BRAZE_LOCAL_FAMILY=harmony`,
   arrastrado de los scripts de roam (que son para gpt-oss). Sobre gemma4:e4b
   eso fuerza una plantilla cuyo `<|start|>` no es token único en ese
   vocabulario, y el backend se niega — correctamente.
2. **El bench omitió el backend y salió con éxito**, escribiendo un JSON vacío.
   Es el pendiente ya anotado ("fail-fast de brazo en el bench: 57 fallos
   instantáneos de carga no deben quemar un brazo en silencio"), ahora con una
   segunda ocurrencia y un costo concreto.

Agravado por una decisión mía: el script redirigía la salida a `/dev/null`, así
que un fracaso total se veía igual que un éxito. **Un script de experimento no
atendido debe verificar que produjo datos y abortar si no** — el relanzamiento
lo hace.
