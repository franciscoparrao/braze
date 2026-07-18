#!/usr/bin/env python3
"""Análisis del ancla BFCL — lecturas E1-E4 pre-declaradas en
docs/bfcl-anchor-design-2026-07-18.md.

Estadística idéntica a la del paper: Wilson 95% para niveles,
Newcombe/MOVER para deltas within-sweep (los 5 brazos vienen de UN
sweep multi-brazo), bootstrap por tarea (60 clusters, B=20.000, seed 42)
para los deltas headline. Sin corrección de continuidad, uniforme con
el resto del manuscrito (§setup).

Uso:
  python3 docs/bfcl-anchor-analysis-2026-07-18.py \
      --sweep docs/sweep-bfcl-anchor-2026-07-18.json \
      --grades docs/sweep-bfcl-anchor-2026-07-18.offline-grades.json
"""
import argparse, json, math, random
from collections import defaultdict

BASE_1B = 'ollama:llama3.2:1b'
COMPOSITE = 'ollama:llama3.2:1b+lead:ollama:gemma4:e4b'
SOLO = 'ollama:gemma4:e4b'
BASE_3B = 'ollama:qwen2.5:3b'
BASE_CEILING = 'ollama:qwen3.5-coder'

# Referencia del suite autoral (docs/sweep-curva-multiescala, n=95/celda)
DEFAULT_SUITE = {BASE_1B: 18.9, BASE_3B: 68.4, BASE_CEILING: 97.9,
                 COMPOSITE: 89.5, SOLO: 91.6}


def wilson(k, n, z=1.96):
    if n == 0:
        return 0.0, 0.0
    p = k / n
    den = 1 + z * z / n
    c = (p + z * z / (2 * n)) / den
    h = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / den
    return 100 * (c - h), 100 * (c + h)


def newcombe(k1, n1, k2, n2):
    p1, p2 = k1 / n1, k2 / n2
    l1, u1 = [x / 100 for x in wilson(k1, n1)]
    l2, u2 = [x / 100 for x in wilson(k2, n2)]
    d = p1 - p2
    return (100 * d,
            100 * (d - math.sqrt((p1 - l1) ** 2 + (u2 - p2) ** 2)),
            100 * (d + math.sqrt((u1 - p1) ** 2 + (p2 - l2) ** 2)))


def cluster_boot_delta(rows_a, rows_b, key, B=20000, seed=42):
    """Bootstrap por tarea del delta a-b; resampleo conjunto de tareas."""
    ba, bb = defaultdict(list), defaultdict(list)
    for r in rows_a:
        ba[r['task_id']].append(1 if r[key] else 0)
    for r in rows_b:
        bb[r['task_id']].append(1 if r[key] else 0)
    tasks = sorted(set(ba) & set(bb))
    rng = random.Random(seed)
    reps = []
    for _ in range(B):
        sample = [rng.choice(tasks) for _ in tasks]
        k1 = sum(sum(ba[t]) for t in sample); n1 = sum(len(ba[t]) for t in sample)
        k2 = sum(sum(bb[t]) for t in sample); n2 = sum(len(bb[t]) for t in sample)
        reps.append(k1 / n1 - k2 / n2)
    reps.sort()
    return 100 * reps[int(0.025 * B)], 100 * reps[int(0.975 * B)]


