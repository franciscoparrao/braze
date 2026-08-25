#!/usr/bin/env python3
"""Paper 3, experimento central: tasa de aceptación falsa de los gates
publicados de optimización de harness, medida sobre pares de
configuración IDÉNTICA extraídos del banco propio.

Idea: en un par de corridas de la MISMA configuración no hay efecto que
detectar — cualquier diferencia es ruido. Si una regla de decisión
publicada "acepta" ese cambio, es un falso positivo. Contamos cuántas
veces ocurre.

Reglas implementadas (de sus papers, no de sus implementaciones — la
crítica es a la regla; ver el threat #1 del outline):
  - meta_harness : acepta el candidato con mejor score en el search set.
  - autodesign   : acepta si J_train sube Y J_dev no baja.
  - hcl          : acepta si Δ ≥ δ Y se retiene el anchor set.
  - braze        : nuestro gate — McNemar exacto pareado, α=0.05.

Uso: python3 scripts/paper3_false_acceptance.py [--out docs/paper3-false-acceptance.json]
"""
from __future__ import annotations

import argparse
import json
import pathlib
import random
from collections import defaultdict
from itertools import combinations

try:
    from scipy import stats
except ImportError:  # el gate de braze necesita scipy; el resto no
    stats = None

REPO = pathlib.Path(__file__).resolve().parent.parent
SEED = 20260825  # fijo: el muestreo de pares debe ser reproducible


# ---------------------------------------------------------------- carga

def load_runs(path: pathlib.Path):
    """Devuelve las corridas de un JSON de sweep, o [] si no aplica."""
    try:
        data = json.loads(path.read_text())
    except Exception:
        return []
    if not isinstance(data, dict) or "results" not in data:
        return []
    out = []
    for r in data["results"]:
        if not isinstance(r, dict):
            continue
        if "task_id" not in r or "passed" not in r:
            continue
        out.append({
            "file": path.name,
            "backend": r.get("backend", "?"),
            "task": r["task_id"],
            "rep": r.get("repetition", 0),
            "passed": bool(r["passed"]),
        })
    return out


def collect_replicate_groups():
    """Agrupa corridas por (archivo, backend) y devuelve solo los grupos
    con ≥2 repeticiones por tarea: ahí viven las réplicas de config
    idéntica."""
    groups = defaultdict(lambda: defaultdict(dict))  # (file,backend) -> task -> rep -> passed
    files = sorted(REPO.glob("docs/**/*.json"))
    used = 0
    for p in files:
        runs = load_runs(p)
        if not runs:
            continue
        used += 1
        for r in runs:
            groups[(r["file"], r["backend"])][r["task"]][r["rep"]] = r["passed"]

    usable = {}
    for key, tasks in groups.items():
        reps = {rep for t in tasks.values() for rep in t}
        if len(reps) < 2 or len(tasks) < 8:
            continue  # sin réplicas o suite demasiado chica para un gate
        usable[key] = tasks
    return usable, used, len(files)


def null_pairs(tasks, rep_a, rep_b):
    """Par de 'brazos' construido con dos repeticiones de la MISMA config.
    Devuelve lista de (tarea, passed_A, passed_B) con ambas presentes."""
    out = []
    for task, byrep in sorted(tasks.items()):
        if rep_a in byrep and rep_b in byrep:
            out.append((task, byrep[rep_a], byrep[rep_b]))
    return out


# ------------------------------------------------------------- las reglas

def rule_meta_harness(pairs, **_):
    """Selección por mejor score en el search set (sin holdout, sin test)."""
    a = sum(1 for _, x, _ in pairs if x)
    b = sum(1 for _, _, y in pairs if y)
    return b > a


