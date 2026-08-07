#!/usr/bin/env python3
"""IRT (2PL) sobre las suites de braze-bench — técnica #2 del roadmap.

Pregunta: ¿qué ítems de una suite cargan información, y cuántos bastan?
`discriminating.toml` se construyó a mano con la heurística "34 ítems =
2.9pp por ítem"; IRT lo formaliza — dificultad (b) y discriminación (a)
por tarea, habilidad (theta) por respondente, estimadas de las corridas
históricas ya committeadas.

Modelo: 2PL, P(correcto | theta) = 1 / (1 + exp(-a_i (theta_j - b_i))).
Estimación: **máxima verosimilitud marginal (MML)** con cuadratura de
Gauss-Hermite sobre theta ~ N(0,1), y habilidades por EAP.

Por qué MML y no JML: el primer intento (2026-08-07) usó JML con ridge
suave y DEGENERÓ — `a` quedó pegado al tope superior en 18 de 19 ítems.
Es el problema de parámetros incidentales: con theta libre por
respondente y solo 19 ítems, cada theta se estima con 19 observaciones y
el sesgo se traslada a los parámetros de ítem. MML integra theta sobre
su prior en vez de estimarlo, que es la solución estándar y la que hace
interpretables los `a`.

Selección de subconjunto: información de Fisher del ítem,
I_i(theta) = a_i^2 P (1-P), integrada sobre el rango de habilidad donde
viven los modelos reales del proyecto (no sobre una normal teórica —
usamos los theta estimados). Greedy sobre información acumulada.

Validación (lo que decide si sirve): ¿el subconjunto reproduce el
RANKING de brazos que la suite completa produjo en cada sweep histórico?
Correlación de Spearman por sweep entre pass-rate-completo y
pass-rate-subset.
"""

import json
import sys
from collections import defaultdict
from pathlib import Path

import numpy as np
from scipy.optimize import minimize
from scipy.stats import spearmanr

RIDGE_A = 1e-2
RIDGE_B = 1e-2


def collect(root: Path, suite_filter: str):
    """Devuelve (matriz, items, respondentes, por_sweep)."""
    cells = {}
    by_sweep = defaultdict(list)
    for p in sorted(root.rglob("*.json")):
        try:
            d = json.loads(p.read_text())
        except Exception:
            continue
        runs = d.get("results") if isinstance(d, dict) else (d if isinstance(d, list) else None)
        if not runs or not isinstance(runs[0], dict) or "task_id" not in runs[0]:
            continue
        suite = ""
        if isinstance(d, dict):
            suite = (d.get("metadata", {}) or {}).get("suite_path", "").split("/")[-1]
        if suite != suite_filter:
            continue
        for r in runs:
            resp = (p.name, r.get("backend"), r.get("repetition"))
            cells[(resp, r["task_id"])] = bool(r.get("passed"))
            by_sweep[p.name].append((r.get("backend"), r["task_id"], bool(r.get("passed"))))
    items = sorted({t for (_, t) in cells})
    resps = sorted({r for (r, _) in cells})
    ii = {t: i for i, t in enumerate(items)}
    jj = {r: j for j, r in enumerate(resps)}
    X = np.full((len(resps), len(items)), np.nan)
    for (r, t), v in cells.items():
        X[jj[r], ii[t]] = 1.0 if v else 0.0
    return X, items, resps, by_sweep


