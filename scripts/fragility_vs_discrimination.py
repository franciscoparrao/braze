#!/usr/bin/env python3
"""Fragilidad vs discriminación por ítem — réplica cross-dominio del
hallazgo de Parupudi (arXiv:2608.21382 § 4.4) sobre datos agénticos.

Él mide, sobre 3.679 ítems de opción múltiple y 26 configuraciones de
harness, que la discriminación de un ítem correlaciona con su fragilidad
(r = +0,28, IC95% [0,25, 0,30]), y concluye que comprimir un benchmark
por discriminación *conserva* los ítems que cargan la sensibilidad al
harness. Si eso transfiere al régimen agéntico, tendría una consecuencia
directa acá: `discriminating.toml` se construyó eligiendo tareas cerca
de la frontera del modelo, que es discriminación, y su piso de ruido
sería consecuencia del criterio de diseño y no mala suerte.

Se corren DOS diseños porque miden fenómenos distintos:

  1. fragilidad entre RÉPLICAS  — la varianza que Parupudi excluye por
     diseño (su decoding es greedy y de semilla única). Es la que este
     proyecto mide con sus brazos A/A.
  2. fragilidad entre CONFIGURACIONES — su unidad de análisis. Acá las
     configuraciones son tipos de KV cache (f16a / q8_0 / q4_0), tres
     condiciones que cualquier evaluador defendería.

En ambos, fragilidad y discriminación salen de conjuntos de corridas
DISJUNTOS: compartirlas induciría correlación espuria.

Uso:
    python3 scripts/fragility_vs_discrimination.py <dir-con-los-json>

El directorio debe contener los sweeps de `discriminating.toml`:
  sweep-wq-{A,E}-s*.json      MXFP4, réplicas exactas (fuerte)
  sweep-wq-B-s*.json          Q3_K_M (débil)
  sweep-kv-quant-{f16a,q8,q4}-s*.json   MXFP4 bajo 3 KV caches

NOTA DE REPRODUCIBILIDAD: al momento de escribir esto esos JSON NO están
versionados en el repo (viven en `nitro:~/braze/docs/`). Sin ellos este
script no corre. Es la misma decisión diferida que el resto de los
sweeps untracked de `docs/`.
"""

import glob
import json
import os
import random
import statistics
import sys

SEED = 20260827  # el análisis es una función determinista de esta semilla


def load(d, pattern):
    """{task_id: [bit, ...]} sobre todos los archivos que matchean."""
    out = {}
    for f in sorted(glob.glob(os.path.join(d, pattern))):
        for r in json.load(open(f))["results"]:
            out.setdefault(r["task_id"], []).append(1 if r["passed"] else 0)
    return out


def load_by_config(d, patterns):
    """{task_id: {config: [bits]}}"""
    out = {}
    for cfg, pat in patterns.items():
        for f in sorted(glob.glob(os.path.join(d, pat))):
            for r in json.load(open(f))["results"]:
                out.setdefault(r["task_id"], {}).setdefault(cfg, []).append(
                    1 if r["passed"] else 0
                )
    return out


def pearson(xs, ys):
    mx, my = statistics.mean(xs), statistics.mean(ys)
    num = sum((x - mx) * (y - my) for x, y in zip(xs, ys))
    dx = sum((x - mx) ** 2 for x in xs) ** 0.5
    dy = sum((y - my) ** 2 for y in ys) ** 0.5
    return num / (dx * dy) if dx and dy else float("nan")


def rank(v):
    order = sorted(range(len(v)), key=lambda i: v[i])
    r = [0.0] * len(v)
    i = 0
    while i < len(order):
        j = i
        while j + 1 < len(order) and v[order[j + 1]] == v[order[i]]:
            j += 1
        avg = (i + j) / 2 + 1
        for m in range(i, j + 1):
            r[order[m]] = avg
        i = j + 1
    return r


def spearman(xs, ys):
    return pearson(rank(xs), rank(ys))


def boot_ci(xs, ys, n=10000):
    """IC bootstrap sobre ÍTEMS — la misma unidad de resampleo que
    Parupudi, y la que corresponde a "¿otra suite habría dado otra
    correlación?"."""
    rng = random.Random(SEED)
    vals = []
    for _ in range(n):
        idx = [rng.randrange(len(xs)) for _ in range(len(xs))]
        v = spearman([xs[i] for i in idx], [ys[i] for i in idx])
        if v == v:  # descarta NaN de remuestras degeneradas
            vals.append(v)
    vals.sort()
    return vals[int(0.025 * len(vals))], vals[int(0.975 * len(vals))]