def transport(r):
    return (r.get('failure_cause') == 'model_backend_error'
            and (r.get('wall_time_ms', 0) < 1000
                 or 'request to model backend failed' in str(r.get('run_error'))
                 or 'stream failed' in str(r.get('run_error'))))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--sweep', default='docs/sweep-bfcl-anchor-2026-07-18.json')
    ap.add_argument('--grades', default='docs/sweep-bfcl-anchor-2026-07-18.offline-grades.json')
    args = ap.parse_args()

    sweep = json.load(open(args.sweep))
    results = sweep['results']
    grades = {(g['backend'], g['task_id'], g['repetition']): g
              for g in json.load(open(args.grades))}
    for r in results:
        g = grades.get((r['backend'], r['task_id'], r['repetition']))
        r['offline_pass'] = bool(g and g['offline_pass'])
        r['category'] = g['category'] if g else '?'

    print(f"commit del binario: {sweep['metadata']['braze_git_commit'][:7]}  "
          f"runs: {len(results)}")

    # --- Sanidad de transporte (lección del 2026-07-18) ------------------
    print("\n== Sanidad de transporte ==")
    dirty = False
    by_arm = defaultdict(list)
    for r in results:
        by_arm[r['backend']].append(r)
    for a, rows in sorted(by_arm.items()):
        t = [r for r in rows if transport(r)]
        if t:
            dirty = True
        print(f"  {a[:52]:52} transport={len(t)}/{len(rows)}")
    if dirty:
        print("  ⚠ hay runs de transporte: el análisis de abajo los INCLUYE;")
        print("    si son >2% de un brazo, re-correr antes de citar en el paper.")

    # --- Niveles por brazo ----------------------------------------------
    print("\n== Niveles (online = identidad de tool; offline = AST de argumentos) ==")
    print(f"  {'brazo':52} {'online':>22} {'offline':>22}  {'suite autoral':>13}")
    stats = {}
    for a, rows in sorted(by_arm.items()):
        n = len(rows)
        ko = sum(r['passed'] for r in rows)
        kf = sum(r['offline_pass'] for r in rows)
        lo_o, hi_o = wilson(ko, n)
        lo_f, hi_f = wilson(kf, n)
        stats[a] = dict(n=n, online=ko, offline=kf, rows=rows)
        ref = DEFAULT_SUITE.get(a)
        print(f"  {a[:52]:52} {100*ko/n:5.1f}% [{lo_o:4.1f},{hi_o:4.1f}] "
              f"{100*kf/n:5.1f}% [{lo_f:4.1f},{hi_f:4.1f}]  "
              f"{(f'{ref:.1f}%' if ref else '—'):>13}")

    # --- Por categoría ---------------------------------------------------
    print("\n== Por categoría (offline) ==")
    for a, rows in sorted(by_arm.items()):
        per = defaultdict(lambda: [0, 0])
        for r in rows:
            per[r['category']][1] += 1
            per[r['category']][0] += r['offline_pass']
        cats = "  ".join(f"{c}={v[0]}/{v[1]}" for c, v in sorted(per.items()))
        print(f"  {a[:52]:52} {cats}")

    def delta(label, a, b, key):
        if a not in stats or b not in stats:
            print(f"  {label}: brazo ausente"); return
        sa, sb = stats[a], stats[b]
        ka = sa['online'] if key == 'passed' else sa['offline']
        kb = sb['online'] if key == 'passed' else sb['offline']
        d, lo, hi = newcombe(ka, sa['n'], kb, sb['n'])
        cl, ch = cluster_boot_delta(sa['rows'], sb['rows'], key)
        cruza = "cruza cero" if lo <= 0 <= hi else "FUERA de cero"
        print(f"  {label}: {d:+.1f}pp  Newcombe [{lo:+.1f},{hi:+.1f}] ({cruza})"
              f"  cluster-boot [{cl:+.1f},{ch:+.1f}]")

    print("\n== E1 — Ordenamiento de baselines (¿la curva del paper transfiere?) ==")
    for key in ('passed', 'offline_pass'):
        etiqueta = 'online ' if key == 'passed' else 'offline'
        orden = sorted([BASE_1B, BASE_3B, BASE_CEILING],
                       key=lambda a: -(stats[a]['online'] if key == 'passed' else stats[a]['offline'])
                       if a in stats else 0)
        print(f"  {etiqueta}: " + " > ".join(a.split(':')[-1] for a in orden)
              + "   (suite autoral: coder > 3b > 1b)")

    print("\n== E2 — La palanca lead a 1B (suite autoral: +70.5pp) ==")
    delta("  online ", COMPOSITE, BASE_1B, 'passed')
    delta("  offline", COMPOSITE, BASE_1B, 'offline_pass')

    print("\n== E3 — Pinned ceiling: composite vs lead solo (esperado: nulo) ==")
    delta("  online ", COMPOSITE, SOLO, 'passed')
    delta("  offline", COMPOSITE, SOLO, 'offline_pass')

    print("\n== E4 — Gap identidad → argumentos (online − offline, por brazo) ==")
    for a, s in sorted(stats.items()):
        gap = 100 * (s['online'] - s['offline']) / s['n']
        print(f"  {a[:52]:52} {gap:+.1f}pp")


if __name__ == '__main__':
    main()
