# Sweep A/B del impuesto JSON (edit-fence) — RECHAZADO

Fecha: 2026-08-10
Pre-registro: `docs/hypothesis-2026-08-10-json-tax-edit-fence.md`
Análisis: `docs/json-tax-analysis-2026-08-10.py` (congelado en `92850d9`
ANTES de que existiera el JSON — verificable en el mensaje del commit)
Datos: `docs/sweep-json-tax-edit-fence-2026-08-10.json` (760 corridas,
`default.toml`, 4 executors × 2 brazos × 19 tareas × 5 reps, seed 42,
temp 0.2, Nitro vía Ollama, `--no-ollama-stop`)

## Veredicto: RECHAZAR, por el criterio pre-registrado exacto

B ≤ A en los tres executors débiles — la condición de rechazo se cumple
sin ambigüedad. La cláusula de iteración única (extender el fence a
`write_file` si su JSON roto era el modo de falla dominante) NO se
dispara: las `write_file` del brazo B funcionaron bien; el modo de
falla fue otro (ver mecanismo). No se itera.

## Paso 1 — contaminación: limpia, pero con el hallazgo central

| executor | fence_edits | edit_file total | fuga nativa | write_file |
|---|---|---|---|---|
| llama3.2:1b | **0** | 0 | 0 | 5 |
| qwen2.5:3b | **0** | 0 | 0 | 27 |
| gemma4:e4b | 16 | 16 | 0 | 21 |
| gpt-oss:20b | 40 | 40 | 0 | 25 |

Cero fuga nativa en los cuatro (ningún modelo llamó `edit_file` por
nombre memorizado): el brazo B midió lo que decía medir. Pero los dos
chicos **jamás emitieron un fence válido** — `fence_edits=0` — y
evadieron por `write_file` (reescritura completa, que siguió en JSON).
La condición de mecanismo para adoptar (`fence_edits > 0`) solo se
cumplió en gemma4:e4b y gpt-oss:20b… que ya estaban saturados.

## Paso 2 — pass rates pareados (B − A)

| executor | A | B | Δ | IC95 Newcombe | discordantes B+/A+ | McNemar p |
|---|---|---|---|---|---|---|
| llama3.2:1b | 24/95 | 20/95 | −4,2pp | [−16,1, +7,8] | 6/10 | 0,45 |
| qwen2.5:3b | 72/95 | 64/95 | −8,4pp | [−20,9, +4,4] | 3/11 | 0,057 |
| gemma4:e4b | 90/95 | 89/95 | −1,1pp | [−8,5, +6,3] | 0/1 | 1,0 |
| gpt-oss:20b* | 94/95 | 94/95 | +0,0pp | [−4,8, +4,8] | 1/1 | 1,0 |

\* control saturado, fuera del criterio por pre-registro.

## Paso 3 — el daño se concentra exactamente en `edit`

| executor | edit (n=15) | create (n=20) | other (n=60) |
|---|---|---|---|
| llama3.2:1b | **−6** | +0 | +2 |
| qwen2.5:3b | **−6** | −1 | −1 |
| gemma4:e4b | +0 | −1 | +0 |
| gpt-oss:20b | −1 | +0 | +1 |

Sin daño distractor del addendum en `other` (±2 máximo): el costo del
brazo no fue el prompt más largo, fue la pérdida del canal.

## Mecanismo — la hipótesis estaba invertida para esta clase de modelo

La premisa (heredada de aider/SWE-agent) era que envolver código en
JSON es un impuesto y el texto plano lo elimina. Lo medido: para los
SLM tool-tuned del proyecto, **el JSON de tool-calls es su modalidad
entrenada y el fence SEARCH/REPLACE es una gramática que no saben
emitir** (llama3.2:1b y qwen2.5:3b: cero fences válidos en 190
corridas del brazo B). Quitarles `edit_file` del inventario no les
quitó un impuesto: les quitó la herramienta que sí dominaban, y las 6
tareas de edición que cada uno perdió son exactamente eso. Los modelos
que SÍ emiten el fence (gemma4, gpt-oss) no lo necesitaban.

El contraste con aider no es contradicción sino condición de contorno:
aider mide modelos grandes entrenados en su formato de edición; braze
mide SLM fine-tuneados para function calling. El "impuesto JSON"
existe río arriba en otra población de modelos — en esta, la lengua
materna es el JSON.

## Posición en la serie de nulos del proyecto

Tercer nulo consistente de la misma familia: constrained decoding
(RECHAZADO — prevención en el decoder no paga), stencil GBNF (empate
×3 — la reparación río abajo ya absorbe los schema_fail), y ahora
edit-fence (RECHAZADO — cambiar el canal de transporte tampoco paga).
Los tres apuntan al mismo lugar: **la capa de reparación del harness
ya cubre la clase sintáctica, y las intervenciones río arriba compiten
contra un problema que dejó de existir**. Para el paper: § ablations /
discusión, citado junto a los otros dos.

## Notas de régimen

- gemma4:e4b marcó A=90/95 (94,7%), por sobre su 93,7% histórico del
  13-jul — consistente con la ventana del fix de Ollama 0.32.1; no es
  parte de este A/B pero alimenta el pendiente "A/B Gemma4 runtime".
- El sweep corrió con builds/tests locales suspendidos (solo
  orquestación local; inferencia en Nitro), sin filas [Timeout]
  anómalas.
- Ninguna regla del pre-registro se modificó después de correr el
  sweep; el análisis estaba commiteado antes de que el JSON existiera.
