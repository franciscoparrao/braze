#!/usr/bin/env bash
# LFM3 1.7B (QAD-Q4_0) servido por Magnitude 0.3.3 en 127.0.0.1:10100, braze por el
# backend zen. Misma configuración que el lote discriminante de octubre.
source ~/.cargo/env
cd ~/braze-study2
export LD_LIBRARY_PATH=$(ls -d target/release/build/llama-cpp-sys-2-*/out/build/bin)
export BRAZE_ZEN_BASE_URL=http://127.0.0.1:10100/inference/v1 BRAZE_ZEN_API_KEY=local
for suite in default discriminating; do
  target/release/braze-bench crates/braze-bench/suites/$suite.toml \
    --backends zen:lfm3-1.7b:gguf:q4-qat --repetitions 3 --seed 42 --temperature 0.2 \
    --task-timeout-secs 900 \
    --output ~/magnitude-runs/sweep-lfm3-1.7b-magnitude-$suite-2026-11-25.json \
    >> ~/magnitude-runs/lfm3-$suite.log 2>&1
  echo "exit $? $(date -Is)" >> ~/magnitude-runs/lfm3-$suite.log
done
