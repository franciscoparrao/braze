#!/usr/bin/env python3
"""Genera los casos dorados de chat template para los fixture tests de
braze-model (`crates/braze-model/src/chat_template_fixtures.rs`) — el
patrón de ferrumox/rabbit (`tests/qwen38_chat_template_fixture.rs`):
renderizar el chat_template.jinja REAL de cada familia con Jinja2 (el
mismo ImmutableSandboxedEnvironment + trim_blocks/lstrip_blocks que usa
transformers) y fijar el resultado string-por-string contra el render
propio del LocalBackend.

Por qué un oráculo y no solo unit tests (razón de rabbit, aplica
verbatim): un chat template difiere de una traducción a mano plausible
por saltos de línea sueltos y orden de bloques — y el modo de falla no
es un error, es un modelo que razona en el lugar equivocado o nunca
emite stop token.

Dependencia dev-time-only: jinja2 (`pip install jinja2`). Nunca una
dependencia de runtime ni de build de braze.

Fuentes de referencia (documentadas, no embebidas):
  chatml : https://huggingface.co/Qwen/Qwen2.5-3B-Instruct
           (campo chat_template de tokenizer_config.json)
  harmony: https://huggingface.co/openai/gpt-oss-20b/raw/main/chat_template.jinja
  gemma  : el template EMBEBIDO en el GGUF que braze realmente corre
           (metadata `tokenizer.chat_template` de
           nitro:~/models/gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf) — NO el
           del hub, que puede diferir del artefacto. Hallazgo 2026-08-15:
           ese template es el dialecto NUEVO de gemma-4
           (`<|turn>role ... <turn|>`), no el `<start_of_turn>` de
           gemma2/3 que braze renderiza — ver el fixture test de gemma.

Uso:
  python3 tools/gen_chat_cases.py --family chatml  --jinja <qwen.jinja>
  python3 tools/gen_chat_cases.py --family harmony --jinja <gptoss.jinja>
  python3 tools/gen_chat_cases.py --family gemma   --jinja <gemma_gguf.jinja>

Escribe crates/braze-model/tests/fixtures/chat/<family>_cases.json.
La fecha es fija (--date, default 2026-08-15) para que el fixture sea
determinista: `strftime_now` del template harmony la devuelve tal cual.
"""

import argparse
import json
import sys
from pathlib import Path

try:
    from jinja2.sandbox import ImmutableSandboxedEnvironment
except ImportError:
    sys.exit("gen_chat_cases: falta jinja2 (pip install jinja2) — dependencia dev-time-only")

REPO_ROOT = Path(__file__).resolve().parent.parent
FIXTURES_DIR = REPO_ROOT / "crates/braze-model/tests/fixtures/chat"

# Casos compartidos: conversaciones PLANAS (system + user/assistant) más,
# donde la referencia no es ambigua, una ronda de tools. Deliberadamente
# SIN campo `tools` en el render de referencia: el preámbulo de tools de
# braze es una desviación deliberada medida por sweep (Fase 1,
# schema_fail 17→0), no un accidente que un fixture deba "corregir".
#
# El caso de tool-call usa arguments={} a propósito: jinja
# (`|tojson` ≈ json.dumps, con espacios) y serde_json (compacto) emiten
# dialectos de espaciado distintos para objetos no vacíos — el fixture
# fija el FRAMING (marcadores, saltos de línea, orden), no el dialecto
# JSON, que se mide aparte.
SYSTEM = "You are braze, a coding agent. Be concise."

def chatml_cases():
    return [
        {
            "name": "minimal",
            "messages": [
                {"role": "system", "content": SYSTEM},
                {"role": "user", "content": "hola, ¿qué es un GGUF?"},
            ],
        },
        {
            "name": "multiturn",
            "messages": [
                {"role": "system", "content": SYSTEM},
                {"role": "user", "content": "lista los archivos"},
                {"role": "assistant", "content": "Hay tres archivos: a.rs, b.rs y c.rs."},
                {"role": "user", "content": "¿cuál es más grande?"},
            ],
        },
        {
            "name": "tool_round",
            "messages": [
                {"role": "system", "content": SYSTEM},
                {"role": "user", "content": "¿qué hora es en el server?"},
                {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [
                        {"function": {"name": "shell_exec", "arguments": {}}}
                    ],
                },
                {"role": "tool", "content": "14:05"},
                {"role": "assistant", "content": "Son las 14:05 en el server."},
                {"role": "user", "content": "gracias, ¿y la fecha?"},
            ],
        },
    ]

