#!/usr/bin/env bash
# braze (TUI por defecto) contra Qwen3.5-9B Q3_K_M servido por llama-server en Nitro.
#
# Es la configuración que dio 100/102 en discriminating.toml a 63 s/tarea
# (docs/sweep-qwen35-9b-q3km-llamacpp-discriminating-2026-10-21.json): modelo
# entero en la RTX 3050 (-ngl 99), contexto 32k, plantilla jinja.
#
# 1. Asegura el servidor en Nitro (tmux `qwensrv`, loopback 127.0.0.1:8080).
# 2. Abre un túnel SSH local (puerto 18080) si no hay uno vivo. Nitro no
#    expone el puerto a la LAN (ufw), así que el túnel es la vía.
# 3. Lanza braze con el backend `zen` (cliente OpenAI-compatible) apuntado al
#    túnel. El perfil operador (~/.config/braze) no cambia: solo se pisan
#    BRAZE_ZEN_BASE_URL y BRAZE_ZEN_API_KEY para este proceso.
#
# Uso:  scripts/braze-qwen-nitro.sh            # TUI
#       scripts/braze-qwen-nitro.sh run "…"    # un turno, sin TUI
#       QWEN_GGUF=Qwen3.5-9B-Q4_K_M.gguf NGL=24 scripts/braze-qwen-nitro.sh
#       (Q4_K_M: 101/102 pero 114 s/tarea; no cabe entera en la GPU)
set -euo pipefail

NITRO=${NITRO:-192.168.1.8}
LPORT=${LPORT:-18080}
QWEN_GGUF=${QWEN_GGUF:-Qwen3.5-9B-Q3_K_M.gguf}
NGL=${NGL:-99}
CTX=${CTX:-32768}
BRAZE=${BRAZE:-$HOME/proyectos/braze/target/release/braze}
MODEL_LABEL=${MODEL_LABEL:-qwen3.5-9b-nitro}

[ -x "$BRAZE" ] || { echo "no encuentro el binario de braze en $BRAZE (cargo build --release -p braze-cli)" >&2; exit 1; }

# 1. Servidor en Nitro. Si ya corre con otro modelo, se respeta: matarlo es decisión del usuario.
ssh -o ConnectTimeout=10 "$NITRO" "tmux has-session -t qwensrv 2>/dev/null || \
  tmux new -d -s qwensrv '~/bonsai-llama/build/bin/llama-server -m ~/models/$QWEN_GGUF -ngl $NGL -c $CTX --jinja --host 127.0.0.1 --port 8080 > ~/qwensrv.log 2>&1'; \
  for i in \$(seq 90); do curl -sf localhost:8080/health >/dev/null && exit 0; sleep 2; done; \
  echo 'llama-server no levantó; ver ~/qwensrv.log en Nitro' >&2; exit 1"

# 2. Túnel local, reusado si ya está vivo.
if ! curl -sf "http://127.0.0.1:$LPORT/health" >/dev/null 2>&1; then
  ssh -f -N -o ExitOnForwardFailure=yes -o ServerAliveInterval=30 -L "$LPORT:127.0.0.1:8080" "$NITRO"
  for i in $(seq 20); do curl -sf "http://127.0.0.1:$LPORT/health" >/dev/null 2>&1 && break; sleep 0.5; done
fi
curl -sf "http://127.0.0.1:$LPORT/health" >/dev/null || { echo "el túnel a Nitro no responde en :$LPORT" >&2; exit 1; }

# 3. braze.
export BRAZE_ZEN_BASE_URL="http://127.0.0.1:$LPORT/v1" BRAZE_ZEN_API_KEY=local
if [ "${1:-}" = "run" ]; then
  shift
  exec "$BRAZE" run --backend zen --model "$MODEL_LABEL" "$@"
fi
exec "$BRAZE" chat --tui --backend zen --model "$MODEL_LABEL" "$@"
