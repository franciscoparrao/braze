#!/usr/bin/env python3
"""Descomposición de varianza: ¿cuánto del rendimiento explica el HARNESS
y cuánto el MODELO?

Propuesta 3 de "Stop Comparing LLM Agents Without Disclosing the Harness"
(arXiv 2605.23950v1), la única de su lista que braze no cumplía todavía:
la divulgación del harness, el protocolo harness-first y los experimentos
swap-harness ya están cubiertos por braze-bench (`metadata.backend_specs`,
`+ablate:`, McNemar pareado).

NO CORRE NINGÚN MODELO. Opera sobre los sweeps ya archivados en `docs/`.

DISEÑO. Se necesita cruce real (mismos modelos × distintas configuraciones
de harness). Del corpus se extrae el subdiseño factorial completo:

    {qwen2.5:3b, qwen2.5:7b, qwen3.5-coder} × {base, +lead, +plan, +plan+lead}

Filtros, todos por reglas que el propio proyecto ya aplica:

  - Un solo `suite_fingerprint`: comparar entre suites distintas mezcla la
    dificultad de las tareas con el efecto que se quiere medir.
  - Se excluyen los archivos que el proyecto marcó en su nombre como no
    usables (`contaminated`, `partial`, `diagnostic`, `smoke`).
  - `run_error` fuera del denominador, igual que hace el bench con los
    HarnessError.
  - Solo tareas presentes en LAS 12 celdas, para que el bloque `task` esté
    balanceado y no arrastre la comparación.

MÉTODO. Una observación por celda (modelo, config, tarea) = su pass rate.
Diseño factorial completo con n=1 por celda, del que se particiona la suma
de cuadrados en efectos principales (modelo, config, tarea), la interacción
modelo×config y el residuo. Se reporta eta cuadrado (SS_factor/SS_total).

La tarea entra como BLOQUE, no como hallazgo: que las tareas difieran entre
sí es trivial y esperable. Lo que se compara es modelo contra config, y por
eso se reporta también eta cuadrado PARCIAL sobre la varianza no explicada
por la tarea, que es la cifra honesta para la pregunta del paper.
"""

import json
import pathlib
from collections import defaultdict

import numpy as np

DOCS = pathlib.Path(__file__).resolve().parent.parent / "docs"
BAD = ("contaminated", "partial", "diagnostic", "smoke", "offline-grades")
SUITE_FP = "8deba9d2bffdf3c1"  # default.toml

MODELS = ["ollama:qwen2.5:3b", "ollama:qwen2.5:7b", "ollama:qwen3.5-coder"]
CONFIGS = ["base", "+lead", "+plan", "+plan+lead"]


def split_spec(spec):
    """'ollama:qwen2.5:3b+plan:x+lead:y' -> ('ollama:qwen2.5:3b', '+plan+lead').

    Las palancas se identifican por NOMBRE, no por su valor: lo que define
    la configuración de harness es qué palancas están activas, no con qué
    modelo auxiliar se instancian.
    """
    parts = spec.split("+")
    levers = [p.split(":")[0] for p in parts[1:]]
    return parts[0], ("+" + "+".join(levers)) if levers else "base"


def load():
    """[(modelo, config, tarea, passed)] del corpus utilizable."""
    rows = []
    files = set()
    commits = set()
    for path in sorted(DOCS.glob("*.json")):
        if any(b in path.name for b in BAD):
            continue
        try:
            d = json.loads(path.read_text(errors="replace"))
        except Exception:
            continue
        if not isinstance(d, dict) or "results" not in d:
            continue
        meta = d.get("metadata", {}) or {}
        if meta.get("suite_fingerprint") != SUITE_FP:
            continue
        used = False
        for r in d["results"]:
            if not isinstance(r, dict) or "backend" not in r:
                continue
            if r.get("run_error"):
                continue
            model, config = split_spec(r["backend"])
            if model not in MODELS or config not in CONFIGS:
                continue
            rows.append((model, config, r.get("task_id"), bool(r.get("passed"))))
            used = True
        if used:
            files.add(path.name)
            commits.add((meta.get("braze_git_commit") or "?")[:8])
    return rows, files, commits


