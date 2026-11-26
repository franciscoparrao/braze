#!/usr/bin/env bash
# Qwen3.6 35B-A3B UD-Q4_K_XL (22,9 GB, en Datos): default.toml por llama-server y
# después por Magnitude, mismo archivo, misma configuración que el lote de octubre.
source ~/.cargo/env
cd ~/braze-study2
export LD_LIBRARY_PATH=$(ls -d target/release/build/llama-cpp-sys-2-*/out/build/bin)
export BRAZE_ZEN_API_KEY=local
G=/run/media/franciscoparrao/Datos/models/qwen3.6-35b-a3b/Qwen3.6-35B-A3B-UD-Q4_K_XL.gguf
run() { # $1 etiqueta, $2 base url, $3 modelo
  BRAZE_ZEN_BASE_URL=$2 target/release/braze-bench crates/braze-bench/suites/default.toml \
    --backends "zen:$3" --repetitions 3 --seed 42 --temperature 0.2 --task-timeout-secs 900 \
    --output ~/magnitude-runs/sweep-qwen36-35b-a3b-$1-default-2026-11-25.json \
    >> ~/magnitude-runs/qwen36-$1-default.log 2>&1
  echo "exit $? $(date -Is)" >> ~/magnitude-runs/qwen36-$1-default.log
}
run llamaserver http://127.0.0.1:8092/v1 qwen3.6-35b-a3b
pkill -x llama-server; sleep 5
setsid nohup ~/.magnitude/bin/magnitude serve > ~/.magnitude/logs/serve-manual.log 2>&1 < /dev/null &
sleep 20
~/.magnitude/bin/magnitude models load local:qwen3.6-35b-a3b-q4_k_m:4fd8c90d3aa4
for i in $(seq 1 120); do ~/.magnitude/bin/magnitude status 2>&1 | grep -q Loading || break; sleep 5; done
run magnitude http://127.0.0.1:10100/inference/v1 local:qwen3.6-35b-a3b-q4_k_m:4fd8c90d3aa4
