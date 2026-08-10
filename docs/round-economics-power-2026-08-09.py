#!/usr/bin/env python3
"""Análisis de poder para el factorial de round-economics.

Pregunta que responde: ¿cuántas réplicas (y/o ítems) necesita el factorial
para que una interacción del tamaño observado en el piloto salga del ruido?

Método:
  1. Sanity check: reproducir el punto y el IC del piloto (+5,7 pp [+0,0,+10,2]).
  2. Modelo semi-paramétrico: probabilidad de pass por (tarea, brazo) estimada
     de las 3 réplicas. Simular experimentos futuros con R réplicas: draws
     binomiales por celda, interacción estimada, IC bootstrap pareado por
     tarea. Poder = fracción de simulaciones cuyo IC95% excluye 0.
  3. Escenarios de tamaño de efecto: el observado (+5,7), el observado sin
     las filas timeout (+4,8) y uno encogido (+3,0) — porque el punto del
     piloto ES ruidoso y el efecto real puede ser menor.
     El encogimiento se implementa escalando el componente de interacción
     de cada tarea hacia la media nula, preservando los efectos principales.
  4. Costo: s/celda medido del piloto -> horas de Nitro por escenario.
"""

import json
import random
import sys
from collections import defaultdict

PRECIO_BARATO = "gpu-layers=99"
CONFIG_DERROCHADORA = "ttc="

ARMS = [("caro", "avara"), ("caro", "derrochadora"),
        ("barato", "avara"), ("barato", "derrochadora")]


def factores(backend):
    precio = "barato" if PRECIO_BARATO in backend else "caro"
    config = "derrochadora" if CONFIG_DERROCHADORA in backend else "avara"
    return precio, config


def cargar(path):
    with open(path) as fh:
        sweep = json.load(fh)
    filas = sweep["results"]
    # celda[(precio,config)][tarea] = lista de bools (una por réplica)
    celda = defaultdict(lambda: defaultdict(list))
    total_ms = 0.0
    n_filas = 0
    for f in filas:
        k = factores(f["backend"])
        celda[k][f["task_id"]].append(bool(f["passed"]))
        total_ms += f["wall_time_ms"]
        n_filas += 1
    tareas = sorted({t for arm in celda.values() for t in arm})
    return celda, tareas, total_ms / n_filas / 1000.0, n_filas


def interaccion_por_tarea(celda, tareas):
    """Contribución de cada tarea al término de interacción (con sus reps)."""
    contrib = {}
    for t in tareas:
        def tasa(k):
            v = celda[k][t]
            return sum(v) / len(v) if v else float("nan")
        contrib[t] = ((tasa(("barato", "derrochadora")) - tasa(("barato", "avara")))
                      - (tasa(("caro", "derrochadora")) - tasa(("caro", "avara"))))
    return contrib


def ic_bootstrap(contrib_por_tarea, tareas, rng, n_boot=2000):
    punto = sum(contrib_por_tarea[t] for t in tareas) / len(tareas)
    reps = []
    for _ in range(n_boot):
        muestra = [rng.choice(tareas) for _ in tareas]
        reps.append(sum(contrib_por_tarea[t] for t in muestra) / len(muestra))
    reps.sort()
    lo = reps[int(0.025 * n_boot)]
    hi = reps[int(0.975 * n_boot) - 1]
    return punto, lo, hi


def probs_por_celda(celda, tareas):
    """p(pass) por (brazo, tarea) estimada de las réplicas observadas."""
    p = {}
    for k in ARMS:
        for t in tareas:
            v = celda[k][t]
            p[(k, t)] = sum(v) / len(v) if v else 0.0
    return p


def encoger_interaccion(p, tareas, factor):
    """Escala el componente de interacción hacia 0 por factor, preservando
    efectos principales y medias por tarea (descomposición ANOVA 2x2)."""
    q = {}
    for t in tareas:
        y = {k: p[(k, t)] for k in ARMS}
        mu = sum(y.values()) / 4
        a = {"caro": (y[("caro", "avara")] + y[("caro", "derrochadora")]) / 2 - mu,
             "barato": (y[("barato", "avara")] + y[("barato", "derrochadora")]) / 2 - mu}
        b = {"avara": (y[("caro", "avara")] + y[("barato", "avara")]) / 2 - mu,
             "derrochadora": (y[("caro", "derrochadora")] + y[("barato", "derrochadora")]) / 2 - mu}
        for k in ARMS:
            inter = y[k] - mu - a[k[0]] - b[k[1]]
            q[(k, t)] = min(1.0, max(0.0, mu + a[k[0]] + b[k[1]] + factor * inter))
    return q


