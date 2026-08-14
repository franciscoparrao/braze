# Fase 0: dsh (DeepSeek Harness) como baseline externo — factibilidad y primer hallazgo

**Fecha**: 2026-08-13. **Contexto**: DeepSeek publicó hoy
`deepseek-ai/deepseek-harness` (dsh, TypeScript/Cordis, MIT,
"everything is a plugin"). Su perfil `headless` (`dsh --profile
headless "<task>"` → última respuesta por stdout, exit 0/1) calza con
el contrato `--external` de braze-bench (precedente: adapter
bare-lead, EMSE Issue 1). Plan de 3 fases: (0) factibilidad manual
contra Ollama/Nitro, (1) adapter `dsh:` en `external.rs`, (2) sweep
pareado harness-vs-harness con los mismos modelos locales.

## Setup que funcionó (plomería)

- `npx -y @deepseek-ai/dsh --profile headless --patch <overlay>` con
  Node 22.
- Overlay: fila `llm-deepseek` con `baseURL:
  http://192.168.1.8:11434/v1` (endpoint OpenAI-compatible de Ollama),
  `thinking: disabled`, `maxTokens: 2048`; fila `agent-default-model`
  con `provider: deepseek-official, model: <id ollama>` (ids no
  listados pasan tal cual al wire); `DEEPSEEK_API_KEY=ollama` (dummy).
- Permisos: definir un preset propio (`bench`: sandbox
  `workspace-write` + approval `never`) y `defaultPreset: bench` en la
  fila `permission` — parchear solo `approval.policy` rompe la
  composición de presets ("match no preset").
- El log de sesión queda en `~/.dsh/sessions/<workspace>/<id>/
  session.jsonl.zstd` — event-sourced, legible, con `request/header`
  incluyendo el system prompt. Buena observabilidad para el adapter.

## El hallazgo: la clase de fallo #1 de braze, reproducida en dsh

Tarea trivial de tools ("¿cuántas líneas tiene notas.txt? responde
solo el número", archivo de 3 líneas en el workspace), dos modelos que
en braze la resuelven al 100%:

| Modelo | En braze (default.toml) | En dsh headless |
|---|---|---|
| ornith:9b | 95/95 funcional | 0 tool calls; fabula un "Desktop" con `.DS_Store` inexistente; exit 0 |
| gpt-oss:20b | 95/95, pass^5=100% | responde una pregunta sobre dropdowns HTML que nadie hizo; exit 0 |

Mecanismo confirmado en el journal de Ollama (Nitro, 0.32.1):

```
WARN "truncating input prompt" limit=2050 prompt=6394 keep=4 new=2050
```

El prompt de dsh (~6.4k tokens: system de Code Mode + runtime context
+ inventario de skills) excede el `n_ctx=4096` con que Ollama carga el
modelo vía `/v1` (dsh no fija `num_ctx`; el endpoint OpenAI de Ollama
no lo acepta por-request) → truncamiento frontal SILENCIOSO → el
modelo ve un fragmento del medio del prompt, sin la tarea → alucina
una respuesta coherente con el fragmento → **dsh reporta exit 0
"completed"** (su contrato es quiescencia del turno, no logro). La
corrida de ornith también fue truncada (sus 2.050 tokens exactos son
el límite), no solo la de gpt-oss.

Lectura para el paper (harness-como-variable, en vivo y con artefacto
de alto perfil del mismo día): el mismo modelo pasa de 100% a 0% al
cambiar SOLO el harness, y el mecanismo es precisamente una palanca
que braze implementa y dsh no — presupuesto de contexto explícito
(`DEFAULT_NUM_CTX=8192` + detección de truncamiento en
`OllamaStreamState` + grading por resultado). No es un juicio de dsh
como producto: es developer preview de día 1, su camino soportado son
las APIs DeepSeek (ventanas de 1M), y el modo local vía baseURL es
off-label. El punto es la *postura de diseño*: un harness diseñado
para modelos frontera no degrada con gracia hacia el régimen
SLM/local, y ni siquiera detecta que degradó.

## Gates para continuar

- **Para re-correr Fase 0 limpia**: subir `OLLAMA_CONTEXT_LENGTH` en
  Nitro (env del servicio, sudo del usuario; costo en KV cache — con
  14Gi evaluar 8192-16384) para que los ~6.4k del prompt de dsh
  quepan. Recién ahí se mide la pregunta real: ¿funciona Code Mode
  (ejecución vía código, la apuesta de modalidad de dsh) con SLMs
  tool-tuned, o se confirma la lección del A/B edit-fence (la
  modalidad entrenada es la confiable)?
- **Fase 1 (adapter `dsh:`)**: BLOQUEADA hasta que Fase 0 pase con al
  menos un modelo local. La plomería ya está probada; el adapter es
  calco del bare-lead.
- Caveat de comparabilidad para Fase 2: dsh no expone seed por request
  → el pareo McNemar pierde la semilla compartida (mismo caveat ya
  documentado del flujo external).

## Notas sueltas

- dsh detectó e inyectó un inventario de skills en el prompt (~el
  catálogo de skills del home del usuario) — contribuyó a los 6.4k
  tokens. Para el adapter, correr con HOME/perfil limpio.
- `agent-default-model` con id de modelo Ollama funciona; el catálogo
  de modelos de dsh es advisory (pass-through).
- Los timestamps del turno y el `request/header` del session log
  permitirían reconstruir el request exacto — útil si Fase 2 necesita
  auditar qué vio el modelo.