def rule_autodesign(pairs, **_):
    """J_train sube ∧ J_dev no baja. Partimos las tareas en dos mitades
    deterministas (train/dev) porque el gate necesita ambos conjuntos."""
    half = len(pairs) // 2
    train, dev = pairs[:half], pairs[half:]
    ja_tr = sum(1 for _, x, _ in train if x)
    jb_tr = sum(1 for _, _, y in train if y)
    ja_dv = sum(1 for _, x, _ in dev if x)
    jb_dv = sum(1 for _, _, y in dev if y)
    return (jb_tr > ja_tr) and (jb_dv >= ja_dv)


def rule_hcl(pairs, delta=1, **_):
    """Δ ≥ δ en el conjunto actual ∧ retención del anchor set (no baja)."""
    half = len(pairs) // 2
    current, anchor = pairs[:half], pairs[half:]
    d = sum(1 for _, _, y in current if y) - sum(1 for _, x, _ in current if x)
    keep = sum(1 for _, _, y in anchor if y) >= sum(1 for _, x, _ in anchor if x)
    return (d >= delta) and keep


def rule_braze(pairs, alpha=0.05, **_):
    """Nuestro gate: McNemar exacto pareado, dos colas."""
    if stats is None:
        return None
    b01 = sum(1 for _, x, y in pairs if (not x) and y)
    b10 = sum(1 for _, x, y in pairs if x and (not y))
    n = b01 + b10
    if n == 0:
        return False
    p = stats.binomtest(min(b01, b10), n, 0.5).pvalue
    return (p < alpha) and (b01 > b10)


RULES = {
    "meta_harness": rule_meta_harness,
    "autodesign": rule_autodesign,
    "hcl_delta1": lambda pairs, **k: rule_hcl(pairs, delta=1),
    "hcl_delta3": lambda pairs, **k: rule_hcl(pairs, delta=3),
    "braze_mcnemar": rule_braze,
}


# ------------------------------------------------------------------ main

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="docs/paper3-false-acceptance.json")
    ap.add_argument("--max-pairs-per-group", type=int, default=6)
    args = ap.parse_args()

    random.seed(SEED)
    usable, used_files, total_files = collect_replicate_groups()

    accepts = defaultdict(int)
    trials = 0
    per_group = []

    for (fname, backend), tasks in sorted(usable.items()):
        reps = sorted({rep for t in tasks.values() for rep in t})
        combos = list(combinations(reps, 2))
        random.shuffle(combos)
        combos = combos[: args.max_pairs_per_group]
        g_trials, g_acc = 0, defaultdict(int)
        for ra, rb in combos:
            pairs = null_pairs(tasks, ra, rb)
            if len(pairs) < 8:
                continue
            # ambos órdenes: el ruido no tiene dirección privilegiada
            for p in (pairs, [(t, y, x) for t, x, y in pairs]):
                trials += 1
                g_trials += 1
                for name, fn in RULES.items():
                    r = fn(p)
                    if r:
                        accepts[name] += 1
                        g_acc[name] += 1
        if g_trials:
            per_group.append({
                "file": fname, "backend": backend[:70],
                "tasks": len(tasks), "reps": len(reps), "trials": g_trials,
                "accepts": dict(g_acc),
            })

    report = {
        "seed": SEED,
        "files_scanned": total_files,
        "files_with_runs": used_files,
        "groups_with_replicates": len(usable),
        "null_trials": trials,
        "false_acceptance_rate": {
            name: (accepts[name] / trials if trials else None) for name in RULES
        },
        "raw_accepts": dict(accepts),
        "per_group": per_group,
    }

    out = REPO / args.out
    out.write_text(json.dumps(report, indent=1))

    print(f"archivos escaneados: {total_files} | con corridas: {used_files}")
    print(f"grupos con réplicas de config idéntica: {len(usable)}")
    print(f"comparaciones nulas construidas: {trials}\n")
    print(f"{'regla':<16} {'acepta':>8} {'tasa':>8}")
    for name in RULES:
        n = accepts[name]
        rate = n / trials if trials else 0
        print(f"{name:<16} {n:>8} {rate:>7.1%}")
    print(f"\nreporte: {args.out}")


if __name__ == "__main__":
    main()
