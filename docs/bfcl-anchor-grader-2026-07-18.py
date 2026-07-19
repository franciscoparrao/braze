#!/usr/bin/env python3
"""Grader offline AST del ancla BFCL (capa 2 de la calificación,
docs/bfcl-anchor-design-2026-07-18.md).

Comprometido ANTES del sweep que califica: la semántica queda fijada por
adelantado. Opera sobre:
  - el JSON del sweep (--sweep docs/sweep-bfcl-anchor-2026-07-18.json)
  - las sesiones preservadas (braze-bench-preserved-sessions/)
  - el subset con ground truths (docs/bfcl-anchor-data-2026-07-18.json)

Semántica (simplificación documentada del AST-check de BFCL):
  - se toma la PRIMERA assistant_tool_call de la sesión;
  - nombre == función del ground truth (sanitizada . -> _);
  - por cada parámetro del ground truth: el valor llamado pertenece a su
    lista de admitidos ("" en la lista => omitible; strings: igualdad
    exacta tras strip; números: igualdad numérica int/float; listas:
    igualdad elemento a elemento con la misma normalización);
  - sin parámetros inventados: cada arg llamado existe en las properties
    del schema de esa función;
  - irrelevance: pass offline = ninguna assistant_tool_call (== online).
"""
import argparse, json, os, sys
from pathlib import Path


def sanitize(name):
    return name.replace('.', '_')


def norm(v):
    if isinstance(v, bool):
        return v
    if isinstance(v, (int, float)):
        return float(v)
    if isinstance(v, str):
        return v.strip()
    if isinstance(v, list):
        return [norm(x) for x in v]
    return v


def value_allowed(called, allowed):
    return any(norm(called) == norm(a) for a in allowed if a != "")


def load_subset(path):
    d = json.load(open(path))
    tasks = {}
    for cat in ('simple', 'multiple'):
        for item in d[cat]:
            e, a = item['entry'], item['answer']
            gt = a['ground_truth'][0]
            fname = sanitize(next(iter(gt)))
            params_allowed = next(iter(gt.values()))
            schema_props = {}
            for fn in e['function']:
                if sanitize(fn['name']) == fname:
                    schema_props = set(fn.get('parameters', {}).get('properties', {}).keys())
            tasks[f'bfcl_{e["id"]}'] = {
                'category': cat, 'fname': fname,
                'params_allowed': params_allowed, 'schema_props': schema_props,
            }
    for item in d['irrelevance']:
        tasks[f'bfcl_{item["entry"]["id"]}'] = {'category': 'irrelevance'}
    return tasks


def first_tool_call(session_dir):
    files = sorted(Path(session_dir).glob('*.jsonl'))
    for f in files:
        for line in open(f):
            try:
                e = json.loads(line)
            except json.JSONDecodeError:
                continue
            if e.get('type') == 'assistant_tool_call':
                return e
    return None


def grade_call(call, spec):
    """(veredicto, motivo). Veredicto True solo si nombre+args pasan."""
    if call is None:
        return False, 'no_tool_call'
    if call['name'] != spec['fname']:
        return False, f'wrong_function:{call["name"]}'
    args = call.get('arguments') or {}
    if not isinstance(args, dict):
        return False, 'arguments_not_object'
    for k in args:
        if spec['schema_props'] and k not in spec['schema_props']:
            return False, f'invented_param:{k}'
    for param, allowed in spec['params_allowed'].items():
        if param in args:
            if not value_allowed(args[param], allowed):
                return False, f'bad_value:{param}={json.dumps(args[param], ensure_ascii=False)[:60]}'
        else:
            if "" not in allowed:
                return False, f'missing_param:{param}'
    return True, 'ok'


def path_component(raw):
    return ''.join(c if (c.isalnum() or c in '.-_') else '_' for c in raw)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--sweep', required=True)
    ap.add_argument('--data', default='docs/bfcl-anchor-data-2026-07-18.json')
    ap.add_argument('--sessions', default='braze-bench-preserved-sessions')
    ap.add_argument('--out', default=None)
    args = ap.parse_args()

    tasks = load_subset(args.data)
    sweep = json.load(open(args.sweep))
    rows = []
    for r in sweep['results']:
        tid = r['task_id']
        spec = tasks.get(tid)
        if spec is None:
            continue
        sess = Path(args.sessions) / path_component(r['backend']) / path_component(tid) / f'rep{r["repetition"]}' / 'session'
        call = first_tool_call(sess) if sess.is_dir() else None
        if spec['category'] == 'irrelevance':
            if not sess.is_dir():
                offline_pass, reason = False, 'session_missing'
            else:
                offline_pass, reason = (call is None), ('ok' if call is None else f'called:{call["name"]}')
        else:
            if not sess.is_dir():
                offline_pass, reason = False, 'session_missing'
            else:
                offline_pass, reason = grade_call(call, spec)
        rows.append({
            'backend': r['backend'], 'task_id': tid, 'repetition': r['repetition'],
            'category': spec['category'], 'online_pass': r['passed'],
            'offline_pass': offline_pass, 'reason': reason,
        })

    # resumen por brazo × categoría
    agg = {}
    for row in rows:
        key = (row['backend'], row['category'])
        a = agg.setdefault(key, {'n': 0, 'online': 0, 'offline': 0})
        a['n'] += 1
        a['online'] += row['online_pass']
        a['offline'] += row['offline_pass']
    print(f"{'backend':58} {'cat':16} {'n':>4} {'online':>8} {'offline':>8}")
    for (b, c), a in sorted(agg.items()):
        print(f"{b:58} {c:16} {a['n']:>4} {a['online']:>7}{'':1} {a['offline']:>7}")
    tot = {}
    for row in rows:
        a = tot.setdefault(row['backend'], {'n': 0, 'online': 0, 'offline': 0})
        a['n'] += 1; a['online'] += row['online_pass']; a['offline'] += row['offline_pass']
    print('\nagregado:')
    for b, a in sorted(tot.items()):
        print(f"{b:58} n={a['n']} online={a['online']} ({100*a['online']/a['n']:.1f}%) offline={a['offline']} ({100*a['offline']/a['n']:.1f}%)")

    out = args.out or args.sweep.replace('.json', '.offline-grades.json')
    json.dump(rows, open(out, 'w'), ensure_ascii=False, indent=1)
    print('\nfilas:', len(rows), '→', out)


if __name__ == '__main__':
    main()
