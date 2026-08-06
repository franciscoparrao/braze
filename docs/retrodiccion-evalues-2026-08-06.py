#!/usr/bin/env python3
"""Retrodicción e-values/SPRT sobre los sweeps históricos de braze-bench.

Técnica #1 de docs/techniques-roadmap-2026-08-06.md. Pregunta: si cada A/B
histórico se hubiera monitoreado con inferencia anytime-valid, ¿cuándo se
habría podido parar, sin cambiar ninguna decisión?

Métodos (declarados en el roadmap ANTES de correr esto):
- E-process para pares discordantes de McNemar: bajo H0 cada par discordante
  favorece a un brazo con p=1/2. E-value con mixture Beta(1/2,1/2)
  (martingala exacta): E_n = 2^n * B(1/2+k, 1/2+n-k) / B(1/2, 1/2).
  Rechazo de H0 cuando E_n >= 20 (alfa=0.05, desigualdad de Ville).
  El e-process SOLO rechaza — no acepta H0.
- SPRT doble unilateral para poder ACEPTAR H0 temprano: p1=0.75 (razón de
  discordantes 3:1, el orden de efecto que los criterios pre-registrados del
  proyecto tratan como señal), alfa=0.05, beta=0.20.
  Umbral H1: LR >= (1-beta)/alfa = 16; umbral H0: LR <= beta/(1-alfa) ~ 0.211.
  "null temprano" = AMBOS SPRTs unilaterales cruzan su cota inferior.

Supuestos (honestos, van al reporte):
- Orden de llegada de los pares = orden del array `results` del primer brazo
  (el orden de ejecución real de la suite). Para sweeps corridos brazo-por-
  brazo secuencialmente, el ahorro real aplicaría al segundo brazo en
  adelante; el ahorro reportado asume ejecución pareada/entrelazada, así que
  es una COTA SUPERIOR del ahorro operativo y se declara como tal.
- Solo pares con >= 20 celdas pareadas y >= 1 discordante entran al análisis.
"""

import json
import math
import sys
from pathlib import Path
from itertools import combinations

ALPHA_E = 20.0          # Ville: 1/alfa
P1 = 0.75               # efecto SPRT declarado
SPRT_H1 = (1 - 0.20) / 0.05   # 16
SPRT_H0 = 0.20 / (1 - 0.05)   # ~0.2105


def load_runs(path: Path):
    try:
        d = json.loads(path.read_text())
    except Exception:
        return None, None
    if isinstance(d, dict) and "results" in d:
        return d["results"], d.get("metadata", {})
    if isinstance(d, list) and d and isinstance(d[0], dict) and "task_id" in d[0]:
        return d, {}
    return None, None


def log_beta(a, b):
    return math.lgamma(a) + math.lgamma(b) - math.lgamma(a + b)


def e_value(k, n):
    """Mixture Beta(1/2,1/2) e-value para n discordantes con k a favor de B."""
    return math.exp(n * math.log(2) + log_beta(0.5 + k, 0.5 + n - k) - log_beta(0.5, 0.5))


