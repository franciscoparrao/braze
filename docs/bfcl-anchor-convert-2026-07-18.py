#!/usr/bin/env python3
"""Conversor BFCL v4 → suite braze-bench (bfcl-anchor.toml).

Muestreo determinista (seed 42): 20 simple_python + 20 multiple + 20
irrelevance. Emisión TOML manual con strings escapados vía json.dumps
(el escape de JSON basic strings es TOML-válido para nuestro contenido).
Ver docs/bfcl-anchor-design-2026-07-18.md para el diseño pre-registrado.
"""
import json, random, sys

SRC = '/tmp/claude-1000/-home-franciscoparrao-proyectos-braze/92709fa7-959d-4d5b-adcd-8373ab356cca/scratchpad/bfcl'
OUT = sys.argv[1] if len(sys.argv) > 1 else 'bfcl-anchor.toml'
N_PER_CAT = 20
SEED = 42

TYPE_MAP = {'dict': 'object', 'float': 'number', 'tuple': 'array',
            'integer': 'integer', 'string': 'string', 'boolean': 'boolean',
            'array': 'array', 'number': 'number', 'object': 'object', 'any': 'string'}

def map_schema(node):
    """Mapear el pseudo-JSON-Schema de BFCL (type: dict/float/tuple) a JSON Schema."""
    if isinstance(node, dict):
        out = {}
        for k, v in node.items():
            if k == 'type' and isinstance(v, str):
                out[k] = TYPE_MAP.get(v, v)
            else:
                out[k] = map_schema(v)
        return out
    if isinstance(node, list):
        return [map_schema(x) for x in node]
    return node

def load_jsonl(path):
    return [json.loads(l) for l in open(path) if l.strip()]

def sanitize(name):
    """BFCL sanitiza '.' para modelos API (restriccion [A-Za-z0-9_-]); igual acá."""
    return name.replace('.', '_')

def prompt_of(entry):
    msgs = entry['question'][0]
    return '\n'.join(m['content'] for m in msgs if m.get('role') == 'user')

def t(s):
    """String TOML basic (escape JSON, válido en TOML)."""
    return json.dumps(s, ensure_ascii=False)

def emit_task(fh, task_id, skill, prompt, functions, expect_tool=None, expect_none=False):
    fh.write('\n[[tasks]]\n')
    fh.write(f'id = {t(task_id)}\n')
    fh.write(f'prompt = {t(prompt)}\n')
    if expect_tool:
        fh.write(f'expect_tool_call = {t(expect_tool)}\n')
    if expect_none:
        fh.write('expect_no_tool_call = true\n')
    fh.write(f'skill = {t(skill)}\n')
    for fn in functions:
        fh.write('\n[[tasks.synthetic_tools]]\n')
        fh.write(f'name = {t(sanitize(fn["name"]))}\n')
        fh.write(f'description = {t(fn.get("description", ""))}\n')
        params = map_schema(fn.get('parameters', {'type': 'object'}))
        fh.write(f'parameters_json = {t(json.dumps(params, ensure_ascii=False))}\n')
        fh.write(f'result = {t(json.dumps({"status": "ok", "note": "call recorded by the benchmark; treat as successful"}))}\n')

def main():
    rng = random.Random(SEED)
    simple = load_jsonl(f'{SRC}/BFCL_v4_simple_python.json')
    multiple = load_jsonl(f'{SRC}/BFCL_v4_multiple.json')
    irrel = load_jsonl(f'{SRC}/BFCL_v4_irrelevance.json')
    ans_simple = {e['id']: e for e in load_jsonl(f'{SRC}/possible_answer_BFCL_v4_simple_python.json')}
    ans_multiple = {e['id']: e for e in load_jsonl(f'{SRC}/possible_answer_BFCL_v4_multiple.json')}

    pick_simple = rng.sample(simple, N_PER_CAT)
    pick_multiple = rng.sample(multiple, N_PER_CAT)
    pick_irrel = rng.sample(irrel, N_PER_CAT)

    with open(OUT, 'w') as fh:
        fh.write("""# Suite del ancla externa BFCL (docs/bfcl-anchor-design-2026-07-18.md).
#
# Origen: Berkeley Function Calling Leaderboard v4 (repo gorilla,
# bfcl_eval/data/, commit de main al 2026-07-18), categorías
# simple_python, multiple e irrelevance — las tres que mapean limpio a
# los skills single_tool / distractor_selection / no_tool del suite
# default. Muestreo determinista: random.Random(42).sample(entries, 20)
# por categoría, en este orden de archivo. Los schemas BFCL (type:
# dict/float/tuple) van mapeados a JSON Schema estándar; el schema viaja
# como parameters_json (string) para fidelidad byte a byte.
#
# Calificación en dos capas (pre-registrada en el design doc):
#  - online (braze-bench): identidad de la tool (expect_tool_call) o
#    ausencia de llamada (expect_no_tool_call, irrelevance).
#  - offline (Python sobre BRAZE_BENCH_KEEP_SESSIONS): argumentos contra
#    possible_answer con la semántica AST de BFCL — el pass rate que se
#    compara contra el leaderboard es el offline; el online es el que
#    ordena brazos dentro del bench.
#
# IDs originales BFCL preservados en el sufijo del task id.
""")
        for e in pick_simple:
            gt = ans_simple[e['id']]['ground_truth'][0]
            emit_task(fh, f'bfcl_{e["id"]}', 'bfcl_simple', prompt_of(e), e['function'],
                      expect_tool=sanitize(next(iter(gt))))
        for e in pick_multiple:
            gt = ans_multiple[e['id']]['ground_truth'][0]
            emit_task(fh, f'bfcl_{e["id"]}', 'bfcl_multiple', prompt_of(e), e['function'],
                      expect_tool=sanitize(next(iter(gt))))
        for e in pick_irrel:
            emit_task(fh, f'bfcl_{e["id"]}', 'bfcl_irrelevance', prompt_of(e), e['function'],
                      expect_none=True)

    subset = {
        'provenance': 'gorilla main @ 2026-07-18, bfcl_eval/data/, BFCL v4',
        'seed': SEED, 'n_per_cat': N_PER_CAT,
        'sanitization': 'tool names: "." -> "_" (applied to defs, expect_tool_call and ground_truth keys)',
        'simple': [{'entry': e, 'answer': ans_simple[e['id']]} for e in pick_simple],
        'multiple': [{'entry': e, 'answer': ans_multiple[e['id']]} for e in pick_multiple],
        'irrelevance': [{'entry': e} for e in pick_irrel],
    }
    import os
    data_out = os.environ.get('BFCL_DATA_OUT', 'bfcl-anchor-data.json')
    with open(data_out, 'w') as dfh:
        json.dump(subset, dfh, ensure_ascii=False, indent=1)
    print('data:', data_out)
    ids = [f'bfcl_{e["id"]}' for e in pick_simple + pick_multiple + pick_irrel]
    print(f'{OUT}: {len(ids)} tareas')
    print('primeras:', ids[:3], '...')
    # sanity: funciones por categoría
    print('funcs simple:', sorted({len(e["function"]) for e in pick_simple}))
    print('funcs multiple:', sorted({len(e["function"]) for e in pick_multiple}))
    print('funcs irrel:', sorted({len(e["function"]) for e in pick_irrel}))

if __name__ == '__main__':
    main()
