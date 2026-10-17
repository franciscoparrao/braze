#!/usr/bin/env python3
"""Paper 3, experimento v3: ¿basta calibrar el umbral una vez sobre el harness base?

RRSI (arXiv 2609.24972) estima su margen de ruido δ con evaluaciones repetidas
del harness base H0 y lo deja fijo durante toda la evolución. La pregunta: si
el ruido cambia con la configuración, un δ medido en H0 acepta cambios nulos
en otras configuraciones a una tasa distinta de la que su calibración promete.

Datos: los mismos grupos de réplicas de configuración idéntica que
`paper3_false_acceptance.py` (mismas exclusiones). Dentro de cada sweep, el
primer brazo con ≥3 repeticiones hace de H0; los demás brazos de ese sweep
(misma suite, otra configuración) son los "incumbentes".

Para cada brazo, Δ = (aprobadas_B − aprobadas_A)/n sobre un par de réplicas;
cualquier Δ > 0 aceptado es un falso positivo (la configuración es la misma).

Reglas:
  - fijo-H0:      acepta si Δ > δ_H0, con δ_H0 = máx |Δ| entre réplicas de H0
                  (la "spread of repeated evaluations of the unchanged base").
  - por-config:   acepta si Δ > δ_g, con δ_g = máx |Δ| entre las OTRAS
                  réplicas del mismo brazo (leave-one-pair-out, sin circularidad).

Uso: python3 scripts/paper3_per_config_threshold.py [--out docs/paper3-per-config-threshold.json]
"""
from __future__ import annotations

import argparse
import json
import statistics
from collections import defaultdict
from itertools import combinations, permutations

from paper3_false_acceptance import REPO, collect_replicate_groups


def rep_pass_counts(tasks):
    """Por repetición: (n tareas presentes en todas, aprobadas) restringido a
    las tareas con todas las repeticiones (pareo completo)."""
    reps = sorted({r for t in tasks.values() for r in t})
    common = [t for t, byrep in tasks.items() if all(r in byrep for r in reps)]
    return reps, common


def deltas(tasks, reps, common):
    """Δ por par ORDENADO de réplicas, en fracción de la suite."""
    n = len(common)
    passed = {r: sum(tasks[t][r] for t in common) for r in reps}
    return {(a, b): (passed[b] - passed[a]) / n for a, b in permutations(reps, 2)}


def flip_rate(tasks, reps, common):
    """Fracción media de tareas que cambian de resultado entre dos réplicas."""
    rates = []
    for a, b in combinations(reps, 2):
        rates.append(sum(tasks[t][a] != tasks[t][b] for t in common) / len(common))
    return statistics.mean(rates)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="docs/paper3-per-config-threshold.json")
    args = ap.parse_args()

    usable, _, _ = collect_replicate_groups()
    by_file = defaultdict(list)
    for (fname, backend), tasks in sorted(usable.items()):
        reps, common = rep_pass_counts(tasks)
        if len(reps) >= 3 and len(common) >= 8:
            by_file[fname].append((backend, tasks, reps, common))

    arms_out, fixed_trials, fixed_acc, cfg_trials, cfg_acc = [], 0, 0, 0, 0
    under = 0          # brazos donde δ_H0 < ruido propio (δ_g)
    pooled = []        # Δ nulos de todos los brazos, para la curva umbral vs FAR
    for fname, arms in sorted(by_file.items()):
        h0 = arms[0]
        d_h0 = deltas(h0[1], h0[2], h0[3])
        delta_h0 = max(abs(v) for v in d_h0.values())
        for backend, tasks, reps, common in arms:
            d = deltas(tasks, reps, common)
            pooled.extend(d.values())
            own = max(abs(v) for v in d.values())
            rec = {"file": fname, "backend": backend[:70], "is_h0": backend == h0[0],
                   "tasks": len(common), "reps": len(reps),
                   "flip_rate": round(flip_rate(tasks, reps, common), 4),
                   "delta_own": round(own, 4), "delta_h0": round(delta_h0, 4)}
            if backend != h0[0]:
                acc_f = sum(v > delta_h0 for v in d.values())
                fixed_trials += len(d)
                fixed_acc += acc_f
                under += own > delta_h0
                # leave-one-pair-out: δ del brazo sin el par evaluado (ni su inverso)
                acc_c = 0
                for (a, b), v in d.items():
                    rest = [abs(w) for (x, y), w in d.items() if {x, y} != {a, b}]
                    acc_c += v > max(rest)
                cfg_trials += len(d)
                cfg_acc += acc_c
                rec.update({"null_pairs": len(d), "accepts_fixed_h0": acc_f, "accepts_per_config": acc_c})
            arms_out.append(rec)

    others = [a for a in arms_out if not a["is_h0"]]
    flips = [a["flip_rate"] for a in arms_out]
    curve = []
    for k in range(0, 31):
        thr = k / 100
        curve.append({"delta": thr, "far": round(sum(v > thr for v in pooled) / len(pooled), 4)})

    report = {
        "sweeps_with_h0_and_incumbents": sum(1 for f in by_file.values() if len(f) >= 2),
        "arms": len(arms_out),
        "incumbent_arms": len(others),
        "flip_rate": {"min": min(flips), "median": statistics.median(flips), "max": max(flips),
                      "p10": sorted(flips)[len(flips) // 10], "p90": sorted(flips)[9 * len(flips) // 10]},
        "incumbents_noisier_than_h0": under,
        "far_fixed_h0": fixed_acc / fixed_trials if fixed_trials else None,
        "far_per_config_lopo": cfg_acc / cfg_trials if cfg_trials else None,
        "null_pairs_incumbents": fixed_trials,
        "curve_pooled_delta_vs_far": curve,
        "per_arm": arms_out,
    }
    (REPO / args.out).write_text(json.dumps(report, indent=1))

    print(f"sweeps con H0 + incumbentes: {report['sweeps_with_h0_and_incumbents']} | brazos: {len(arms_out)} "
          f"| incumbentes: {len(others)} | pares nulos de incumbentes: {fixed_trials}")
    f = report["flip_rate"]
    print(f"tasa de volteo por brazo: min {f['min']:.1%} p10 {f['p10']:.1%} mediana {f['median']:.1%} "
          f"p90 {f['p90']:.1%} max {f['max']:.1%}")
    print(f"incumbentes más ruidosos que su H0 (δ_g > δ_H0): {under}/{len(others)}")
    print(f"FAR con δ fijo de H0: {report['far_fixed_h0']:.1%} | con δ por configuración (LOPO): "
          f"{report['far_per_config_lopo']:.1%}")
    print("curva δ → FAR (pool): " + "  ".join(f"{c['delta']:.2f}:{c['far']:.1%}" for c in curve[::5]))
    print(f"reporte: {args.out}")


if __name__ == "__main__":
    main()
