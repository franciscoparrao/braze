# A/B serving-stack (zen/llama-server vs LocalBackend) — DIFERIDO a Nitro 32GB

Fecha: 2026-09-13/14
Pre-registro: `docs/hypothesis-2026-09-07-serving-stack-ab.md`
Estado: **INCOMPLETO. Brazo local válido; brazo zen inválido por OOM de RAM.
DIFERIDO al upgrade de Nitro a 32GB (decisión del autor 2026-09-14).**
Sin veredicto H1/H0 — datos insuficientes.

## Qué se corrió

Ambos brazos sobre Nitro, binario braze-bench @`82fe1cb` compilado con
`--features local-cuda` (ver desviación 1), suite `discriminating.toml`
(34 tareas), 3 reps, seed 42, temp 0.2, contexto 32768, output cap 12288,
task-timeout 900s.

| brazo | stack | -ngl | resultado |
|---|---|---|---|
| **local** | LocalBackend in-process, `llama-cpp-2 0.1.152+cuda`, Harmony de braze | auto-fit → 25 (KV en VRAM) | **COMPLETO: 82/102 = 80,4%**, 0 harness errors |
| **zen** | `llama-server` fork `a698f1c` `--jinja`, backend zen | 8 (ver desviación 2) | **INVÁLIDO: OOM-kill a 46/102** |

## Por qué el brazo zen es inválido

El `llama-server` fue **matado por el OOM-killer del SO** a las 13:22:41
(dmesg: `Out of memory: Killed process ... (llama-server) anon-rss:7802552kB`,
`global_oom` en la máquina de 14GB). Tras eso, 5 fallos de transporte
consecutivos abrieron el circuit breaker y el bench hizo fail-fast del brazo.

De las 46 celdas escritas, **29 son artefactos del server muriéndose**
(ModelBackendError "connection refused" / HarnessError circuit-breaker),
solo **17 limpias**. Causa raíz: el brazo zen tiene dos consumidores de RAM
—el server gpt-oss residente (~7.8GB a `-ngl 8`, con casi todos los pesos en
RAM) más el proceso del bench y los picos de `cargo check` de las tareas de
edición— y 14GB no alcanzan. El brazo local sobrevivió porque `llama-cpp-2`
in-process con auto-fit puso más pesos en GPU y es un solo proceso.

**Es el muro de 14GB de RAM de Nitro**, ya conocido (mató antes el brazo
`qwen3.5-coder`; es el argumento pre-existente para subir a 32GB — ver
CLAUDE.md § LocalBackend/Gemma).

## Preview (NO es veredicto)

Sobre las 17 celdas limpias del brazo zen, pareadas contra local:
- local **9/17 (53%)** vs **zen 14/17 (82%)**, discordantes zen>local=6,
  local>zen=1, neto +5 pro-zen, **McNemar p=0.125** (no significativo).

Direccionalmente consistente con el disparador del pre-registro (zen >
local), pero **muy por debajo** del MDE declarado (~11 pares netos sobre
~102 celdas). n=17 no decide nada. **No leer esto como resultado.**

(El pareo ingenuo sobre las 46 celdas SIN excluir los artefactos da
"local gana p=0.009" — es engañoso: cuenta como fallos de zen las celdas
donde el server ya estaba muerto. Documentado para que nadie lo cite.)

## Nota sobre el nivel absoluto

El brazo local dio **80,4%**, muy por encima del 57,8% histórico de Study 2
(mismo modelo, misma suite, LocalBackend). Es el confound #1 que el propio
pre-registro nombró: el harness mejoró entre agosto y septiembre (P1.1/L-5
y otros). Por eso el A/B congela el binario y compara local-vs-zen a
binario igual — exactamente lo que quedó a medias.

## Desviaciones del pre-registro (declaradas)

1. **Binario `82fe1cb`, no `93fa19e`.** Gana los campos de metadata
   `zen_base_url`/`ollama_num_ctx` que este mismo A/B estrena (verificado:
   el JSON del brazo zen registró `zen_base_url: http://127.0.0.1:8090`).
   Cambio metadata-only, sin delta de comportamiento. Usado idéntico en
   ambos brazos, así que la comparación interna se preserva.
2. **Paridad de `-ngl` imposible entre stacks.** El `llama-cpp-2`
   in-process cupo con auto-fit "25" en ~5GB VRAM; el fork zen a `-ngl 25`
   intenta ~11GB (maneja la memoria GPU de gpt-oss MXFP4 distinto) → OOM de
   VRAM. Se bajó zen a `-ngl 8` (el ballpark que el pre-registro anticipó).
   El pre-registro prioriza paridad de contexto (32k, preservada) sobre
   paridad de offload; wall-time no es endpoint. Efecto colateral: a `-ngl 8`
   el server carga ~7.8GB en RAM → el OOM de arriba.

## Qué falta y cómo retomar (sin re-pre-registrar)

Re-correr **solo el brazo zen** (local ya es válido y reutilizable) con el
MISMO diseño, sobre Nitro con **32GB de RAM**, donde server + bench + cargo
coexisten sin OOM. Cláusula anti-racionalización del pre-registro
respetada: no se re-corre con KV-quant ni otros params "para encontrar el
efecto" — se difiere al hardware que el diseño necesitaba. Al completar:
pareo McNemar + sign test local-vs-zen sobre las 102 celdas, y veredicto
H1/H0.

## Procedencia

- Datos: `docs/ab-serving-armA-local-2026-09-13.json` (102 celdas, válido),
  `docs/ab-serving-armB-zen-2026-09-13.json` (46 celdas, inválido) + `.log`.
- Binario @`82fe1cb`, `llama-cpp-2 0.1.152+cuda`, GGUF
  `gpt-oss-20b-MXFP4.gguf`, zen fork `a698f1c`.
- OOM confirmado en `dmesg`/`journalctl -k` de Nitro (13:22:41, 2026-09-13).
