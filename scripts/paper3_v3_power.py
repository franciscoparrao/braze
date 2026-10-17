#!/usr/bin/env python3
"""Paper 3 v3: potencia de δ fijo de H0 vs δ re-derivado por configuración.

Se inyecta una mejora real de tamaño e (fracción de la suite) sobre cada par nulo de
los incumbentes: Δ' = Δ + e. Potencia = fracción de Δ' aceptados (Δ' > δ). Mismos
datos, estimador de cola (máximo |Δ|) y LOPO que el resultado principal; H0 = primer
brazo y H0 al azar (200 sorteos).

Uso: python3 scripts/paper3_v3_power.py [--out docs/paper3-v3-power.json]
"""
import argparse, json, random
from paper3_v3_robustness import REPO, collect_replicate_groups, rep_pass_counts, unordered_deltas, estimate
from collections import defaultdict

EFFECTS = (0.0, 0.03, 0.05, 0.10, 0.15, 0.20)

def run(sweeps, pick, rng):
    acc = {e: [0, 0, 0] for e in EFFECTS}  # trials, fijo, por-config
    for arms in sweeps.values():
        h0 = arms[0] if pick == "first" else rng.choice(arms)
        d_fix = estimate(list(h0["d"].values()), "max")
        for arm in arms:
            if arm is h0:
                continue
            for pair, v in arm["d"].items():
                rest = [w for p, w in arm["d"].items() if p != pair]
                if not rest:
                    continue
                d_cfg = estimate(rest, "max")
                for x in (v, -v):
                    for e in EFFECTS:
                        a = acc[e]; a[0] += 1; a[1] += x + e > d_fix; a[2] += x + e > d_cfg
    return acc

def main():
    ap = argparse.ArgumentParser(); ap.add_argument("--out", default="docs/paper3-v3-power.json"); args = ap.parse_args()
    rng = random.Random(20261017)
    usable, _, _ = collect_replicate_groups()
    by_file = defaultdict(list)
    for (f, b), tasks in sorted(usable.items()):
        reps, common = rep_pass_counts(tasks)
        if len(reps) >= 3 and len(common) >= 8:
            by_file[f].append({"backend": b, "d": unordered_deltas(tasks, reps, common)})
    sweeps = {f: a for f, a in by_file.items() if len(a) >= 2}
    out = {}
    for pick in ("first", "random"):
        tot = {e: [0, 0, 0] for e in EFFECTS}
        for _ in range(1 if pick == "first" else 200):
            for e, v in run(sweeps, pick, rng).items():
                for i in range(3): tot[e][i] += v[i]
        out[pick] = {str(e): {"fixed_h0": v[1] / v[0], "per_config": v[2] / v[0]} for e, v in tot.items()}
        print(f"H0={pick}: " + "  ".join(f"e={e:.2f}: fijo {v[1]/v[0]:.0%} / por-config {v[2]/v[0]:.0%}" for e, v in tot.items()))
    (REPO / args.out).write_text(json.dumps(out, indent=1)); print("reporte:", args.out)

if __name__ == "__main__":
    main()
