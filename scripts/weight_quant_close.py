"""Cierre del A/B de weight-quant (MXFP4 nativo vs Q3_K_M en gpt-oss:20b),
aplicando los criterios pre-registrados de
`docs/hypothesis-2026-08-22-weight-quant-ab.md`.

Pareo por (tarea, seed) sobre las seeds que los tres brazos comparten.
El pre-registro autoriza explícitamente bajar a 2 seeds si el tiempo
apremia —"nunca eliminar el brazo A/A"— así que el sweep disponible
(A x3, B x2, E x2) cumple el diseño mínimo declarado y no es una corrida
truncada.

Uso:  python3 scripts/weight_quant_close.py <dir-con-los-json>

Los JSON viven en `nitro:~/braze/docs/sweep-wq-*.json` y NO están
versionados — mismo caveat que `fragility_vs_discrimination.py`.
"""

import glob
import json
import math
import os
import statistics
import sys

if len(sys.argv) != 2:
    print(__doc__)
    sys.exit(2)
HERE = sys.argv[1]


def arm(prefix):
    """{(task, seed): (passed, strict, ms)}"""
    out = {}
    for f in sorted(glob.glob(os.path.join(HERE, prefix))):
        d = json.load(open(f))
        seed = d["metadata"]["sampling"]["seed"]
        for r in d["results"]:
            out[(r["task_id"], seed)] = (
                bool(r["passed"]),
                bool(r.get("passed_strict", r["passed"])),
                r.get("wall_time_ms") or 0,
            )
    return out


A = arm("sweep-wq-A-*.json")
B = arm("sweep-wq-B-*.json")
E = arm("sweep-wq-E-*.json")

seeds_A = sorted({s for _, s in A})
seeds_B = sorted({s for _, s in B})
seeds_E = sorted({s for _, s in E})
print(f"seeds: A={seeds_A}  B={seeds_B}  E={seeds_E}")
print("El pre-registro autoriza bajar a 2 seeds, nunca quitar el A/A.\n")


def rate(d, seeds):
    cells = [v for (t, s), v in d.items() if s in seeds]
    p = sum(1 for c in cells if c[0])
    st = sum(1 for c in cells if c[1])
    ms = [c[2] for c in cells if c[2]]
    return p, st, len(cells), (statistics.mean(ms) / 1000 if ms else float("nan"))


def mcnemar_exact(pairs):
    """pairs: [(x, y)] booleanos. Devuelve (b, c, p) de dos colas."""
    b = sum(1 for x, y in pairs if x and not y)
    c = sum(1 for x, y in pairs if y and not x)
    n = b + c
    if n == 0:
        return b, c, 1.0
    # binomial exacta con p=0.5, dos colas
    k = min(b, c)
    tail = sum(math.comb(n, i) for i in range(k + 1)) / (2**n)
    return b, c, min(1.0, 2 * tail)


print("=" * 62)
print("BRAZOS (métrica dual, seeds compartidas)")
print("=" * 62)
for name, d, seeds in (("A  MXFP4", A, seeds_B), ("E  MXFP4 (A/A)", E, seeds_B), ("B  Q3_K_M", B, seeds_B)):
    p, st, n, secs = rate(d, seeds)
    print(f"  {name:16} {p:3}/{n}  ({p / n:.1%})   strict {st}/{n}   {secs:6.0f} s/tarea")

# Piso de ruido: A contra E, mismo brazo, seeds compartidas.
common_AE = sorted(set(A) & set(E))
floor_pairs = [(A[k][0], E[k][0]) for k in common_AE]
b, c, p_floor = mcnemar_exact(floor_pairs)
disc_floor = b + c
print()
print("=" * 62)
print("PISO DE RUIDO (A/A: MXFP4 contra sí mismo)")
print("=" * 62)
print(f"  pares: {len(floor_pairs)}   discordantes: {disc_floor}  ({b} a favor de A, {c} de E)")
print(f"  McNemar exacto p = {p_floor:.4f}")
print(f"  discordancia = {disc_floor / len(floor_pairs):.1%} de las celdas  <-- el PISO")

# Contraste del tratamiento.
common_AB = sorted(set(A) & set(B))
treat_pairs = [(A[k][0], B[k][0]) for k in common_AB]
b2, c2, p_treat = mcnemar_exact(treat_pairs)
print()
print("=" * 62)
print("TRATAMIENTO (B = Q3_K_M contra A = MXFP4)")
print("=" * 62)
print(f"  pares: {len(treat_pairs)}   discordantes: {b2 + c2}  ({b2} a favor de A, {c2} de B)")
print(f"  McNemar exacto p = {p_treat:.6f}")
delta = (sum(1 for x, y in treat_pairs if y) - sum(1 for x, y in treat_pairs if x)) / len(treat_pairs)
print(f"  delta de pass rate (B - A) = {delta:+.1%}")

print()
print("=" * 62)
print("VEREDICTO CONTRA LOS CRITERIOS PRE-REGISTRADOS")
print("=" * 62)
print(f"  1. Piso primero: discordancia A/A = {disc_floor}/{len(floor_pairs)} celdas.")
outside = (b2 + c2) > disc_floor and p_treat < 0.05
print(f"  2/3. ¿El pass rate de B cae FUERA del piso? {'SÍ' if outside else 'NO'}")
print(f"       ({b2 + c2} discordantes del tratamiento vs {disc_floor} del piso; p={p_treat:.2g})")
_, _, _, sA = rate(A, seeds_B)
_, _, _, sB = rate(B, seeds_B)
print(f"  H1 (velocidad): A={sA:.0f}s  B={sB:.0f}s  -> B es {'MÁS RÁPIDO' if sB < sA else 'MÁS LENTO'} ({(sB - sA) / sA:+.1%})")