def build_matrix(rows):
    """Pass rate por (modelo, config, tarea), solo tareas en las 12 celdas."""
    agg = defaultdict(lambda: [0, 0])
    for model, config, task, passed in rows:
        cell = agg[(model, config, task)]
        cell[0] += 1
        cell[1] += 1 if passed else 0

    tasks_per_cell = defaultdict(set)
    for (model, config, task) in agg:
        tasks_per_cell[(model, config)].add(task)
    common = set.intersection(*tasks_per_cell.values())
    tasks = sorted(common)

    matrix = np.zeros((len(MODELS), len(CONFIGS), len(tasks)))
    counts = np.zeros_like(matrix)
    for i, model in enumerate(MODELS):
        for j, config in enumerate(CONFIGS):
            for k, task in enumerate(tasks):
                n, p = agg[(model, config, task)]
                matrix[i, j, k] = p / n
                counts[i, j, k] = n
    return matrix, tasks, counts


def decompose(x):
    """Partición de SS de un factorial completo con n=1 por celda."""
    M, C, T = x.shape
    grand = x.mean()
    ss_total = ((x - grand) ** 2).sum()

    ss_model = C * T * ((x.mean(axis=(1, 2)) - grand) ** 2).sum()
    ss_config = M * T * ((x.mean(axis=(0, 2)) - grand) ** 2).sum()
    ss_task = M * C * ((x.mean(axis=(0, 1)) - grand) ** 2).sum()

    # Las TRES interacciones de dos vías, no solo modelo×config: mandar
    # modelo×tarea y config×tarea al residuo las haría pasar por ruido
    # cuando son efectos reales ("esta palanca ayuda solo en ciertas
    # tareas" es justamente lo que la serie de sweeps viene mostrando).
    # Con n=1 por celda, el residuo queda siendo la interacción triple.
    m_, c_, t_ = x.mean(axis=(1, 2)), x.mean(axis=(0, 2)), x.mean(axis=(0, 1))

    mc = x.mean(axis=2) - m_[:, None] - c_[None, :] + grand
    ss_mc = T * (mc**2).sum()

    mt = x.mean(axis=1) - m_[:, None] - t_[None, :] + grand
    ss_mt = C * (mt**2).sum()

    ct = x.mean(axis=0) - c_[:, None] - t_[None, :] + grand
    ss_ct = M * (ct**2).sum()

    ss_resid = ss_total - ss_model - ss_config - ss_task - ss_mc - ss_mt - ss_ct
    return {
        "ss_total": ss_total,
        "modelo": ss_model,
        "config_harness": ss_config,
        "tarea": ss_task,
        "modelo_x_config": ss_mc,
        "modelo_x_tarea": ss_mt,
        "config_x_tarea": ss_ct,
        "residuo": ss_resid,
    }


