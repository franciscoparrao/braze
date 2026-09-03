# Pre-registro: ¿sobrevive el gradiente harness×escala en la suite discriminante?

Fecha: 2026-08-30
Antecedente: `docs/variance-decomposition-2026-08-30.md`, hallazgo 4
Estado al escribir esto: **ninguna corrida lanzada.**

## Qué se re-verifica

La descomposición sobre `default.toml` encontró que el rango de pass rate
que el harness mueve **decrece monótonamente con la escala del modelo**:

| ejecutor | rango movido por el harness |
|---|---|
| qwen2.5:3b | 31.1 pp |
| qwen2.5:7b | 14.5 pp |
| qwen3.5-coder | 5.3 pp |

Es la tesis del Paper 1 cuantificada, pero **la propia limitación del
análisis la pone en duda**: `default.toml` está saturada para el coder
(0.994 en base), y un techo comprime mecánicamente su rango. El gradiente
podría ser un artefacto del techo y no una propiedad del harness.

La suite discriminante v2 (34 tareas, ~3 pp por ítem, construida
explícitamente para no saturar) es donde esto se decide.

## Hipótesis

**H1.** El gradiente persiste: el rango que el harness mueve sigue siendo
monótonamente decreciente con la escala del ejecutor, en una suite donde
ningún modelo satura.

**H0.** El gradiente era artefacto de la saturación de `default.toml`: sin
techo, los rangos se igualan o el orden se rompe.

## Diseño

**3 ejecutores × 3 configuraciones × 34 tareas × 2 repeticiones = 612 corridas.**

Ejecutores (los mismos del análisis original, para que la comparación sea
directa): `qwen2.5:3b`, `qwen2.5:7b`, `qwen3.5-coder`.

Configuraciones:

| config | qué es |
|---|---|
| `base` | harness por defecto |
| `+lead:ollama:ornith:9b` | escalación reactiva (palanca que AGREGA) |
| `+ablate:no-rescue` | sin escalera de rescate textual (palanca que QUITA) |

Se corre un sweep por ejecutor, no uno solo con nueve brazos: cada sweep
escribe su propio JSON, así un corte no se lleva las corridas ya hechas.

### Desviación deliberada: el lead cambia de gemma4:e4b a ornith:9b

Los sweeps originales usaron `gemma4:e4b` de lead. **No es replicable en el
hardware disponible**: Nitro tiene 14 GB totales (9 libres al escribir
esto), y `qwen3.5-coder` (6,6) + `gemma4:e4b` (9,6) = 16,2 GB residentes
simultáneos. Ese brazo reproduciría el OOM-kill del servicio Ollama del
2026-08-10 a mitad de sweep.

`ornith:9b` (5,6 GB) deja el peor par en 12,2 GB y es un lead genuinamente
capaz — satura `default.toml` (95/95). **Consecuencia para la
interpretación: esto es una replicación CONCEPTUAL del gradiente, no una
réplica exacta.** Si el gradiente aparece, aparece con otro lead y en otra
suite, lo que lo fortalece; si no aparece, no se podrá distinguir "el
gradiente era artefacto" de "este lead es peor", y habrá que decirlo así.

Se añade `+ablate:no-rescue` justamente por eso: es una palanca que no
carga un segundo modelo, así que su contribución al rango es inmune al
cambio de lead.

## Criterio de decisión, comprometido antes de correr

Métrica: rango = `max(pass_rate) - min(pass_rate)` sobre las 3 configs, por
ejecutor.

- **Orden monótono decreciente (3b > 7b > coder) → H1 sostenida.** El
  hallazgo 4 es citable en el Paper 1 con ambas suites.
- **Orden roto pero 3b claramente por encima del coder → sostenida en su
  forma débil**: "el harness mueve más a los modelos débiles", sin afirmar
  monotonía.
- **Rangos comparables entre los tres (diferencia < 5 pp entre el mayor y
  el menor) → H0.** El gradiente era artefacto del techo, y el hallazgo 4
  se retira del Paper 1. Se publica la corrección.

Cláusula anti-racionalización: si sale H0, **no** se reinterpretará como
"la suite discriminante es demasiado difícil y comprime por abajo". Ese
sería el mismo error de techo con el signo cambiado, y exigiría su propio
pre-registro.

## Amenazas a la validez

- 2 repeticiones: la resolución la fijan los 34 ítems (~3 pp cada uno), las
  réplicas solo estiman el ruido. Suficiente para un rango, insuficiente
  para un intervalo estrecho.
- Un solo lead y una sola ablación: el "rango del harness" se estima con 3
  puntos, no con el espacio de configuraciones.
- Los tres ejecutores son de dos familias (Qwen 2.5 ×2, Qwen 3.5-coder);
  el gradiente confunde escala con familia en el extremo superior.
- Binario único (a diferencia del análisis original, que mezclaba 14
  commits) — esto es una mejora, y hace los resultados NO directamente
  comparables en valor absoluto con los de `default.toml`.
