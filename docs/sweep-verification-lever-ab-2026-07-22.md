# A/B de la palanca de verificación (H2) — 2026-07-22

**Pregunta**: ¿el gate de verificación de fin de turno (correr `cargo
check` antes de aceptar el turno, inyectar el fallo real y dar una ronda
de arreglo) mejora el pass rate en ejecutores débiles — y, la pregunta
profunda del #15, un modelo que declara "listo" **actúa** sobre el fallo
inyectado o lo ignora?

**Diseño** (pre-registrado, `docs/verification-lever-design-2026-07-22.md`,
criterio commiteado en 96472fe ANTES del sweep; enmienda de suite en
4b46403 también antes): suite `verification-lever.toml` (6 errores de
compilación Rust: borrow/move/tipo/mut, verificados que fallan `cargo
check`), ejecutores `qwen2.5:3b` y `gemma4:e4b` por el LocalBackend en
Nitro (GPU), control vs `+ablate:verify-gate` (max_rounds=2, comando
`cargo check`), reps=3, seed=42, McNemar pareado por (tarea, rep).
Datos: `sweep-verify-ab.json`.

## Resultado

| brazo | pass | Wilson 95% | avg_rounds |
|---|---|---|---|
| qwen2.5:3b control | 3/18 (17%) | [6,39]% | 4.3 |
| qwen2.5:3b **+gate** | **6/18 (33%)** | [16,56]% | 5.7 |
| gemma4:e4b control | 12/18 (67%) | [44,84]% | 3.5 |
| gemma4:e4b **+gate** | **17/18 (94%)** | [74,99]% | 3.3 |

McNemar pareado: qwen solo-gate=3, solo-control=**0**, p=0.250; gemma
solo-gate=5, solo-control=**0**, p=0.062. **Todos los pares discordantes
favorecen al gate; cero casos en que el gate empeoró.**

## Lectura — POSITIVO, con la potencia como límite honesto

1. **El gate ayuda, en la dirección correcta siempre.** +16pp en qwen,
   +27pp en gemma, y en las 8 tareas donde algún brazo se movió, el gate
   ganó — nunca perdió. Contrasta netamente con el nulo triple del
   stencil: acá el efecto es direccional y consistente.
2. **La pregunta profunda del #15 se responde: el modelo SÍ recupera.**
   El miedo era que un modelo que confabula "listo" ignorara la
   observación inyectada (forma dura del #15, que habría exigido H3). No
   pasó: de los fallos del control, el gate recuperó **5/6 en gemma** y
   **3/15 en qwen**. El modelo *actúa* sobre el `cargo check` real que se
   le inyecta. La recuperación escala con la capacidad de *usar* el
   feedback: gemma (más capaz) arregla casi todo lo que ve; qwen (más
   débil) a menudo no puede arreglarlo ni con el error delante — el gate
   le da la verdad, pero no la habilidad.
3. **Sin costo apreciable.** Rondas: qwen 4.3→5.7 (1.33×), gemma
   3.5→3.3 (¡menos!). Bajo el techo de 1.5× del criterio. El gate
   disparó 24 veces en total; 8 terminaron en recuperación.

**Contra el criterio pre-registrado (ADOPT si las tres):**
- (2) mecanismo verifica (gate disparó y recuperó > 0): **cumplido con
  holgura** (8 recuperaciones).
- (3) costo de rondas ≤ 1.5×: **cumplido** (1.33× / 0.94×).
- (1) pass +5pp con IC del delta fuera de cero: el +5pp se cumple de
  sobra (+16/+27pp), pero a n=18 por brazo el test pareado es **marginal**
  (gemma p=0.062, qwen p=0.250) — el IC del delta roza cero. La
  dirección es limpísima; la potencia no alcanza para significancia
  formal.

**Veredicto: PROMETEDOR, subpotenciado — no un ADOPT limpio (significancia
marginal), decididamente no un REJECT (dirección impecable + mecanismo
verificado + costo nulo).** A diferencia del stencil (nulo genuino), acá
hay señal real que solo pide más n. El paso siguiente que estos datos
piden: subir tareas/reps para clavar la significancia — con el efecto tan
consistente (0 reversiones en 8 movimientos), n≈40-50 tareas debería
cruzar el umbral.

## Por qué importa para el fin último (herramienta local sin nube)

Esta es la primera palanca de **confiabilidad** medida que **sube el pass
rate de un modelo local débil** sin costo — model-agnostic, y verificada
sobre modelos locales en hardware local. Es evidencia directa de que el
eje confiabilidad del harness (a diferencia del eje capacidad, saturado)
sigue abierto y sirve a "una herramienta de la que puedo depender".