def poder(p, tareas, n_reps, rng, n_sim=400, n_boot=800):
    """Simula n_sim experimentos con n_reps réplicas; poder = frac. de IC>0."""
    exitos = 0
    puntos = []
    for _ in range(n_sim):
        contrib = {}
        for t in tareas:
            def tasa_sim(k):
                pr = p[(k, t)]
                return sum(1 for _ in range(n_reps) if rng.random() < pr) / n_reps
            contrib[t] = ((tasa_sim(("barato", "derrochadora")) - tasa_sim(("barato", "avara")))
                          - (tasa_sim(("caro", "derrochadora")) - tasa_sim(("caro", "avara"))))
        punto, lo, hi = ic_bootstrap(contrib, tareas, rng, n_boot)
        puntos.append(punto)
        if lo > 0:
            exitos += 1
    return exitos / n_sim, sum(puntos) / len(puntos)


def main(path):
    rng = random.Random(20260809)
    celda, tareas, s_por_celda, n_filas = cargar(path)
    print(f"Filas: {n_filas}  tareas: {len(tareas)}  s/celda promedio: {s_por_celda:.1f}")

    # --- 1. Sanity check ---------------------------------------------------
    contrib = interaccion_por_tarea(celda, tareas)
    punto, lo, hi = ic_bootstrap(contrib, tareas, rng, n_boot=10_000)
    print(f"\n== 1. Sanity check vs piloto documentado ==")
    print(f"   interacción: {punto:+.1%}  IC95% [{lo:+.1%}, {hi:+.1%}]")
    print(f"   (documentado: +5,7 pp [+0,0, +10,2])")

    # --- 2. Poder por escenario --------------------------------------------
    p_obs = probs_por_celda(celda, tareas)
    escenarios = [
        ("efecto observado (+5,7 pp)", p_obs),
        ("efecto encogido 60% (~+3,4 pp)", encoger_interaccion(p_obs, tareas, 0.6)),
        ("efecto encogido 50% (~+2,9 pp)", encoger_interaccion(p_obs, tareas, 0.5)),
    ]
    reps_grid = [3, 5, 8, 10, 15, 20]

    print(f"\n== 2. Poder simulado (prob. de que el IC95% excluya 0) ==")
    print(f"   {'escenario':<34} " + " ".join(f"R={r:<4}" for r in reps_grid))
    filas_costo = []
    for nombre, p in escenarios:
        poderes = []
        for r in reps_grid:
            pw, media = poder(p, tareas, r, rng)
            poderes.append(pw)
        print(f"   {nombre:<34} " + " ".join(f"{pw:>5.0%}" for pw in poderes))

    # --- 3. Costo ----------------------------------------------------------
    print(f"\n== 3. Costo en Nitro (53 tareas x 4 brazos, {s_por_celda:.1f}s/celda) ==")
    for r in reps_grid:
        horas = len(tareas) * 4 * r * s_por_celda / 3600
        print(f"   R={r:<3} -> {len(tareas)*4*r} celdas ≈ {horas:.1f} h")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1]))


def poder_tareas(p, tareas, n_tareas, n_reps, rng, n_sim=400, n_boot=800):
    """Poder con n_tareas remuestreadas del pool (población de tareas
    intercambiables con el banco actual) y n_reps réplicas."""
    exitos = 0
    for _ in range(n_sim):
        pool = [rng.choice(tareas) for _ in range(n_tareas)]
        contrib = {}
        for i, t in enumerate(pool):
            def tasa_sim(k):
                pr = p[(k, t)]
                return sum(1 for _ in range(n_reps) if rng.random() < pr) / n_reps
            contrib[i] = ((tasa_sim(("barato", "derrochadora")) - tasa_sim(("barato", "avara")))
                          - (tasa_sim(("caro", "derrochadora")) - tasa_sim(("caro", "avara"))))
        ids = list(range(n_tareas))
        punto = sum(contrib.values()) / n_tareas
        reps_b = []
        for _ in range(n_boot):
            m = [rng.choice(ids) for _ in ids]
            reps_b.append(sum(contrib[i] for i in m) / n_tareas)
        reps_b.sort()
        if reps_b[int(0.025 * n_boot)] > 0:
            exitos += 1
    return exitos / n_sim


def main2(path):
    rng = random.Random(20260810)
    celda, tareas, s_por_celda, _ = cargar(path)
    p_obs = probs_por_celda(celda, tareas)
    p_60 = encoger_interaccion(p_obs, tareas, 0.6)
    print("== Poder vs número de TAREAS (R=3 réplicas) ==")
    print(f"   {'escenario':<30} " + " ".join(f"T={t:<5}" for t in [53, 80, 120, 200, 300]))
    for nombre, p in [("efecto observado (+5,7)", p_obs), ("encogido 60% (~+3,4)", p_60)]:
        fila = []
        for t in [53, 80, 120, 200, 300]:
            fila.append(poder_tareas(p, tareas, t, 3, rng))
        print(f"   {nombre:<30} " + " ".join(f"{pw:>5.0%}" for pw in fila))
    print("\n== Costo (4 brazos x 3 réplicas, 37,7 s/celda) ==")
    for t in [80, 120, 200, 300]:
        print(f"   T={t:<4} -> {t*12} celdas ≈ {t*12*s_por_celda/3600:.1f} h Nitro + AUTORAR {t-53} tareas nuevas")


if __name__ == "__main__2":
    pass
