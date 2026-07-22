# A/B potenciado de la palanca de verificación (H2) — 2026-07-22 (DEFINITIVO)

**Corrige `docs/sweep-verification-lever-ab-2026-07-22.md` (el piloto
n=18).** El piloto sugirió un efecto positivo fuerte (+16pp qwen, +27pp
gemma, 0 reversiones); se reportó explícitamente como *prometedor pero
subpotenciado*. Este run confirmatorio — n comprometido ANTES de correr
(commit a6769c8), 20 bugs de compilación Rust distintos × 3 reps = 60
pares por ejecutor — es el test potenciado del mismo criterio.

## Resultado

| brazo | pass | Wilson 95% | avg_rounds |
|---|---|---|---|
| qwen2.5:3b control | 17/60 (28%) | [19,41]% | 4.5 |
| qwen2.5:3b +gate | 16/60 (27%) | [17,39]% | 5.6 |
| gemma4:e4b control | 55/60 (92%) | [82,96]% | 3.5 |
| gemma4:e4b +gate | 53/60 (88%) | [78,94]% | 3.5 |

McNemar pareado: **qwen solo-gate=9, solo-control=10, p=1.0; gemma
solo-gate=3, solo-control=5, p=0.73.** El gate disparó 90 veces.

## Veredicto: REJECT — el gate NO ayuda (nulo, apenas negativo)

El efecto del piloto **se evaporó**. Con una población de 20 tareas
distintas, el gate recupera aproximadamente tantos fallos como rompe:
- **qwen**: recupera 9 de los 43 fallos del control, pero *rompe* 10
  tareas que el control pasaba → neto −1.
- **gemma**: recupera 3 de 5, pero rompe 5 → neto −2.

El McNemar (p=1.0 / p=0.73) es un nulo resonante: los pares discordantes
están balanceados = ruido. **El piloto fue un falso positivo de muestra
chica** — sus 6 tareas resultaron ser un subconjunto favorable (gemma
baseline 67% ahí vs 92% en la población amplia: las 14 tareas nuevas eran
más fáciles, techo que no deja espacio al gate).

## El mecanismo, matizado (el hallazgo que SÍ sobrevive)

La pregunta profunda del #15 tiene una respuesta más rica que "sí
funciona":
1. **El modelo SÍ actúa sobre el fallo inyectado a veces** — la
   recuperación es real (9/43 qwen, 3/5 gemma). No lo ignora (la forma
   dura del #15 no se materializó).
2. **Pero la ronda extra es un arma de doble filo** (roam #16: más
   rondas pueden envenenar). El modelo a veces arregla, a veces degrada
   su propio trabajo o no puede arreglar el error ni con él delante
   (qwen recupera solo 9 de 43 — la mayoría no las puede arreglar). Más
   GPU non-determinismo entre brazos.
3. **Neto: se cancela.** Forzar la verificación + una ronda de arreglo
   NO es una ganancia neta para modelos débiles en esta población.

## Por qué esto es un buen resultado (y qué valida)

- **La disciplina funcionó exactamente como debe.** El piloto insinuó un
  efecto, se reportó con cautela ("subpotenciado"), el run potenciado lo
  corrigió. Ciencia honesta.
- **La palanca de verificación se une al stencil como NULO**, no como
  positivo — reforzando la tesis del paper 1 ("not all scaffolding
  helps") con MÁS fuerza, no menos.
- **Valida no meterla al paper 1.** Si el piloto positivo se hubiera
  apurado a una subsección del paper 1, se habría publicado un falso
  positivo que este run habría contradicho. Potenciar antes de creer
  salvó al paper de un mal resultado.

## Qué quedaría por explorar (no cambia el nulo actual)

El peldaño H3 (autoridad: no dejar terminar hasta exit 0, en vez de una
sola ronda de gracia) podría diferir — pero la evidencia de que la ronda
extra tanto arregla como rompe sugiere que forzar MÁS rondas empeoraría
el lado del daño, no solo el de la recuperación. Tareas reales
(SWE-bench) en vez de bugs sintéticos es la otra variable. Ninguna
convierte este nulo en positivo por sí sola.