def main():
    rows, files, commits = load()
    matrix, tasks, counts = build_matrix(rows)
    ss = decompose(matrix)

    print("=" * 68)
    print("DESCOMPOSICIÓN DE VARIANZA — modelo vs harness")
    print("=" * 68)
    print(f"corridas usadas:     {len(rows)}")
    print(f"archivos de sweep:   {len(files)}")
    print(f"commits del binario: {sorted(commits)}")
    print(f"diseño:              {len(MODELS)} modelos × {len(CONFIGS)} configs × {len(tasks)} tareas")
    print(f"n por celda-tarea:   min={int(counts.min())} mediana={int(np.median(counts))} max={int(counts.max())}")
    print()

    print("PASS RATE por celda (promedio sobre tareas comunes)")
    print(f"{'modelo':<24}" + "".join(f"{c:>14}" for c in CONFIGS))
    for i, model in enumerate(MODELS):
        print(f"{model:<24}" + "".join(f"{matrix[i, j].mean():>14.3f}" for j in range(len(CONFIGS))))
    print()

    print("PARTICIÓN DE LA SUMA DE CUADRADOS")
    total = ss["ss_total"]
    non_task = total - ss["tarea"]
    print(f"{'fuente':<20}{'SS':>10}{'eta²':>9}{'eta² sin tarea':>17}")
    for key in (
        "tarea",
        "modelo",
        "config_harness",
        "modelo_x_config",
        "modelo_x_tarea",
        "config_x_tarea",
        "residuo",
    ):
        partial = "" if key == "tarea" else f"{ss[key] / non_task:>17.3f}"
        print(f"{key:<20}{ss[key]:>10.2f}{ss[key] / total:>9.3f}{partial}")
    print()

    ratio = ss["config_harness"] / ss["modelo"] if ss["modelo"] else float("inf")
    print(f"config_harness / modelo = {ratio:.2f}×")
    print()

    print("¿SE INVIERTE EL RANKING DE MODELOS AL CAMBIAR EL HARNESS?")
    base_order = None
    for j, config in enumerate(CONFIGS):
        order = [MODELS[i] for i in np.argsort(-matrix[:, j, :].mean(axis=1))]
        if base_order is None:
            base_order = order
        flag = "" if order == base_order else "   <-- INVERSIÓN"
        print(f"  {config:<12} {' > '.join(m.replace('ollama:', '') for m in order)}{flag}")
    print()

    print("SPREAD en puntos porcentuales (lo interpretable)")
    per_model = matrix.mean(axis=(1, 2))
    per_config = matrix.mean(axis=(0, 2))
    print(f"  entre modelos (promediando configs): {100 * (per_model.max() - per_model.min()):.1f} pp")
    print(f"  entre configs (promediando modelos): {100 * (per_config.max() - per_config.min()):.1f} pp")
    for i, model in enumerate(MODELS):
        row = matrix[i].mean(axis=1)
        print(f"    dentro de {model:<24} el harness mueve {100 * (row.max() - row.min()):.1f} pp")

    # Sensibilidad: las celdas con n chico traen pass rates ruidosos que
    # podrían inflar las interacciones. Se re-corre exigiendo un mínimo
    # de corridas por celda-tarea. El ratio config/modelo NO sobrevive
    # (1.02x -> 0.73x), la interacción config×tarea SÍ — y por eso el
    # documento reporta la segunda como hallazgo y la primera como empate.
    print()
    print("SENSIBILIDAD (n mínimo por celda-tarea)")
    print(f"{'min_n':>6}{'tareas':>8}{'modelo':>9}{'config':>9}{'config×tarea':>15}{'ratio c/m':>11}")
    sensitivity = []
    for min_n in (1, 5):
        agg = defaultdict(lambda: [0, 0])
        for model, config, task, passed in rows:
            cell = agg[(model, config, task)]
            cell[0] += 1
            cell[1] += 1 if passed else 0
        keep = sorted(
            t
            for t in {t for (_, _, t) in agg}
            if all(agg.get((m, c, t), [0, 0])[0] >= min_n for m in MODELS for c in CONFIGS)
        )
        if len(keep) < 4:
            continue
        sub = np.zeros((len(MODELS), len(CONFIGS), len(keep)))
        for i, model in enumerate(MODELS):
            for j, config in enumerate(CONFIGS):
                for k, task in enumerate(keep):
                    n, p = agg[(model, config, task)]
                    sub[i, j, k] = p / n
        s = decompose(sub)
        tot = s["ss_total"]
        ratio_s = s["config_harness"] / s["modelo"] if s["modelo"] else float("inf")
        print(
            f"{min_n:>6}{len(keep):>8}{s['modelo'] / tot:>9.3f}{s['config_harness'] / tot:>9.3f}"
            f"{s['config_x_tarea'] / tot:>15.3f}{ratio_s:>10.2f}x"
        )
        sensitivity.append(
            {
                "min_n": min_n,
                "tasks": len(keep),
                "eta2_modelo": float(s["modelo"] / tot),
                "eta2_config": float(s["config_harness"] / tot),
                "eta2_config_x_tarea": float(s["config_x_tarea"] / tot),
                "ratio_config_modelo": float(ratio_s),
            }
        )

    out = {
        "schema": "variance-decomposition/1",
        "sensitivity": sensitivity,
        "source": "sweeps archivados en docs/, sin correr modelos",
        "suite_fingerprint": SUITE_FP,
        "files": sorted(files),
        "commits": sorted(commits),
        "design": {"models": MODELS, "configs": CONFIGS, "tasks": tasks},
        "runs_used": len(rows),
        "cell_pass_rate": {
            MODELS[i]: {CONFIGS[j]: float(matrix[i, j].mean()) for j in range(len(CONFIGS))}
            for i in range(len(MODELS))
        },
        "sum_of_squares": {k: float(v) for k, v in ss.items()},
        "eta_squared": {
            k: float(ss[k] / ss["ss_total"])
            for k in (
                "tarea",
                "modelo",
                "config_harness",
                "modelo_x_config",
                "modelo_x_tarea",
                "config_x_tarea",
                "residuo",
            )
        },
        "eta_squared_excluding_task": {
            k: float(ss[k] / (ss["ss_total"] - ss["tarea"]))
            for k in (
                "modelo",
                "config_harness",
                "modelo_x_config",
                "modelo_x_tarea",
                "config_x_tarea",
                "residuo",
            )
        },
    }
    path = DOCS / "variance-decomposition-2026-08-30.json"
    path.write_text(json.dumps(out, indent=1, ensure_ascii=False))
    print(f"\nJSON en {path}")


if __name__ == "__main__":
    main()
