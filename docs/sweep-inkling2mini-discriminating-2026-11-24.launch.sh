#!/usr/bin/env bash
# Inkling 2 Mini (UD-Q3_K_XL, Mainline_Support) vía llama-server en 127.0.0.1:8091,
# braze por el backend zen. Configuración del sweep de Bonsai 8B salvo el timeout.
source ~/.cargo/env
cd ~/braze-study2
export LD_LIBRARY_PATH=$(ls -d target/release/build/llama-cpp-sys-2-*/out/build/bin)
export BRAZE_ZEN_BASE_URL=http://127.0.0.1:8091/v1 BRAZE_ZEN_API_KEY=local
target/release/braze-bench crates/braze-bench/suites/discriminating.toml \
  --backends zen:inkling-2-mini --repetitions 3 --seed 42 --temperature 0.2 \
  --task-timeout-secs 900 \
  --output ~/inkling-runs/sweep-inkling2mini-discriminating-2026-11-24.json "$@" \
  >> ~/inkling-runs/discriminating.log 2>&1
echo "exit $? $(date -Is)" >> ~/inkling-runs/discriminating.log
