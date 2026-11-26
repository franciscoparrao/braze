#!/usr/bin/env bash
# Reintento de Qwen3.6 por Magnitude tras el 503 "server overloaded" (sin memoria para un
# búfer). Magnitude reiniciado limpio; default.toml desde cero en un archivo nuevo.
source ~/.cargo/env
cd ~/braze-study2
export LD_LIBRARY_PATH=$(ls -d target/release/build/llama-cpp-sys-2-*/out/build/bin)
export BRAZE_ZEN_API_KEY=local BRAZE_ZEN_BASE_URL=http://127.0.0.1:10100/inference/v1
M=~/.magnitude/bin/magnitude
$M models stop; for p in $(pgrep -x magnitude); do kill $p; done; sleep 10
free -m | sed -n 2p
setsid nohup $M serve > ~/.magnitude/logs/serve-retry.log 2>&1 < /dev/null &
sleep 20
$M models load local:qwen3.6-35b-a3b-q4_k_m:4fd8c90d3aa4
for i in $(seq 1 120); do $M status 2>&1 | grep -q Loading || break; sleep 5; done
free -m | sed -n 2p
target/release/braze-bench crates/braze-bench/suites/default.toml \
  --backends "zen:local:qwen3.6-35b-a3b-q4_k_m:4fd8c90d3aa4" --repetitions 3 --seed 42 --temperature 0.2 \
  --task-timeout-secs 900 \
  --output ~/magnitude-runs/sweep-qwen36-35b-a3b-magnitude-retry-default-2026-11-25.json \
  >> ~/magnitude-runs/qwen36-magnitude-retry-default.log 2>&1
echo "exit $? $(date -Is)" >> ~/magnitude-runs/qwen36-magnitude-retry-default.log
