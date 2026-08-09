#!/usr/bin/env python3
"""Análisis del factorial de round-economics sobre un JSON de braze-bench.

Lee un sweep con cuatro brazos (precio de ronda × configuración de harness) y
reporta, en este orden:

  1. El PISO DE RUIDO del régimen — discordancia entre réplicas idénticas
     dentro de cada brazo. Va primero a propósito: bajo presupuesto de
     wall-clock el tiempo es lo que binariza el pass/fail, y el walltime ya
     está medido como ±30% entre corridas idénticas en modelos chicos
     (docs/noise-floor-2026-07-26.md). Sin este número, el término de
     interacción de abajo no se puede leer.
  2. Salud del contraste — filas [Timeout] (backstop de infraestructura, con
     rondas/tokens censurados) contra filas [WallClock] (el corte
     experimental). Si hay Timeouts, el sweep midió el backstop.
  3. Descriptivos por brazo.
  4. El término de interacción con bootstrap pareado por tarea.

Uso:  python3 scripts/round_economics_analysis.py sweep.json
"""

import json
import random
import sys
from collections import defaultdict

# El brazo se identifica por su nombre de display, que lleva el sufijo
# `+ablate:` completo — de ahí salen los dos factores sin adivinar.
PRECIO_BARATO = "gpu-layers=99"
CONFIG_DERROCHADORA = "ttc="


def factores(backend: str) -> tuple[str, str]:
    precio = "barato" if PRECIO_BARATO in backend else "caro"
    config = "derrochadora" if CONFIG_DERROCHADORA in backend else "avara"
    return precio, config


