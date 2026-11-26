#!/usr/bin/env bash
# Re-corrida intercalada del Estudio 2 — comando del pre-registro
# docs/hypothesis-2026-11-17-study2-interleaved.md (commits 15d74ca y 0fe2725).
cd ~/braze-study2
source ~/.cargo/env   # cargo en el PATH (enmienda 1 del pre-registro)
export LD_LIBRARY_PATH=$(ls -d target/release/build/llama-cpp-sys-2-*/out/build/bin)
export BRAZE_OLLAMA_NUM_CTX=32768 BRAZE_MAX_TOKENS=12288 \
       BRAZE_LOCAL_FAMILY=harmony BRAZE_LOCAL_KV_OFFLOAD=host \
       RUST_LOG=braze_model=info
G=$HOME/models/gpt-oss-20b-MXFP4.gguf
target/release/braze-bench crates/braze-bench/suites/discriminating.toml \
  --backends "local:$G,local:$G+ablate:project-memory,local:$G+ablate:project-memory-seeded" \
  --repetitions 3 --seed 42 --temperature 0.2 --task-timeout-secs 900 \
  --interleave 20261117 \
  --output ~/study2-interleaved/sweep-pm-ab-interleaved-2026-11-19.json "$@" \
  >> ~/study2-interleaved/sweep.log 2>&1
echo "exit $? $(date -Is)" >> ~/study2-interleaved/sweep.log
