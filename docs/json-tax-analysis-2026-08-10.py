#!/usr/bin/env python3
"""Análisis pre-registrado del A/B del impuesto JSON (edit-fence).

Escrito y congelado ANTES de ver los resultados del sweep (el sweep
corría al momento de escribir esto; el criterio vive en
docs/hypothesis-2026-08-10-json-tax-edit-fence.md). Orden obligado:

  1. Validación de contaminación del brazo B (fence_edits vs
     ejecuciones de edit_file). Si la fuga nativa domina, el sweep se
     declara inválido ANTES de mirar pass rates.
  2. Pass rates por executor: B − A con IC Newcombe 95% y McNemar
     exacto pareado por (task_id, repetition).
  3. Desglose por clase de tarea (edit / create / other, derivada de la
     suite: expect_file_contains sobre un path presente en setup_files
     = edit; sobre un path nuevo = create) — el criterio exige que el
     delta se concentre en 'edit' y que 'other' no se dañe.

Uso: python3 docs/json-tax-analysis-2026-08-10.py <sweep.json> [suite.toml]
"""

import json
import math
import sys
from collections import defaultdict

try:
    import tomllib
except ImportError:  # < 3.11
    tomllib = None

FENCE_SUFFIX = "+ablate:edit-fence"


def wilson(p_hat, n, z=1.959964):
    if n == 0:
        return (0.0, 1.0)
    denom = 1 + z * z / n
    center = (p_hat + z * z / (2 * n)) / denom
    half = z * math.sqrt(p_hat * (1 - p_hat) / n + z * z / (4 * n * n)) / denom
    return (center - half, center + half)


def newcombe_diff(p1, n1, p2, n2):
    """IC 95% de p1 - p2 (método de Newcombe basado en Wilson)."""
    l1, u1 = wilson(p1, n1)
    l2, u2 = wilson(p2, n2)
    return (p1 - p2 - math.sqrt((p1 - l1) ** 2 + (u2 - p2) ** 2),
            p1 - p2 + math.sqrt((u1 - p1) ** 2 + (p2 - l2) ** 2))


def mcnemar_exact(b, c):
    """p bilateral exacto sobre los pares discordantes (binomial 0.5)."""
    n = b + c
    if n == 0:
        return 1.0
    k = min(b, c)
    tail = sum(math.comb(n, i) for i in range(0, k + 1)) / 2 ** n
    return min(1.0, 2 * tail)


def classify_tasks(suite_path):
    """task_id -> 'edit' | 'create' | 'other' según la suite TOML."""
    if not suite_path or tomllib is None:
        return {}
    with open(suite_path, "rb") as fh:
        suite = tomllib.load(fh)
    classes = {}
    for t in suite.get("tasks", []):
        setup = set((t.get("setup_files") or {}).keys())
        expects = set((t.get("expect_file_contains") or {}).keys())
        if expects & setup:
            classes[t["id"]] = "edit"
        elif expects:
            classes[t["id"]] = "create"
        else:
            classes[t["id"]] = "other"
    return classes


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    data = json.load(open(sys.argv[1]))
    rows = data["results"] if isinstance(data, dict) and "results" in data else data
    classes = classify_tasks(sys.argv[2] if len(sys.argv) > 2 else None)

    by_backend = defaultdict(list)
    for r in rows:
        by_backend[r["backend"]].append(r)

    executors = sorted(
        b for b in by_backend if not b.endswith(FENCE_SUFFIX)
        and b + FENCE_SUFFIX in by_backend
    )
    if not executors:
        sys.exit("No hay pares A/B (base, base+ablate:edit-fence) en el JSON.")

    # ── Paso 1: contaminación (ANTES de cualquier pass rate) ──────────
    print("== Paso 1: validación de contaminación del brazo B ==")
    contaminated = False
    for base in executors:
        arm_b = by_backend[base + FENCE_SUFFIX]
        fence = sum(r.get("fence_edits", 0) for r in arm_b)
        edit_calls = sum(r.get("tool_call_names", []).count("edit_file") for r in arm_b)
        write_calls = sum(r.get("tool_call_names", []).count("write_file") for r in arm_b)
        leak = edit_calls - fence  # edit_file despachadas que NO vinieron del fence
        status = "OK"
        if edit_calls > 0 and leak > fence:
            status = "CONTAMINADO (fuga nativa domina)"
            contaminated = True
        print(f"  {base:24} fence={fence:3} edit_file_total={edit_calls:3} "
              f"fuga_nativa={leak:3} write_file={write_calls:3}  {status}")
    if contaminated:
        print("\n*** SWEEP INVÁLIDO según pre-registro: la fuga nativa domina en"
              " al menos un executor. Los pass rates de abajo se imprimen solo"
              " como diagnóstico, NO como resultado del A/B. ***")

    # ── Paso 2: B − A por executor ────────────────────────────────────
    print("\n== Paso 2: pass rate B − A por executor (pareado) ==")
    for base in executors:
        a_rows = {(r["task_id"], r["repetition"]): r["passed"] for r in by_backend[base]}
        b_rows = {(r["task_id"], r["repetition"]): r["passed"]
                  for r in by_backend[base + FENCE_SUFFIX]}
        keys = sorted(set(a_rows) & set(b_rows))
        pa = sum(a_rows[k] for k in keys)
        pb = sum(b_rows[k] for k in keys)
        n = len(keys)
        disc_b = sum(1 for k in keys if b_rows[k] and not a_rows[k])  # B gana
        disc_c = sum(1 for k in keys if a_rows[k] and not b_rows[k])  # A gana
        lo, hi = newcombe_diff(pb / n, n, pa / n, n)
        p = mcnemar_exact(disc_b, disc_c)
        print(f"  {base:24} A={pa}/{n} B={pb}/{n}  Δ={100*(pb-pa)/n:+.1f}pp "
              f"IC95=[{100*lo:+.1f},{100*hi:+.1f}]  discordantes B+/A+={disc_b}/{disc_c} "
              f"McNemar p={p:.4f}")

    # ── Paso 3: desglose por clase de tarea ───────────────────────────
    if classes:
        print("\n== Paso 3: Δ (B−A) por clase de tarea ==")
        for base in executors:
            a_rows = {(r["task_id"], r["repetition"]): r["passed"] for r in by_backend[base]}
            b_rows = {(r["task_id"], r["repetition"]): r["passed"]
                      for r in by_backend[base + FENCE_SUFFIX]}
            keys = sorted(set(a_rows) & set(b_rows))
            parts = []
            for cls in ("edit", "create", "other"):
                ks = [k for k in keys if classes.get(k[0]) == cls]
                if not ks:
                    continue
                d = sum(b_rows[k] for k in ks) - sum(a_rows[k] for k in ks)
                parts.append(f"{cls}: {d:+d}/{len(ks)}")
            print(f"  {base:24} {'  '.join(parts)}")

    print("\nCriterio pre-registrado: adoptar si en algún débil Δ ≥ +10pp con"
          " IC fuera de cero, mecanismo limpio (paso 1 OK, fence>0) y sin"
          " daño en 'other'; rechazar si B ≤ A en los tres débiles."
          " gpt-oss:20b se reporta, no decide.")


if __name__ == "__main__":
    main()