def analyze_pair(runs_a, runs_b, wall_by_pair):
    """Stream de discordantes en orden de ejecución; devuelve dict resumen."""
    keys_order = []
    seen = set()
    for r in runs_a:
        key = (r["task_id"], r.get("repetition", 0))
        if key not in seen:
            seen.add(key)
            keys_order.append(key)
    a = {(r["task_id"], r.get("repetition", 0)): bool(r.get("passed")) for r in runs_a}
    b = {(r["task_id"], r.get("repetition", 0)): bool(r.get("passed")) for r in runs_b}
    common = [k for k in keys_order if k in b]
    if len(common) < 20:
        return None

    n = k_b = 0
    e_stop = None            # índice de par (1-based sobre common) al rechazar
    lr_hi = lr_lo = 1.0      # SPRTs unilaterales (favor B / favor A)
    sprt_stop = None
    sprt_verdict = None
    hi_dead = lo_dead = False
    e_max = 1.0

    for i, key in enumerate(common, 1):
        pa, pb = a[key], b[key]
        if pa == pb:
            continue
        n += 1
        x = 1 if (pb and not pa) else 0   # discordante a favor de B
        k_b += x
        ev = e_value(k_b, n)
        e_max = max(e_max, ev)
        if e_stop is None and ev >= ALPHA_E:
            e_stop = i
        # SPRT favor-B: H1 p=0.75 sobre x; favor-A: H1 p=0.75 sobre (1-x)
        lr_hi *= (P1 if x else (1 - P1)) / 0.5
        lr_lo *= ((1 - P1) if x else P1) / 0.5
        if sprt_stop is None:
            if lr_hi >= SPRT_H1:
                sprt_stop, sprt_verdict = i, "efecto(B)"
            elif lr_lo >= SPRT_H1:
                sprt_stop, sprt_verdict = i, "efecto(A)"
            else:
                if lr_hi <= SPRT_H0:
                    hi_dead = True
                if lr_lo <= SPRT_H0:
                    lo_dead = True
                if hi_dead and lo_dead:
                    sprt_stop, sprt_verdict = i, "null"

    if n == 0:
        return None

    # McNemar exacto de n fijo (dos colas) para el veredicto histórico
    def binom_two_sided(k, n):
        pmf = [math.comb(n, i) * 0.5 ** n for i in range(n + 1)]
        return min(1.0, sum(p for p in pmf if p <= pmf[k] + 1e-12))

    p_final = binom_two_sided(k_b, n)
    total = len(common)
    saved_pairs = (total - e_stop) if e_stop else 0
    saved_pairs_sprt = (total - sprt_stop) if sprt_stop else 0
    mean_wall = (
        sum(wall_by_pair.get(k, 0) for k in common) / total if total else 0
    )
    return {
        "pares": total, "discordantes": n, "favor_B": k_b,
        "p_mcnemar_final": p_final, "e_max": e_max,
        "e_rechazo_en_par": e_stop,
        "sprt_stop_en_par": sprt_stop, "sprt_veredicto": sprt_verdict,
        "pct_ahorro_e": 100 * saved_pairs / total,
        "pct_ahorro_sprt": 100 * saved_pairs_sprt / total,
        "horas_ahorro_sprt": saved_pairs_sprt * mean_wall / 3600.0,
    }


def main():
    root = Path(sys.argv[1] if len(sys.argv) > 1 else "docs")
    rows = []
    for path in sorted(root.rglob("*.json")):
        runs, _meta = load_runs(path)
        if not runs:
            continue
        by_arm = {}
        for r in runs:
            if not isinstance(r, dict) or "backend" not in r or "task_id" not in r:
                continue
            by_arm.setdefault(r["backend"], []).append(r)
        if len(by_arm) < 2:
            continue
        wall = {}
        for r in runs:
            key = (r["task_id"], r.get("repetition", 0))
            # promedio del par (dos brazos) — aproximación para el ahorro
            wall[key] = wall.get(key, 0) * 0.5 + (r.get("wall_time_ms", 0) or 0) / 1000.0
        for arm_a, arm_b in combinations(sorted(by_arm), 2):
            res = analyze_pair(by_arm[arm_a], by_arm[arm_b], wall)
            if res:
                rows.append((path.name, arm_a, arm_b, res))

    print(f"{'sweep':52s} {'pares':>5s} {'disc':>4s} {'p_fin':>7s} "
          f"{'e_stop':>6s} {'sprt':>10s} {'ahorro%':>7s} {'~horas':>6s}")
    for name, a, b, r in rows:
        short = name[:50]
        sprt = f"{r['sprt_veredicto'] or '-'}@{r['sprt_stop_en_par'] or '-'}"
        print(f"{short:52s} {r['pares']:5d} {r['discordantes']:4d} "
              f"{r['p_mcnemar_final']:7.3f} {str(r['e_rechazo_en_par'] or '-'):>6s} "
              f"{sprt:>10s} {r['pct_ahorro_sprt']:7.1f} {r['horas_ahorro_sprt']:6.1f}")
    if rows:
        import statistics as st
        ahorros = [r["pct_ahorro_sprt"] for _, _, _, r in rows]
        horas = sum(r["horas_ahorro_sprt"] for _, _, _, r in rows)
        print(f"\ncontrastes analizados: {len(rows)}")
        print(f"ahorro SPRT mediano: {st.median(ahorros):.1f}%  |  total ~{horas:.1f} h")
        decid = [r for _, _, _, r in rows if r["sprt_stop_en_par"]]
        print(f"contrastes que el SPRT decide antes del final: {len(decid)}/{len(rows)}")


if __name__ == "__main__":
    main()