def fit_2pl(X, n_nodes=41):
    """MML por cuadratura Gauss-Hermite. Devuelve (theta_EAP, a, b)."""
    mask = ~np.isnan(X)
    Y = np.nan_to_num(X)
    n_r, n_i = X.shape
    nodes, weights = np.polynomial.hermite_e.hermegauss(n_nodes)
    weights = weights / weights.sum()          # N(0,1) discretizada

    def log_lik_matrix(a, b):
        """(n_r, n_nodes): log P(patrón del respondente | theta_k)."""
        z = np.clip(a[None, :] * (nodes[:, None] - b[None, :]), -30, 30)  # (K, I)
        logp = -np.logaddexp(0.0, -z)
        logq = -np.logaddexp(0.0, z)
        # (R, K) = sum_i [ y_ri logp_ki + (1-y_ri) logq_ki ] sobre i observados
        return (Y * mask) @ logp.T + ((1 - Y) * mask) @ logq.T

    def neg_marginal_ll(params):
        a = params[:n_i]
        b = params[n_i:]
        ll = log_lik_matrix(a, b) + np.log(weights)[None, :]
        m = ll.max(axis=1, keepdims=True)
        return -float((m.ravel() + np.log(np.exp(ll - m).sum(axis=1))).sum())

    x0 = np.concatenate([np.ones(n_i), np.zeros(n_i)])
    res = minimize(
        neg_marginal_ll, x0, method="L-BFGS-B",
        bounds=[(0.05, 6.0)] * n_i + [(-6.0, 6.0)] * n_i,
    )
    a, b = res.x[:n_i], res.x[n_i:]
    # Habilidades EAP (esperanza a posteriori) para integrar la información
    ll = log_lik_matrix(a, b) + np.log(weights)[None, :]
    m = ll.max(axis=1, keepdims=True)
    post = np.exp(ll - m)
    post /= post.sum(axis=1, keepdims=True)
    theta = post @ nodes
    return theta, a, b


def item_information(a, b, thetas):
    """Información de Fisher integrada sobre los theta observados."""
    z = np.clip(a[None, :] * (thetas[:, None] - b[None, :]), -30, 30)
    p = 1.0 / (1.0 + np.exp(-z))
    return (a[None, :] ** 2 * p * (1 - p)).mean(axis=0)


def main():
    root = Path(sys.argv[1] if len(sys.argv) > 1 else "docs")
    suite = sys.argv[2] if len(sys.argv) > 2 else "default.toml"
    X, items, resps, by_sweep = collect(root, suite)
    print(f"suite={suite}  respondentes={X.shape[0]}  items={X.shape[1]}  "
          f"celdas={int((~np.isnan(X)).sum())}")
    pr = np.nanmean(X, axis=0)
    degen = [(items[i], pr[i]) for i in range(len(items)) if pr[i] in (0.0, 1.0)]
    print(f"items degenerados (0% o 100% en TODO respondente): {len(degen)}")
    for t, v in degen:
        print(f"   {t:38s} {v:.0%}")

    theta, a, b = fit_2pl(X)
    info = item_information(a, b, theta)
    order = np.argsort(-info)

    print(f"\n{'item':38s} {'p_obs':>6s} {'a':>6s} {'b':>7s} {'info':>7s}")
    for i in order:
        print(f"{items[i]:38s} {pr[i]:6.2f} {a[i]:6.2f} {b[i]:7.2f} {info[i]:7.4f}")

    total = info.sum()
    cum = np.cumsum(info[order]) / total
    for frac in (0.80, 0.90, 0.95):
        k = int(np.searchsorted(cum, frac) + 1)
        print(f"\n{frac:.0%} de la información: {k}/{len(items)} items")

    # --- validación: ¿el subset reproduce el ranking de brazos por sweep? ---
    print("\n--- validación: ranking de brazos, suite completa vs subset ---")
    for k in (6, 8, 10, 12):
        keep = {items[i] for i in order[:k]}
        rhos, used = [], 0
        for sweep, rows in by_sweep.items():
            full, sub = defaultdict(list), defaultdict(list)
            for backend, task, ok in rows:
                full[backend].append(ok)
                if task in keep:
                    sub[backend].append(ok)
            arms = [x for x in full if len(full[x]) and len(sub.get(x, []))]
            if len(arms) < 3:
                continue
            f = [np.mean(full[x]) for x in arms]
            s = [np.mean(sub[x]) for x in arms]
            if len(set(f)) < 2 or len(set(s)) < 2:
                continue
            rho = spearmanr(f, s).statistic
            if not np.isnan(rho):
                rhos.append(rho)
                used += 1
        if rhos:
            print(f"  k={k:2d}: sweeps={used:3d}  Spearman medio={np.mean(rhos):.3f}  "
                  f"mediana={np.median(rhos):.3f}  ρ=1.0 en {sum(1 for r in rhos if r > 0.999)}/{used}")


if __name__ == "__main__":
    main()
