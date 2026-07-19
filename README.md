# braze

An open-source agentic coding harness (CLI + TUI) in Rust, built as
**research infrastructure**: every reliability lever is independently
switchable per benchmark row, so a claim like "the lead model helps at
3B" is an ablation with a confidence interval and a commit hash behind
it, not a design note.

braze is the artifact behind the study *"Not All Scaffolding Helps: A
Lever-by-Lever Study of Agentic Harnesses at Small-Model Scales"*
(manuscript in `paper/`). It is an experiment, not a product.

## What's inside

A 15-crate workspace. Three frozen-contract traits organize everything:
`ToolProvider` (local tools and MCP servers behind one interface),
`ModelBackend` (Anthropic, Ollama, OpenRouter behind one streaming
interface), and `SessionStore`+`ContextCompactor` (append-only event
log; the compactor decides what a model sees on replay). Levers on top:
proactive lead model (`+lead:`), planning pre-round (`+plan:`),
two-level tool deferral, per-family textual tool-call rescue,
observation collapse, post-edit compile check, typed task list, harness
notes, best-of-n — each one a per-row `+ablate:` toggle in the
benchmark runner.

## Build and test

```bash
cargo build --workspace          # Rust edition 2024 (rustc >= 1.96)
cargo test  --workspace          # ~1,000 tests
cargo run -p braze-cli -- chat --tui   # interactive TUI (needs a backend, see below)
```

Backends are configured in `~/.config/braze/config.json` (or env vars
`BRAZE_*`). The cheapest way to try it: a local
[Ollama](https://ollama.com) server with a tool-calling-capable model
(`ollama pull qwen2.5:3b`), then
`braze chat --tui --backend ollama --model qwen2.5:3b`.

## Reproducing the paper's sweeps

Every sweep cited in the manuscript is reproducible from its raw JSON
in `docs/` (e.g. `docs/sweep-bfcl-anchor-2026-07-18.json`). Each JSON
embeds: the `braze` git commit that ran it, a suite fingerprint, the
per-model Ollama digests, sampling parameters, and (from July 2026 on)
the Ollama server version. The general shape of a reproduction:

```bash
cargo build -p braze-bench --release
BRAZE_OLLAMA_BASE_URL=http://<inference-node>:11434 \
BRAZE_OLLAMA_TRANSPORT_RETRIES=6 \
./target/release/braze-bench crates/braze-bench/suites/default.toml \
  --backends "ollama:llama3.2:1b,ollama:llama3.2:1b+lead:ollama:gemma4:e4b" \
  --repetitions 5 --output results.json
```

Notes that matter for faithful reproduction:

- **Serving stack**: all published sweeps ran against Ollama **0.30.7**
  on a dedicated LAN inference node (Acer Nitro, 16 GB RAM, CPU-only),
  one sweep at a time — concurrent load measurably contaminates pass
  rates (documented in the manuscript's threats section).
- **Model identity**: match the `ollama_model_digests` in the sweep's
  JSON, not just the tag — tags can be silently re-pulled to different
  weights.
- **Pre-registered criteria**: the adoption/rejection criteria that
  gate the paper's design decisions are committed documents under
  `docs/` predating their sweeps, transcribed verbatim in the
  manuscript's Appendix B.

## License

Dual-licensed under MIT or Apache-2.0, at your option
(`LICENSE-MIT`, `LICENSE-APACHE`).

## Citation

See `CITATION.cff`. If you use this software or the sweep data, please
cite the manuscript (preferred citation therein).