def design_replicas(d):
    """Diseño 1: fragilidad entre réplicas exactas."""
    frag = load(d, "sweep-wq-[AE]-*.json")
    strong = load(d, "sweep-kv-quant-*.json")
    weak = load(d, "sweep-wq-B-*.json")
    tasks = sorted(set(frag) & set(strong) & set(weak))

    rows = []
    for t in tasks:
        bits = frag[t]
        k, n = sum(bits), len(bits)
        rows.append(
            {
                "task": t,
                "k": k,
                "n": n,
                "fragile": 0 < k < n,
                # Discordancia normalizada: conserva la diferencia entre
                # 1/5 y 2/5, que un booleano perdería.
                "frag": min(k, n - k) / (n / 2),
                "disc": statistics.mean(strong[t]) - statistics.mean(weak[t]),
            }
        )

    print("=" * 68)
    print("DISEÑO 1 — fragilidad entre RÉPLICAS EXACTAS (5 corridas MXFP4)")
    print("=" * 68)
    rc = sum(1 for r in rows if r["k"] == r["n"])
    rw = sum(1 for r in rows if r["k"] == 0)
    fr = sum(1 for r in rows if r["fragile"])
    N = len(rows)
    print(f"  robust-correct (n/n) : {rc:2}/{N}  ({rc / N:.0%})")
    print(f"  fragile              : {fr:2}/{N}  ({fr / N:.0%})")
    print(f"  robust-wrong   (0/n) : {rw:2}/{N}  ({rw / N:.0%})")

    robust = rc / N
    optimistic = sum(1 for r in rows if r["k"] > 0) / N
    mean_acc = statistics.mean([r["k"] / r["n"] for r in rows])
    print(f"\n  banda: robust={robust:.3f}  media={mean_acc:.3f}  optimista={optimistic:.3f}")
    print(f"  amplitud = {(optimistic - robust) * 100:.1f} pp")
    print(f"  fracción run-lucky = {(mean_acc - robust) / mean_acc:.2f}")

    xs = [r["frag"] for r in rows]
    ys = [r["disc"] for r in rows]
    lo, hi = boot_ci(xs, ys)
    print(f"\n  Spearman rho = {spearman(xs, ys):+.3f}  IC95% [{lo:+.3f}, {hi:+.3f}]  n={N}")
    return rows


def design_configs(d):
    """Diseño 2: fragilidad entre configuraciones — el análogo directo."""
    by_cfg = load_by_config(
        d,
        {
            "f16a": "sweep-kv-quant-f16a-*.json",
            "q8": "sweep-kv-quant-q8-*.json",
            "q4": "sweep-kv-quant-q4-*.json",
        },
    )
    strong = load(d, "sweep-wq-[AE]-*.json")
    weak = load(d, "sweep-wq-B-*.json")
    tasks = sorted(set(by_cfg) & set(strong) & set(weak))

    rows = []
    for t in tasks:
        # Se agregan las semillas DENTRO de cada config para no confundir
        # el efecto de la configuración con el de la semilla.
        means = [statistics.mean(by_cfg[t][c]) for c in ("f16a", "q8", "q4")]
        rows.append(
            {
                "task": t,
                "spread": max(means) - min(means),
                "disc": statistics.mean(strong[t]) - statistics.mean(weak[t]),
            }
        )

    print()
    print("=" * 68)
    print("DISEÑO 2 — fragilidad entre CONFIGURACIONES (KV cache)")
    print("=" * 68)
    moved = sum(1 for r in rows if r["spread"] > 0)
    N = len(rows)
    print(f"  ítems que la config mueve: {moved}/{N}  ({moved / N:.0%})")
    print(f"  spread medio: {statistics.mean([r['spread'] for r in rows]):.3f}")

    xs = [r["spread"] for r in rows]
    ys = [r["disc"] for r in rows]
    lo, hi = boot_ci(xs, ys)
    print(f"\n  Spearman rho = {spearman(xs, ys):+.3f}  IC95% [{lo:+.3f}, {hi:+.3f}]  n={N}")
    print("  (Parupudi, MCQ: +0,28  IC95% [0,25, 0,30]  n=3.679)")

    ordered = sorted(rows, key=lambda r: r["disc"], reverse=True)
    q = max(1, N // 4)
    f_top = statistics.mean([r["spread"] for r in ordered[:q]])
    f_rest = statistics.mean([r["spread"] for r in ordered[q:]])
    print(f"\n  spread, cuartil más discriminativo (n={q}): {f_top:.3f}")
    print(f"  spread, el resto                   (n={N - q}): {f_rest:.3f}")
    print(f"  brecha = {f_top - f_rest:+.3f}   (Parupudi: 0,96 vs 0,85)")
    return rows


def main():
    if len(sys.argv) != 2:
        print(__doc__)
        sys.exit(2)
    d = sys.argv[1]
    if not glob.glob(os.path.join(d, "sweep-wq-*.json")):
        print(f"sin sweeps de weight-quant en {d} — ver NOTA DE REPRODUCIBILIDAD")
        sys.exit(1)
    r1 = design_replicas(d)
    design_configs(d)

    print()
    print("=" * 68)
    print("POR ÍTEM (diseño 1, ordenado por discriminación)")
    print("=" * 68)
    print(f"{'tarea':<34}{'passed':>9}{'frag':>7}{'disc':>8}")
    for r in sorted(r1, key=lambda r: r["disc"], reverse=True):
        print(f"{r['task']:<34}{r['k']}/{r['n']:<7}{r['frag']:>7.2f}{r['disc']:>8.2f}")


if __name__ == "__main__":
    main()
