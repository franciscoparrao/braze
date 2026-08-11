# Sweep A/B lead-summary + TTC — TTC RECHAZADO; lead-summary NO-ADOPTADO con señal direccional

Fecha: 2026-08-10
Pre-registro: `docs/hypothesis-2026-08-10-lead-summary-ttc.md` (criterios
congelados antes del sweep; commit `5c313e2`)
Datos: `docs/sweep-lead-summary-ttc-2026-08-10.json` (brazos qwen,
completados antes del incidente) + `docs/sweep-lead-summary-ttc-2026-08-10b.json`
(re-run de los 4 brazos restantes). Seed 42 en ambas invocaciones — el
pareo por (tarea, repetición) sobrevive el corte por diseño.

## El incidente que partió el sweep en dos

A las 20:03, a mitad del sweep original, el kernel de Nitro hizo
OOM-kill del servicio Ollama (evidencia: `journalctl -u ollama` —
"Failed with result 'oom-kill'", restart counter 1). El circuit breaker
de braze abrió contra `qwen2.5:7b` tras 5 fallos de transporte y
clasificó el resto como HarnessError **fuera del denominador**: la
estadística sobrevivió intacta, el sweep no. Los brazos qwen (1-2)
habían completado antes; los otros 4 se re-corrieron con Nitro limpio y
SIN `--no-ollama-stop` (regla operativa nueva en CLAUDE.md: ese flag es
solo para sweeps de modelo único). Cero `run_error` en el re-run.

## Validación de mecanismo (pre-registrada, antes de pass rates)

| chequeo | resultado |
|---|---|
| compactación en fila 5 (digest) | 31 compactaciones, 25/95 tareas — ✓ |
| compactación en fila 6 (lead-summary) | 37 compactaciones, 25/95 tareas — ✓ comparable |
| TTC qwen: rollouts reales | 95/95 filas con `ttc_rollouts`, 2,95× tokens — ✓ |
| TTC llama: rollouts reales | 95/95, 6,59× tokens — ✓ |

## TTC local (`+ablate:ttc=3`): **RECHAZADO**

| executor | A (baseline) | B (ttc=3) | Δ | IC95 Newcombe | disc B+/A+ | McNemar p |
|---|---|---|---|---|---|---|
| qwen2.5:3b | 72/95 | 72/95 | +0,0pp | [−12,1, +12,1] | 3/3 | 1,0 |
| llama3.2:1b | 19/95 | 11/95 | **−8,4pp** | [−18,8, +2,1] | **0/8** | **0,0078** |

Criterio: "rechazar si Δ ≤ 0 en ambos" — cumplido, y con hallazgo de
mecanismo: en llama el TTC no es neutro, es **significativamente
dañino** (los 8 pares discordantes favorecen TODOS al baseline, p
exacto 0,0078). La auto-consistencia por `outcome_fingerprint` en un
modelo débil vota por el modo de la distribución — y el modo de un
modelo débil es con frecuencia un error *estable* (outputs degenerados
que coinciden entre sí), que le gana la votación al intento único
correcto. Es el riesgo que el pre-registro anotó ("fingerprints
degeneran") elevado a resultado principal: **comprar cómputo con
votación exige que el acierto sea más consistente que el error, y en
los débiles es al revés.** Triple costo, confiabilidad negativa.

## Summary-por-lead (`+ablate:lead-summary`): **NO-ADOPTADO, señal direccional positiva**

| par | A (fila 5: digest) | B (fila 6: lead-summary) | Δ | IC95 | disc B+/A+ | McNemar p |
|---|---|---|---|---|---|---|
| qwen3b+lead:7b, thr=4 | 65/95 | 71/95 | +6,3pp | [−6,5, +18,8] | **6/0** | 0,0312 |

Ni adopción (pide ≥ +10pp con IC fuera de cero; +6,3pp con IC cruzando
cero no llega) ni rechazo (6 > 5). El pre-registro no declaró
iteración: **no-adoptado y punto**. Pero la señal merece registro
honesto: los 6 discordantes favorecen TODOS al lead-summary (p exacto
0,031) — cuando la compactación pierde algo que importaba, el summary
del lead lo pierde menos que el digest extractivo. Si alguna vez se
revisita, el camino es un banco con MÁS presión de compactación (aquí
solo 25/95 tareas compactaron — el efecto solo puede vivir ahí, y ahí
el estimado local sería mayor que el +6,3pp diluido).

Contexto del costo (informativo, cross-sweep): la fila 5 vs el baseline
qwen sin lead ni compactación agresiva: −7,4pp [−19,8, +5,4] —
compactar cada 4 eventos cuesta contexto, como anticipó el riesgo del
pre-registro. El contraste 6−5 aísla la fuente del summary bajo esa
presión idéntica, que es lo único que este A/B decide.

## Corrección documental que este sweep salda

`docs/techniques-roadmap-2026-08-06.md` decía "prior débil tras el nulo
del lead-summary" — una afirmación SIN sweep detrás (no existía en repo
ni en Nitro; verificado antes de correr). El dato real es "no-adoptado
por tamaño, dirección positiva": el prior para selección submodular de
compactación queda si acaso algo MENOS débil de lo que ese doc asumió.
Corregido en el propio roadmap con puntero acá.

## Posición en el mapa de palancas

Con esto, **todas las palancas implementadas del proyecto tienen
veredicto medido**: adoptadas (rescate, colapso ACI, post-edit check,
gate sintáctico, caching, …), rechazadas (constrained decoding, stencil
n.s., H2 potenciado, edit-fence, TTC), y no-adoptadas con señal
(lead-summary). El inventario palanca-por-palanca ya no tiene casillas
vacías — material directo para la tabla de ablaciones del paper de
seguimiento.
