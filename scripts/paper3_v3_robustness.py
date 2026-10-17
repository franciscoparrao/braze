#!/usr/bin/env python3
"""Paper 3 v3, robustez del resultado "δ fijo de H0 vs δ re-derivado por configuración".

Sobre los mismos datos que `paper3_per_config_threshold.py`, varía las tres
elecciones que el outline v3 declara como threats y reporta la FAR de ambas
reglas con IC bootstrap por sweep (cluster = archivo de sweep):

  1. Estimador de δ (sobre los |Δ| de réplicas de la misma configuración):
       max     — "spread of repeated evaluations" (el del resultado principal)
       mean    — media de |Δ|
       2sd     — 2 × desviación estándar de Δ
  2. Elección de H0 dentro del sweep:
       first   — primer brazo (resultado principal)
       random  — brazo al azar, promediado sobre 200 sorteos
       quiet   — el brazo menos ruidoso (escenario favorable al δ fijo)
       noisy   — el brazo más ruidoso (escenario aún más favorable)
  3. Igualación de pares: el δ de H0 se estima con el mismo número de pares no
     ordenados que tiene el leave-one-pair-out del incumbente (para que la regla
     por-config no gane solo por usar menos pares).

Uso: python3 scripts/paper3_v3_robustness.py [--out docs/paper3-v3-robustness.json]
"""
from __future__ import annotations

import argparse
import json
import random
import statistics
from collections import defaultdict
from itertools import combinations

from paper3_false_acceptance import REPO, collect_replicate_groups
from paper3_per_config_threshold import rep_pass_counts

SEED = 20261017
B = 2000  # remuestras bootstrap por cluster (sweep)


def unordered_deltas(tasks, reps, common):
    """Δ (B−A)/n por par NO ordenado (a<b); el inverso es −Δ."""
    n = len(common)
    passed = {r: sum(tasks[t][r] for t in common) for r in reps}
    return {(a, b): (passed[b] - passed[a]) / n for a, b in combinations(reps, 2)}


def estimate(abs_or_signed, how):
    """δ a partir de una lista de Δ con signo (se usan ambos órdenes)."""
    vals = [v for d in abs_or_signed for v in (d, -d)]
    if not vals:
        return 0.0
    if how == "max":
        return max(abs(v) for v in vals)
    if how == "mean":
        return statistics.mean(abs(v) for v in vals)
    if how == "2sd":
        return 2 * statistics.pstdev(vals)
    raise ValueError(how)


def noise(arm):
    return statistics.mean(abs(v) for v in arm["d"].values()) if arm["d"] else 0.0


def evaluate(sweeps, how, h0_pick, rng, match_pairs):
    """Devuelve {sweep: (pares, acc_fijo, acc_porconfig)} para una configuración del análisis."""
    out = {}
    for fname, arms in sweeps.items():
        if h0_pick == "first":
            h0 = arms[0]
        elif h0_pick == "random":
            h0 = rng.choice(arms)
        elif h0_pick == "quiet":
            h0 = min(arms, key=noise)
        elif h0_pick == "noisy":
            h0 = max(arms, key=noise)
        h0_list = list(h0["d"].values())
        trials = acc_f = acc_c = 0
        for arm in arms:
            if arm is h0:
                continue
            for pair, v in arm["d"].items():
                rest = [w for p, w in arm["d"].items() if p != pair]
                if not rest:
                    continue
                d_cfg = estimate(rest, how)
                base = rng.sample(h0_list, min(len(rest), len(h0_list))) if match_pairs else h0_list
                d_fix = estimate(base, how)
                for x in (v, -v):            # ambos órdenes, como en el experimento central
                    trials += 1
                    acc_f += x > d_fix
                    acc_c += x > d_cfg
        if trials:
            out[fname] = (trials, acc_f, acc_c)
    return out


def summarize(per_sweep, rng):
    keys = sorted(per_sweep)
    tot = [sum(per_sweep[k][i] for k in keys) for i in range(3)]
    far_f, far_c = tot[1] / tot[0], tot[2] / tot[0]
    boots = []
    for _ in range(B):
        s = [per_sweep[rng.choice(keys)] for _ in keys]
        t = sum(x[0] for x in s)
        boots.append((sum(x[1] for x in s) / t, sum(x[2] for x in s) / t))
    diff = sorted(f - c for f, c in boots)
    ci = lambda xs: (xs[int(0.025 * len(xs))], xs[int(0.975 * len(xs))])
    return {"pairs": tot[0], "sweeps": len(keys), "far_fixed_h0": far_f, "far_per_config": far_c,
            "diff": far_f - far_c, "diff_ci95": ci(diff),
            "far_fixed_ci95": ci(sorted(b[0] for b in boots)), "far_per_config_ci95": ci(sorted(b[1] for b in boots))}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="docs/paper3-v3-robustness.json")
    args = ap.parse_args()
    rng = random.Random(SEED)

    usable, _, _ = collect_replicate_groups()
    by_file = defaultdict(list)
    for (fname, backend), tasks in sorted(usable.items()):
        reps, common = rep_pass_counts(tasks)
        if len(reps) >= 3 and len(common) >= 8:
            by_file[fname].append({"backend": backend, "d": unordered_deltas(tasks, reps, common)})
    sweeps = {f: a for f, a in by_file.items() if len(a) >= 2}

    rows = []
    for how in ("max", "mean", "2sd"):
        for pick in ("first", "random", "quiet", "noisy"):
            for match in (False, True):
                if pick == "random":
                    # promedio sobre sorteos de H0: se acumulan conteos por sweep
                    acc = defaultdict(lambda: [0, 0, 0])
                    for _ in range(200):
                        for k, v in evaluate(sweeps, how, pick, rng, match).items():
                            for i in range(3):
                                acc[k][i] += v[i]
                    per = {k: tuple(v) for k, v in acc.items()}
                else:
                    per = evaluate(sweeps, how, pick, rng, match)
                s = summarize(per, rng)
                s.update({"estimator": how, "h0": pick, "match_pairs": match})
                rows.append(s)
                print(f"δ={how:<4} H0={pick:<6} igualar={'sí' if match else 'no'}  FAR fijo {s['far_fixed_h0']:6.1%}  "
                      f"por-config {s['far_per_config']:6.1%}  dif {s['diff']:+6.1%} "
                      f"IC95 [{s['diff_ci95'][0]:+.1%}, {s['diff_ci95'][1]:+.1%}]")
    (REPO / args.out).write_text(json.dumps({"seed": SEED, "bootstrap": B, "sweeps": len(sweeps), "rows": rows}, indent=1))
    print(f"reporte: {args.out}")


if __name__ == "__main__":
    main()
