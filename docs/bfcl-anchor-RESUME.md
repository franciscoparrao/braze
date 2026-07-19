# Ancla BFCL — cómo retomar (estado al 2026-07-18)

Todo lo que precede al sweep está **hecho, commiteado y validado**. Lo
único pendiente es correr el sweep con Nitro sano y analizar.

## Precondición: Nitro sano

El intento del 2026-07-18 se abortó porque Nitro corría ~8× lento
(28.7 s/run en `llama3.2:1b` contra 3.6 s/run de referencia en el sweep
de la curva) por contención con otro proceso del usuario. **Verificar
antes de lanzar**:

```bash
# debe volver en ~1-2s, no en ~30s
time curl -s http://192.168.1.8:11434/api/generate \
  -d '{"model":"llama3.2:1b","prompt":"count to three","stream":false,"options":{"num_predict":20}}' \
  -o /dev/null
# y que no haya nada más consumiendo el nodo
curl -s http://192.168.1.8:11434/api/ps
```

Si el request tarda >5s, **no lanzar**: los números saldrían contaminados
por contención (modo de falla documentado en CLAUDE.md) y hay riesgo de
timeouts espurios contra el límite de 180s.

## Comando exacto

Desde el worktree `~/proyectos/braze-bfcl-anchor` (rama `bfcl-anchor`),
con el binario ya compilado (`cargo build --release -p braze-bench`):

```bash
SCRATCH=/tmp/bfcl && mkdir -p $SCRATCH
rm -rf braze-bench-preserved-sessions
setsid nohup env \
  RUST_LOG=braze_engine=info \
  BRAZE_OLLAMA_BASE_URL=http://192.168.1.8:11434 \
  BRAZE_BENCH_KEEP_SESSIONS=1 \
  BRAZE_OLLAMA_TRANSPORT_RETRIES=6 \
  ./target/release/braze-bench crates/braze-bench/suites/bfcl-anchor.toml \
  --backends "ollama:llama3.2:1b,ollama:llama3.2:1b+lead:ollama:gemma4:e4b,ollama:gemma4:e4b,ollama:qwen2.5:3b,ollama:qwen3.5-coder" \
  --repetitions 5 \
  --output docs/sweep-bfcl-anchor-2026-07-18.json \
  > $SCRATCH/sweep.log 2>&1 &
```

Notas del comando (todas son lecciones de los tres intentos fallidos):

- **`setsid nohup`**: los procesos en background normales del harness
  mueren a los ~45 min; los detached sobreviven.
- **SIN `--no-ollama-stop`**: los 5 brazos suman ~22GB de residentes
  contra los 16GB de Nitro → OOM (causa del intento v1). El default
  detiene cada modelo al cambiar de brazo; pico ~9.2GB.
- **`BRAZE_OLLAMA_TRANSPORT_RETRIES=6`**: absorbe ráfagas de red de
  hasta ~1 min (causa del intento v2).
- **`BRAZE_BENCH_KEEP_SESSIONS=1`**: obligatorio, el grader offline
  lee las transcripciones.
- El log NO muestra `[PASS]/[FAIL]` mientras corre (stdout
  block-buffered); el progreso se sigue con `grep -c '^-> ' $SCRATCH/sweep.log`.

Duración esperada con Nitro sano: **~2h** (1.500 corridas).

## Verificación post-sweep (antes de citar cualquier número)

```bash
python3 - <<'EOF'
import json
d=json.load(open('docs/sweep-bfcl-anchor-2026-07-18.json'))['results']
for a in sorted({r['backend'] for r in d}):
    rows=[r for r in d if r['backend']==a]
    t=[r for r in rows if r['failure_cause']=='model_backend_error' and
       (r['wall_time_ms']<1000 or 'request to model backend failed' in str(r['run_error'])
        or 'stream failed' in str(r['run_error']))]
    print(f"{a[:52]:52} transport={len(t)}/{len(rows)}")
EOF
```

**Regla de descarte pre-registrada**: si algún brazo supera 2% de runs
de transporte, se descarta el sweep y se re-corre. No se aplica
exclusión analítica sobre datos ya vistos.

## Análisis (dos comandos)

```bash
python3 docs/bfcl-anchor-grader-2026-07-18.py --sweep docs/sweep-bfcl-anchor-2026-07-18.json
python3 docs/bfcl-anchor-analysis-2026-07-18.py \
  --sweep docs/sweep-bfcl-anchor-2026-07-18.json \
  --grades docs/sweep-bfcl-anchor-2026-07-18.offline-grades.json
```

El segundo imprime las lecturas E1–E4 pre-declaradas en
`docs/bfcl-anchor-design-2026-07-18.md`.

## Intentos preservados (para el disclosure del paper)

| Archivo | Qué fue |
|---|---|
| `...contaminated-nitro-oom.json` | v1: 1296/1500 fallos de transporte; `--no-ollama-stop` agotó la RAM de Nitro |
| `...contaminated-v2.json` | v2: 1392/1500 fallos; red degradada, aún sin retry |
| `...aborted-v3-nitro-contention.log` | v3: abortado a los 56 runs; sin fallos (el retry funcionó) pero Nitro 8× lento por contención |

## Después del ancla, la cola de Nitro (diseños ya pre-registrados)

1. `docs/rerun-contaminated-cells-design-2026-07-18.md` — bloque 1
   (`1B+plan+lead` + ancla `1B+lead`, 190 runs) y bloque 2 (tres brazos
   coder del planner-ab, 285 runs).
2. `docs/empty-response-template-probe-design-2026-07-18.md` — Parte B
   (80 requests, ~5 min). La Parte A ya está hecha y confirmó prefill en
   llama3.2:1b y qwen2.5:3b (`...-partA-2026-07-18.md`).