def main(path: str) -> int:
    with open(path) as fh:
        sweep = json.load(fh)
    meta = sweep.get("metadata", {})
    filas = sweep["results"]

    presupuesto = meta.get("turn_wall_clock_secs")
    print(f"Sweep: {path}")
    print(f"  suite            : {meta.get('suite_path')}")
    print(f"  presupuesto turno: {presupuesto}s" if presupuesto is not None
          else "  presupuesto turno: (sin presupuesto)")
    print(f"  backstop infra   : {meta.get('task_timeout_secs')}s")
    print(f"  repeticiones     : {meta.get('repetitions')}")
    if presupuesto is None:
        print("  AVISO: sweep SIN presupuesto de wall-clock — no es una celda "
              "de round-economics, los descriptivos valen pero la interacción no.")

    por_brazo: dict[str, dict[tuple[str, int], bool]] = defaultdict(dict)
    causas: dict[str, dict[str, int]] = defaultdict(lambda: defaultdict(int))
    tiempos: dict[str, list[float]] = defaultdict(list)
    rondas: dict[str, list[int]] = defaultdict(list)
    for f in filas:
        brazo = f["backend"]
        por_brazo[brazo][(f["task_id"], f["repetition"])] = bool(f["passed"])
        if f.get("failure_cause"):
            causas[brazo][f["failure_cause"]] += 1
        tiempos[brazo].append(f["wall_time_ms"] / 1000.0)
        rondas[brazo].append(f["rounds"])

    brazos = list(por_brazo)

    # --- 1. Piso de ruido del régimen -------------------------------------
    print("\n== 1. Piso de ruido: discordancia entre réplicas idénticas ==")
    print("   (mismo brazo, misma tarea, distinta repetición — el seed cambia,")
    print("    todo lo demás es idéntico. Es ruido por construcción.)")
    for brazo in brazos:
        celdas = por_brazo[brazo]
        tareas = sorted({t for t, _ in celdas})
        reps = sorted({r for _, r in celdas})
        discordantes = 0
        inestables = 0
        for t in tareas:
            vals = [celdas.get((t, r)) for r in reps]
            vals = [v for v in vals if v is not None]
            if len(set(vals)) > 1:
                inestables += 1
                discordantes += sum(1 for v in vals if v != vals[0])
        print(f"   {brazo}")
        print(f"      tareas inestables: {inestables}/{len(tareas)}  "
              f"celdas discordantes: {discordantes}")

    # --- 2. Salud del contraste -------------------------------------------
    print("\n== 2. Salud del contraste ==")
    total_timeout = sum(c.get("timeout", 0) for c in causas.values())
    total_wallclock = sum(c.get("wall_clock_exhausted", 0) for c in causas.values())
    print(f"   filas [WallClock] (corte experimental, contabilidad intacta): {total_wallclock}")
    print(f"   filas [Timeout]   (backstop de infra, rondas/tokens censurados): {total_timeout}")
    if total_timeout:
        print("   ATENCIÓN: hay filas censuradas — el backstop mordió antes que el")
        print("   presupuesto. Subir --task-timeout-secs y re-correr antes de interpretar.")
    if presupuesto is not None and total_wallclock == 0:
        print("   ATENCIÓN: NINGUNA fila tocó el presupuesto — el presupuesto no")
        print("   está mordiendo, así que el factorial no manipuló nada. Bajarlo.")

    # --- 3. Descriptivos ---------------------------------------------------
    print("\n== 3. Por brazo ==")
    print(f"   {'brazo':<62} {'pass':>8} {'rondas':>7} {'seg':>7}")
    for brazo in brazos:
        celdas = por_brazo[brazo]
        pr = sum(celdas.values()) / len(celdas)
        print(f"   {brazo:<62} {pr:>7.1%} "
              f"{sum(rondas[brazo]) / len(rondas[brazo]):>7.1f} "
              f"{sum(tiempos[brazo]) / len(tiempos[brazo]):>7.1f}")
        if causas[brazo]:
            detalle = ", ".join(f"{k}={v}" for k, v in sorted(causas[brazo].items()))
            print(f"      causas: {detalle}")

    # --- 4. Interacción ----------------------------------------------------
    celda = {}
    for brazo in brazos:
        celda[factores(brazo)] = por_brazo[brazo]
    faltan = [k for k in [("caro", "avara"), ("caro", "derrochadora"),
                          ("barato", "avara"), ("barato", "derrochadora")]
              if k not in celda]
    if faltan:
        print(f"\n== 4. Interacción: NO calculable, faltan brazos: {faltan} ==")
        return 0

    tareas = sorted({t for cel in celda.values() for t, _ in cel})

    def tasa(key, subset):
        cel = celda[key]
        vals = [v for (t, _), v in cel.items() if t in subset]
        return sum(vals) / len(vals) if vals else float("nan")

    def interaccion(subset):
        return ((tasa(("barato", "derrochadora"), subset) - tasa(("barato", "avara"), subset))
                - (tasa(("caro", "derrochadora"), subset) - tasa(("caro", "avara"), subset)))

    punto = interaccion(set(tareas))
    # Bootstrap pareado POR TAREA: la tarea es la unidad que se comparte
    # entre los cuatro brazos, así que remuestrear tareas respeta el pareo.
    rng = random.Random(20260808)
    reps_boot = []
    for _ in range(10_000):
        muestra = [rng.choice(tareas) for _ in tareas]
        reps_boot.append(interaccion(set(muestra)))
    reps_boot.sort()
    lo, hi = reps_boot[250], reps_boot[9750]

    print("\n== 4. Término de interacción (precio × configuración) ==")
    print("   (derrochadora−avara)|barato  −  (derrochadora−avara)|caro")
    for p in ("caro", "barato"):
        d = tasa((p, "derrochadora"), set(tareas)) - tasa((p, "avara"), set(tareas))
        print(f"   efecto de derrochar a precio {p:<7}: {d:+.1%}")
    print(f"   INTERACCIÓN: {punto:+.1%}   IC95% bootstrap pareado [{lo:+.1%}, {hi:+.1%}]")
    print("\n   Leer contra el piso de ruido de la sección 1, no contra cero:")
    print("   una interacción es MÁS CHICA que los efectos principales que la")
    print("   componen, y este régimen binariza ruido de reloj.")
    return 0


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print(__doc__)
        sys.exit(2)
    sys.exit(main(sys.argv[1]))
