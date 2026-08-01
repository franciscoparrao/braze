# Procedencia de los sweeps en formato pre-metadata — v9 L-7

Fecha: 2026-08-01. Cierra el ítem L-7 de `docs/AUDITORIA-2026-07-v9.md`.

Siete archivos de resultados en `docs/` son **arrays JSON del formato
original del bench**, anterior al envoltorio `{"metadata": ..., "runs": ...}`
que hoy embebe el sampling, el commit y el fingerprint de suite en el propio
archivo. Son JSON válidos y sus corridas son íntegras; lo que no llevan
adentro es la configuración de sampling con que corrieron. Este documento la
deja fijada, **verificada contra el código del bench en el commit de cada
época** (`git show <commit>:crates/braze-bench/src/main.rs`), no reconstruida
de memoria.

| Archivo | Fecha | Sampling real | Cómo se verificó |
|---|---|---|---|
| `sweep-deepseek-v4-flash.json` | 2026-07-05 | **default del proveedor** (OpenRouter/DeepSeek server-side; braze-bench aún NO tenía flags de sampling) | `main.rs` en `853335e` (último commit del 05-jul): cero menciones de `temperature` en todo braze-bench |
| `sweep-nitro-sampling-2026-07-06/nitro-q25-t02.json` | 2026-07-06 | qwen2.5:3b, temp 0.2 | el sweep ES el A/B de sampling; el brazo está en el nombre (`t02`) y en su `.log` |
| `sweep-nitro-sampling-2026-07-06/nitro-q25-rec.json` | 2026-07-06 | qwen2.5:3b, receta Qwen: temp 0.7 / top-p 0.8 / top-k 20 / repeat-penalty 1.05 | ídem (`rec` = recomendado por Qwen; CLAUDE.md § modelos locales) |
| `sweep-nitro-sampling-2026-07-06/nitro-q35c-t02.json` | 2026-07-06 | qwen3.5-coder, temp 0.2 | ídem |
| `sweep-nitro-sampling-2026-07-06/nitro-q35c-rec.json` | 2026-07-06 | qwen3.5-coder, receta Qwen (como arriba) | ídem |
| `sweep-planner-ab.json` | 2026-07-11 | **temp 0.2** (default del flag `--temperature`), **sin seed** (default `None` → sampling no determinístico del proveedor; las repeticiones son independientes) | `main.rs` en `28f7a53` (el commit que la tabla del paper cita para planner-ab): `default_value_t = 0.2`; `seed: Option<u64>` sin default |
| `sweep-bfcl-anchor-2026-07-18.offline-grades.json` | 2026-07-18 | no aplica — es el archivo de **grades offline** del ancla BFCL (salida del grader AST), no una corrida de modelo | su generador es `docs/bfcl-anchor-analysis-2026-07-18.py`; el sampling de las corridas que califica vive en los JSON del sweep BFCL, formato nuevo |

## Las dos aclaraciones que importan

1. **El sweep de deepseek NO corrió a 0.2.** La suposición natural ("el
   default del bench siempre fue 0.2") es falsa para el 05-jul: los flags de
   sampling se agregaron el 06-jul (backlog 1-7, CLAUDE.md). Cualquier
   comparación futura contra ese sweep debe tratar su sampling como el
   default server-side del proveedor de esa fecha. El sweep no respalda
   ningún claim del manuscrito (la tabla del paper no lo lista); lo citan
   CLAUDE.md y PLAN.md como recomendación operativa de modelo.

2. **`sweep-planner-ab.json` sí respalda una sección del paper**
   (\S planner, tabla de sweeps, commit `28f7a53`), y su régimen queda ahora
   fijado por esta nota: temp 0.2, sin seed, repeticiones independientes —
   el mismo régimen que los 38 sweeps del formato nuevo registran embebido
   (`temperature=0.2, seed=None`), verificado en la auditoría v9 § L-7/L-9.

No se modifican los JSON: reescribir evidencia primaria para agregarle
metadata inventaría una procedencia que el archivo nunca tuvo. La nota vive
al lado, con el método de verificación a la vista.
