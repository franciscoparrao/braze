#!/usr/bin/env bash
# Qwen3.6 35B-A3B UD-Q4_K_XL desde Datos por llama-server: suite discriminante,
# misma configuración que el lote de octubre.
source ~/.cargo/env
cd ~/llama.cpp-inkling
setsid nohup build/bin/llama-server -m /run/media/franciscoparrao/Datos/models/qwen3.6-35b-a3b/Qwen3.6-35B-A3B-UD-Q4_K_XL.gguf \
  --host 127.0.0.1 --port 8092 -c 65536 --jinja -np 1 --alias qwen3.6-35b-a3b \
  > ~/magnitude-runs/qwen36-llamaserver-server.log 2>&1 < /dev/null &
for i in $(seq 1 90); do curl -s -m 2 127.0.0.1:8092/health | grep -q ok && break; sleep 5; done
cd ~/braze-study2
export LD_LIBRARY_PATH=$(ls -d target/release/build/llama-cpp-sys-2-*/out/build/bin)
export BRAZE_ZEN_API_KEY=local BRAZE_ZEN_BASE_URL=http://127.0.0.1:8092/v1
target/release/braze-bench crates/braze-bench/suites/discriminating.toml \
  --backends zen:qwen3.6-35b-a3b --repetitions 3 --seed 42 --temperature 0.2 --task-timeout-secs 900 \
  --output ~/magnitude-runs/sweep-qwen36-35b-a3b-llamaserver-discriminating-2026-11-25.json \
  >> ~/magnitude-runs/qwen36-llamaserver-discriminating.log 2>&1
echo "exit $? $(date -Is)" >> ~/magnitude-runs/qwen36-llamaserver-discriminating.log