def harmony_cases():
    # Solo conversaciones planas: para las rondas de tools las DOS
    # referencias públicas discrepan entre sí (el chat_template.jinja de
    # HF pone `to=functions.X` tras `assistant`; la librería
    # openai-harmony canónica lo pone tras `<|channel|>commentary`, que
    # es lo que braze implementa y lo que gpt-oss emite) — no hay
    # oráculo único que fijar. Documentado en el fixture test.
    return [
        {
            "name": "minimal",
            "messages": [
                {"role": "system", "content": SYSTEM},
                {"role": "user", "content": "hola, ¿qué es un GGUF?"},
            ],
        },
        {
            "name": "multiturn",
            "messages": [
                {"role": "system", "content": SYSTEM},
                {"role": "user", "content": "lista los archivos"},
                {"role": "assistant", "content": "Hay tres archivos: a.rs, b.rs y c.rs."},
                {"role": "user", "content": "¿cuál es más grande?"},
            ],
        },
    ]

def gemma_cases():
    # La referencia gemma-4 es un dialecto entero distinto al que braze
    # renderiza (ver docstring) — estos casos documentan ESE hecho, no
    # una igualdad esperada: el fixture test de gemma afirma la
    # divergencia de dialecto, no un match.
    return [
        {
            "name": "minimal",
            "messages": [
                {"role": "system", "content": SYSTEM},
                {"role": "user", "content": "hola, ¿qué es un GGUF?"},
            ],
        },
        {
            "name": "multiturn",
            "messages": [
                {"role": "system", "content": SYSTEM},
                {"role": "user", "content": "lista los archivos"},
                {"role": "assistant", "content": "Hay tres archivos: a.rs, b.rs y c.rs."},
                {"role": "user", "content": "¿cuál es más grande?"},
            ],
        },
    ]

FAMILIES = {
    "chatml": chatml_cases,
    "harmony": harmony_cases,
    "gemma": gemma_cases,
}


def render(env, template_src, messages, date, family):
    def strftime_now(fmt):
        # Fecha fija -> fixture determinista. El formato que piden los
        # templates reales es %Y-%m-%d; cualquier otro sería un caso
        # nuevo a decidir, no a adivinar.
        assert fmt == "%Y-%m-%d", f"strftime_now con formato inesperado: {fmt}"
        return date

    def raise_exception(msg):
        raise RuntimeError(f"template raise_exception: {msg}")

    template = env.from_string(
        template_src,
        globals={"strftime_now": strftime_now, "raise_exception": raise_exception},
    )
    kwargs = {"messages": messages, "add_generation_prompt": True}
    if family == "gemma":
        # bos_token lo agrega el tokenizer en braze (AddBos::Always); el
        # template lo interpola, así que se lo damos vacío para comparar
        # solo el texto del template.
        kwargs["bos_token"] = ""
    return template.render(**kwargs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--family", required=True, choices=sorted(FAMILIES))
    parser.add_argument("--jinja", required=True, type=Path, help="chat_template.jinja de referencia")
    parser.add_argument("--date", default="2026-08-15", help="fecha fija para strftime_now (YYYY-MM-DD)")
    args = parser.parse_args()

    template_src = args.jinja.read_text()
    # El mismo entorno que transformers usa para apply_chat_template.
    env = ImmutableSandboxedEnvironment(trim_blocks=True, lstrip_blocks=True)

    cases = []
    for case in FAMILIES[args.family]():
        expected = render(env, template_src, case["messages"], args.date, args.family)
        cases.append({**case, "expected": expected})

    FIXTURES_DIR.mkdir(parents=True, exist_ok=True)
    out_path = FIXTURES_DIR / f"{args.family}_cases.json"
    out_path.write_text(
        json.dumps({"family": args.family, "date": args.date, "cases": cases},
                   ensure_ascii=False, indent=1) + "\n"
    )
    print(f"{out_path}: {len(cases)} casos ({sum(len(c['expected']) for c in cases)} chars de referencia)")


if __name__ == "__main__":
    main()
