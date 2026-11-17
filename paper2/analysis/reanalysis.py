#!/usr/bin/env python3
"""Reanálisis del Paper 2 tras el review simulado EMSE PROMPT-SE (2026-11-06).

Regenera desde los JSON commiteados en docs/ cada estadístico que el review
cuestionó, y separa lo que se arregla con los datos existentes de lo que pide
corridas nuevas. Plan: paper2/revision-plan-emse-prompt-se-2026-11-06.md.

Uso (desde la raíz del repo):
    python3 paper2/analysis/reanalysis.py > docs/paper2-reanalysis-2026-11-06.md
    python3 paper2/analysis/reanalysis.py --fig1-csv paper2/analysis/fig1-points.csv
      (solo escribe los puntos de la figura 1, que lee paper2/R/fig1_frontera.R)

Sin estado ni red: solo lee docs/. Deterministico.
"""
import glob
import itertools
import json
import statistics as st
from collections import defaultdict

import numpy as np
from scipy import stats

STUDY1 = sorted(glob.glob("docs/sweep-memory-distillation-r20-moveB-batch*-2026-07-17.json"))
REPLICATION = sorted(glob.glob("docs/sweep-m1-ornith9b-batch*-2026-08-15.json"))
STUDY2 = {a: f"docs/pm-ab/sweep-pm-ab-{a}-2026-08-04.json" for a in ("baseline", "empty", "seeded")}
NOISE_FLOOR = "docs/sweep-noise-floor-default-2026-09-12.json"
TASKS = [("original", "rust_borrow_fix"), ("loop", "rust_borrow_fix_loop"), ("move", "rust_move_fix")]


def load(paths):
    out = []
    for p in paths:
        out.extend(json.load(open(p))["results"])
    return out


def holm(pvals):
    """Holm-Bonferroni ajustado, mismo orden que la entrada."""
    order = sorted(range(len(pvals)), key=lambda i: pvals[i])
    adj, running = [0.0] * len(pvals), 0.0
    for rank, i in enumerate(order):
        running = max(running, min(1.0, (len(pvals) - rank) * pvals[i]))
        adj[i] = running
    return adj


def fisher(a_pass, a_n, b_pass, b_n):
    return stats.fisher_exact([[a_pass, a_n - a_pass], [b_pass, b_n - b_pass]])[1]


def mcnemar(b, c):
    return stats.binomtest(min(b, c), b + c, 0.5).pvalue if b + c else 1.0


def welch(x, y):
    """Diferencia de medias y - x, IC 95 % de Welch y p."""
    t = stats.ttest_ind(y, x, equal_var=False)
    ci = t.confidence_interval(0.95)
    return st.mean(y) - st.mean(x), (ci.low, ci.high), t.pvalue


def correct(r, use_cargo):
    """Corrección funcional: archivo esperado + (cargo check si existe) + sin timeout.
    Ignora a propósito los topes de tokens y rondas (se reportan aparte)."""
    if r.get("failure_cause") == "timeout" or r.get("run_error"):
        return False
    ok = bool(r.get("expected_files_found"))
    if use_cargo and r.get("expected_cargo_check_passed") is not None:
        ok = ok and bool(r["expected_cargo_check_passed"])
    return ok


def arms(rows, base):
    none = [r for r in rows if r["task_id"] == f"{base}_none"]
    pb = [r for r in rows if r["task_id"] == f"{base}_human_playbook"]
    return none, pb


def per_round(runs):
    """Tokens de entrada por ronda como razón de medias (total tokens / total rondas).
    Con este estimador la Ec. (1) es exacta: ΔR·T_base − ΔT·R_mem = −(Δ neto)."""
    return sum(r["input_tokens"] for r in runs) / sum(r["rounds"] for r in runs)


def boot_dT(n0, n1, reps=10000, seed=0):
    """IC 95 % percentil de ΔT (razón de medias), remuestreando corridas dentro de cada brazo."""
    rng = np.random.default_rng(seed)
    t0 = np.array([[r["input_tokens"], r["rounds"]] for r in n0], float)
    t1 = np.array([[r["input_tokens"], r["rounds"]] for r in n1], float)
    out = []
    for _ in range(reps):
        a = t0[rng.integers(0, len(t0), len(t0))].sum(0)
        b = t1[rng.integers(0, len(t1), len(t1))].sum(0)
        out.append(b[0] / b[1] - a[0] / a[1])
    return np.percentile(out, [2.5, 97.5])


def wilson(x, n, z=1.959963984540054):
    p = x / n
    c = (p + z * z / (2 * n)) / (1 + z * z / n)
    h = z * ((p * (1 - p) / n + z * z / (4 * n * n)) ** 0.5) / (1 + z * z / n)
    return c - h, c + h


def newcombe(x1, n1, x2, n2):
    """Diferencia p2 − p1 con IC 95 % de Newcombe (híbrido de Wilson, método 10)."""
    p1, p2 = x1 / n1, x2 / n2
    l1, u1 = wilson(x1, n1)
    l2, u2 = wilson(x2, n2)
    d = p2 - p1
    return d, d - ((p2 - l2) ** 2 + (u1 - p1) ** 2) ** 0.5, d + ((u2 - p2) ** 2 + (p1 - l1) ** 2) ** 0.5


def section_pass(title, rows, use_cargo):
    print(f"\n### {title}\n")
    print("| Tarea | Pass oficial (none→pb) | Δ Newcombe 95 % | p Fisher | Holm | Corrección funcional (none→pb) | Δ Newcombe 95 % | p Fisher | Holm | Fallas por tope de tokens (none→pb) | Fallas por tope de rondas (none→pb) | Timeouts (none→pb) | Errores de backend (none→pb) |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    p_off, p_cor, lines = [], [], []
    for name, base in TASKS:
        n0, n1 = arms(rows, base)
        a, b = sum(r["passed"] for r in n0), sum(r["passed"] for r in n1)
        c0, c1 = sum(correct(r, use_cargo) for r in n0), sum(correct(r, use_cargo) for r in n1)
        tok = (sum(r.get("expected_tokens_within_budget") is False for r in n0),
               sum(r.get("expected_tokens_within_budget") is False for r in n1))
        rnd = (sum(r.get("expected_rounds_within_budget") is False for r in n0),
               sum(r.get("expected_rounds_within_budget") is False for r in n1))
        to = (sum(r.get("failure_cause") == "timeout" for r in n0),
              sum(r.get("failure_cause") == "timeout" for r in n1))
        be = (sum(bool(r.get("run_error")) for r in n0), sum(bool(r.get("run_error")) for r in n1))
        p_off.append(fisher(a, len(n0), b, len(n1)))
        p_cor.append(fisher(c0, len(n0), c1, len(n1)))
        nw = "{:+.2f} [{:+.2f}, {:+.2f}]"
        lines.append((name, f"{a}/{len(n0)}→{b}/{len(n1)}", nw.format(*newcombe(a, len(n0), b, len(n1))),
                      f"{c0}/{len(n0)}→{c1}/{len(n1)}", nw.format(*newcombe(c0, len(n0), c1, len(n1))), tok, rnd, to, be))
    for (name, off, noff, cor, ncor, tok, rnd, to, be), po, ho, pc, hc in zip(lines, p_off, holm(p_off), p_cor, holm(p_cor)):
        print(f"| {name} | {off} | {noff} | {po:.3f} | {ho:.3f} | {cor} | {ncor} | {pc:.3f} | {hc:.3f} | {tok[0]}→{tok[1]} | {rnd[0]}→{rnd[1]} | {to[0]}→{to[1]} | {be[0]}→{be[1]} |")


def section_economics(title, rows):
    print(f"\n### {title}\n")
    print("| Tarea | ΔR (IC 95 %) | p Holm ΔR | T_base | T_mem | ΔT (IC 95 % bootstrap) | memory_tokens | R_mem | ΔR* = ΔT·R_mem/T_base | Net Δtokens (IC 95 %) | p Holm net | Identidad ΔT·R_mem − ΔR·T_base | Wall s (none→pb) |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    res = []
    for name, base in TASKS:
        n0, n1 = arms(rows, base)
        r0, r1 = [r["rounds"] for r in n0], [r["rounds"] for r in n1]
        dR, ciR, pR = welch(r1, r0)  # rondas ahorradas = base - mem
        t0, t1 = per_round(n0), per_round(n1)
        dT = t1 - t0
        ciT = boot_dT(n0, n1)
        mem = st.mean(r.get("memory_tokens") or 0 for r in n1)
        Rm = st.mean(r1)
        be = dT * Rm / t0
        dN, ciN, pN = welch([r["input_tokens"] for r in n0], [r["input_tokens"] for r in n1])
        ident = dT * Rm - dR * t0
        w = (st.mean(r["wall_time_ms"] for r in n0) / 1000, st.mean(r["wall_time_ms"] for r in n1) / 1000)
        res.append((name, dR, ciR, pR, t0, t1, dT, ciT, mem, Rm, be, dN, ciN, pN, ident, w))
    hR, hN = holm([x[3] for x in res]), holm([x[13] for x in res])
    for (name, dR, ciR, pR, t0, t1, dT, ciT, mem, Rm, be, dN, ciN, pN, ident, w), hr, hn in zip(res, hR, hN):
        print(f"| {name} | {dR:+.2f} ({ciR[0]:+.2f}, {ciR[1]:+.2f}) | {hr:.3f} | {t0:.0f} | {t1:.0f} | {dT:+.0f} ({ciT[0]:+.0f}, {ciT[1]:+.0f}) | {mem:.0f} | {Rm:.2f} | {be:.2f} | {dN:+.0f} ({ciN[0]:+.0f}, {ciN[1]:+.0f}) | {hn:.3f} | {ident:+.0f} | {w[0]:.1f}→{w[1]:.1f} |")
    print("\nNotas: ΔR > 0 = el playbook ahorra rondas. T = razón de medias (total de tokens de entrada / total de rondas del brazo); con ese estimador la columna «Identidad» reproduce el Δ neto exacto (Ec. 1). La versión del 2026-11-06 usaba la media de las razones por corrida, con la que la identidad no cierra (error de 2 a 200 tokens). `memory_tokens` es la estimación del bench (caracteres / 4, `crates/braze-bench/src/memory.rs::estimate_tokens`), no un conteo del tokenizador: ΔT no separa inyección de cambio de trayectoria. IC de ΔT: bootstrap percentil, 10.000 réplicas, semilla 0, remuestreo de corridas dentro de cada brazo.")


def section_unaided(rows):
    print("\n### Dificultad sin ayuda (brazos `none` del Estudio 1, gpt-oss:20b)\n")
    print("| Tarea | Pass | Rondas medias | Timeouts |")
    print("|---|---|---|---|")
    for name, base in TASKS:
        n0, _ = arms(rows, base)
        print(f"| {name} | {sum(r['passed'] for r in n0)}/{len(n0)} | {st.mean(r['rounds'] for r in n0):.2f} | {sum(r.get('failure_cause') == 'timeout' for r in n0)} |")


def section_tool_calls(rows):
    """Proxies de mecanismo de §6.1, definidos de forma explícita y reproducible."""
    print("\n### Llamadas a herramientas en las tareas frescas (Estudio 1)\n")
    print("Definición: llamadas antes del primer `edit_file` (sin contar el edit); corridas sin `edit_file` excluidas de esa columna.\n")
    print("| Tarea | Corridas con edit (none→pb) | p Fisher | Llamadas antes del primer edit (none→pb) | p Mann-Whitney | Holm | Llamadas totales (none→pb) | p Mann-Whitney | Holm |")
    print("|---|---|---|---|---|---|---|---|---|")
    out = []
    for name, base in TASKS[1:]:
        n0, n1 = arms(rows, base)
        def before(r):
            names = r.get("tool_call_names") or []
            return names.index("edit_file") if "edit_file" in names else None
        e0, e1 = [r for r in n0 if before(r) is not None], [r for r in n1 if before(r) is not None]
        b0, b1 = [before(r) for r in e0], [before(r) for r in e1]
        t0, t1 = [r["tool_calls_total"] for r in n0], [r["tool_calls_total"] for r in n1]
        pf = fisher(len(e0), len(n0), len(e1), len(n1))
        pb = stats.mannwhitneyu(b0, b1, alternative="two-sided").pvalue
        pt = stats.mannwhitneyu(t0, t1, alternative="two-sided").pvalue
        out.append((name, len(e0), len(n0), len(e1), len(n1), pf, b0, b1, pb, t0, t1, pt))
    hb, ht = holm([o[8] for o in out]), holm([o[11] for o in out])
    for (name, le0, ln0, le1, ln1, pf, b0, b1, pb, t0, t1, pt), hb_, ht_ in zip(out, hb, ht):
        print(f"| {name} | {le0}/{ln0}→{le1}/{ln1} | {pf:.2f} | {st.mean(b0):.2f}→{st.mean(b1):.2f} | {pb:.3f} | {hb_:.3f} | {st.mean(t0):.2f}→{st.mean(t1):.2f} | {pt:.3f} | {ht_:.3f} |")


def section_study2():
    data = {a: {(r["task_id"], r["repetition"]): r for r in json.load(open(p))["results"]} for a, p in STUDY2.items()}
    print("\n### Estudio 2: timeouts y rendimiento por brazo\n")
    print("| Brazo | Pass | Timeouts (900 s) | Pass entre no-timeouts | Mediana wall (s) | Mediana s/ronda | Mediana s/ronda sin timeouts | Media tokens de entrada | Media rondas |")
    print("|---|---|---|---|---|---|---|---|---|")
    for a, d in data.items():
        R = list(d.values())
        nt = [r for r in R if r.get("failure_cause") != "timeout"]
        spr = [r["wall_time_ms"] / 1000 / r["rounds"] for r in R if r["rounds"]]
        spr_nt = [r["wall_time_ms"] / 1000 / r["rounds"] for r in nt if r["rounds"]]
        print(f"| {a} | {sum(r['passed'] for r in R)}/{len(R)} | {len(R) - len(nt)} | {sum(r['passed'] for r in nt)}/{len(nt)} | {st.median(r['wall_time_ms'] for r in R) / 1000:.0f} | {st.median(spr):.0f} | {st.median(spr_nt):.1f} | {st.mean(r['input_tokens'] for r in R):.0f} | {st.mean(r['rounds'] for r in R):.1f} |")
    print("\n### Estudio 2: McNemar con y sin pares que involucran un timeout\n")
    print("| Contraste | Discordantes (a gana / b gana) | p | Con timeout | Sin timeout: discordantes | p sin timeout |")
    print("|---|---|---|---|---|---|")
    for a, b in (("seeded", "baseline"), ("empty", "baseline"), ("seeded", "empty")):
        A, B = data[a], data[b]
        keys = [k for k in A if k in B]
        disc = [k for k in keys if A[k]["passed"] != B[k]["passed"]]
        to = [k for k in disc if "timeout" in (A[k].get("failure_cause"), B[k].get("failure_cause"))]
        clean = [k for k in disc if k not in to]
        wa = sum(A[k]["passed"] for k in disc); wb = len(disc) - wa
        ca = sum(A[k]["passed"] for k in clean); cb = len(clean) - ca
        print(f"| {a} − {b} | {wa} / {wb} | {mcnemar(wa, wb):.3f} | {len(to)} | {ca} / {cb} | {mcnemar(ca, cb):.3f} |")


def section_noise_floor():
    R = json.load(open(NOISE_FLOOR))["results"]
    by = defaultdict(list)
    for r in R:
        by[r["task_id"]].append(r)
    pairs = flips = fp_diff = 0
    for runs in by.values():
        for x, y in itertools.combinations(runs, 2):
            pairs += 1
            flips += x["passed"] != y["passed"]
            fp_diff += x.get("outcome_fingerprint") != y.get("outcome_fingerprint")
    print("\n### Barrido del piso de ruido (2026-09-12)\n")
    print(f"- Corridas: {len(R)}, aprobadas: {sum(r['passed'] for r in R)}, timeouts: {sum(r.get('failure_cause') == 'timeout' for r in R)}.")
    print(f"- Pares dentro de tarea: {pairs}; con distinto pass/fail: {flips}; con distinta huella de resultado: {fp_diff}.")
    print("- Con 100 % de aprobación, un cambio de pass/fail es imposible por construcción: el barrido no puede medir la contribución del stack.")


def section_ledger():
    print("\n### Inventario de barridos del Estudio 1 (gpt-oss:20b)\n")
    print("| Archivo | Corridas | Commit | Huella de suite |")
    print("|---|---|---|---|")
    for f in sorted(glob.glob("docs/sweep-memory-distillation-*.json")):
        d = json.load(open(f)); m = d.get("metadata", {})
        print(f"| `{f.split('/')[-1]}` | {len(d['results'])} | {str(m.get('braze_git_commit'))[:7]} | {m.get('suite_fingerprint')} |")


def section_holdout(title, rows):
    h = [r for r in rows if r["task_id"] == "rust_non_borrow_holdout_with_playbook"]
    print(f"\n### Holdout ({title})\n")
    print(f"- Con playbook, sin brazo pareado: {sum(r['passed'] for r in h)}/{len(h)} aprobadas, {st.mean(r['rounds'] for r in h):.2f} rondas medias.")


def section_mechanism(rows_by_exec):
    """Llamadas por herramienta por corrida, ambos ejecutores (§6.1). Mann-Whitney bilateral, Holm por ejecutor y herramienta sobre las tres tareas."""
    print("\n### Llamadas por herramienta por corrida (none→pb), ambos ejecutores\n")
    print("| Ejecutor | Herramienta | original | loop | move |")
    print("|---|---|---|---|---|")
    for ex, rows in rows_by_exec.items():
        tools = sorted({t for r in rows for t in (r.get("tool_call_names") or [])})
        for tool in tools:
            cells, ps = [], []
            for name, base in TASKS:
                n0, n1 = arms(rows, base)
                c0 = [(r.get("tool_call_names") or []).count(tool) for r in n0]
                c1 = [(r.get("tool_call_names") or []).count(tool) for r in n1]
                p = stats.mannwhitneyu(c0, c1, alternative="two-sided").pvalue if set(c0 + c1) != {0} else 1.0
                ps.append(p)
                cells.append((st.mean(c0), st.mean(c1)))
            print(f"| {ex} | `{tool}` | " + " | ".join(f"{a:.2f}→{b:.2f} (Holm {h:.3f})" for (a, b), h in zip(cells, holm(ps))) + " |")
    s1 = rows_by_exec["gpt-oss:20b"]
    fresh_pass = [r for r in s1 if r["passed"] and any(r["task_id"].startswith(b + "_") for _, b in TASKS[1:])]
    one = sum((r.get("tool_call_names") or []).count("edit_file") == 1 for r in fresh_pass)
    print(f"\n- Estudio 1, corridas aprobadas de loop y move (ambos brazos): {len(fresh_pass)}; con exactamente un `edit_file`: {one}.")


def fisher_power(base, drop, n=20, sims=4000, seed=1, alpha=0.05):
    rng = np.random.default_rng(seed)
    a = rng.binomial(n, base, sims)
    b = rng.binomial(n, base - drop, sims)
    return np.mean([fisher(x, n, y, n) < alpha for x, y in zip(a, b)])


def section_power(s1, rep):
    print("\n### Potencia y efectos mínimos detectables (α = 0,05, potencia 80 %)\n")
    print("Tasa de éxito, Fisher bilateral con n = 20 por celda (simulación, 4.000 réplicas, semilla 1), base 0,70:\n")
    print("| Caída de la tasa | Potencia |")
    print("|---|---|")
    for d in (0.2, 0.3, 0.4, 0.5):
        print(f"| {d:.1f} | {fisher_power(0.70, d):.2f} |")
    print("\nRondas: efecto mínimo detectable por tarea, (z₀.₉₇₅ + z₀.₈₀) × EE de Welch:\n")
    print("| Ejecutor | original | loop | move |")
    print("|---|---|---|---|")
    z = stats.norm.ppf(0.975) + stats.norm.ppf(0.80)
    for ex, rows in (("gpt-oss:20b", s1), ("ornith:9b", rep)):
        mdes = []
        for _, base in TASKS:
            n0, n1 = arms(rows, base)
            x, y = [r["rounds"] for r in n0], [r["rounds"] for r in n1]
            se = (st.variance(x) / len(x) + st.variance(y) / len(y)) ** 0.5
            mdes.append(z * se)
        print(f"| {ex} | " + " | ".join(f"{m:.2f}" for m in mdes) + " |")


def exact_signed_rank(d):
    """Wilcoxon de rangos con signo exacto por enumeración de signos (ceros descartados, rangos promedio en empates)."""
    d = [x for x in d if x != 0]
    ranks = stats.rankdata([abs(x) for x in d])
    obs = sum(r for r, x in zip(ranks, d) if x > 0)
    tot = sum(ranks)
    dist = [sum(r for r, s in zip(ranks, signs) if s) for signs in itertools.product((0, 1), repeat=len(d))]
    lo = sum(v <= min(obs, tot - obs) + 1e-9 for v in dist) / len(dist)
    return min(1.0, 2 * lo), len(d)


def section_study2_cluster():
    data = {a: json.load(open(p))["results"] for a, p in STUDY2.items()}
    per_task = {a: defaultdict(int) for a in data}
    for a, R in data.items():
        for r in R:
            per_task[a][r["task_id"]] += r["passed"]
    tasks = sorted(per_task["baseline"])
    cells = len(data["baseline"])
    print("\n### Estudio 2: unidades, Holm y pruebas a nivel de tarea\n")
    print(f"Unidad del McNemar: celdas (tarea, repetición), {cells} por brazo ({len(tasks)} tareas × 3). Las diferencias de pass se cuentan en celdas.\n")
    print("| Contraste | Δ celdas (pp) | p McNemar | Holm (2 contrastes vs baseline) | Signo exacto (tareas, ceros fuera) | Wilcoxon exacto por enumeración (ceros fuera) | Wilcoxon scipy wilcox/aprox. | Wilcoxon scipy pratt/aprox. | Wilcoxon scipy zsplit/aprox. |")
    print("|---|---|---|---|---|---|---|---|---|")
    keyed = {a: {(r["task_id"], r["repetition"]): r["passed"] for r in R} for a, R in data.items()}
    rows, ps = [], []
    for a, b in (("seeded", "baseline"), ("empty", "baseline"), ("seeded", "empty")):
        A, B = keyed[a], keyed[b]
        wa = sum(A[k] and not B[k] for k in A); wb = sum(B[k] and not A[k] for k in A)
        p = mcnemar(wa, wb)
        d = [per_task[a][t] - per_task[b][t] for t in tasks]
        pos, neg = sum(x > 0 for x in d), sum(x < 0 for x in d)
        psign = stats.binomtest(min(pos, neg), pos + neg, 0.5).pvalue
        pex, nz = exact_signed_rank(d)
        var = [stats.wilcoxon(d, zero_method=zm, method="approx").pvalue for zm in ("wilcox", "pratt", "zsplit")]
        rows.append((a, b, wa - wb, p, pos, neg, psign, pex, nz, var))
        if b == "baseline":
            ps.append(p)
    hs = holm(ps) + [None]
    for (a, b, dc, p, pos, neg, psign, pex, nz, var), h in zip(rows, hs):
        hcol = f"{h:.3f}" if h is not None else "—"
        print(f"| {a} − {b} | {dc:+d} ({100 * dc / cells:+.1f}) | {p:.3f} | {hcol} | {pos} vs {neg}, p = {psign:.3f} | p = {pex:.3f} (n = {nz}) | {var[0]:.3f} | {var[1]:.3f} | {var[2]:.3f} |")
    disc = rows[0][4] + rows[0][5]
    print("\nNota: la variante primaria del paper es el Wilcoxon exacto condicional por enumeración de signos con ceros descartados; las variantes de scipy (aproximación normal) se listan porque cambian el lado del 0,05.")
    A, B = keyed["seeded"], keyed["baseline"]
    nd = sum(A[k] != B[k] for k in A)
    k = next(k for k in range(nd // 2, nd + 1) if stats.binomtest(k, nd, 0.5).pvalue < 0.05)
    print(f"\n- Efecto mínimo detectable del McNemar seeded − baseline con {nd} discordantes: {k} vs {nd - k}, es decir Δ = {2 * k - nd} celdas ({100 * (2 * k - nd) / cells:.1f} pp).")


def fig1_points(path, by_exec):
    """Puntos de la figura 1: mismas cifras que section_economics (una sola fuente)."""
    import csv
    with open(path, "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["ejecutor", "tarea", "delta_r", "r_lo", "r_hi", "delta_t", "t_lo", "t_hi", "t_base", "r_mem", "net", "break_even"])
        for ex, rows in by_exec.items():
            for name, base in TASKS:
                n0, n1 = arms(rows, base)
                dR, ciR, _ = welch([r["rounds"] for r in n1], [r["rounds"] for r in n0])
                t0, t1 = per_round(n0), per_round(n1)
                ciT = boot_dT(n0, n1)
                Rm = st.mean(r["rounds"] for r in n1)
                net = st.mean(r["input_tokens"] for r in n1) - st.mean(r["input_tokens"] for r in n0)
                w.writerow([ex, name, f"{dR:.6f}", f"{ciR[0]:.6f}", f"{ciR[1]:.6f}", f"{t1 - t0:.6f}", f"{ciT[0]:.6f}", f"{ciT[1]:.6f}",
                            f"{t0:.6f}", f"{Rm:.6f}", f"{net:.6f}", f"{(t1 - t0) * Rm / t0:.6f}"])


if __name__ == "__main__":
    import sys
    s1, rep = load(STUDY1), load(REPLICATION)
    if len(sys.argv) == 3 and sys.argv[1] == "--fig1-csv":
        fig1_points(sys.argv[2], {"gpt-oss:20b (Study 1)": s1, "ornith:9b (replication)": rep})
        sys.exit(0)
    print("# Reanálisis del Paper 2 (2026-11-06; ampliado 2026-11-14)\n")
    print("Generado por `paper2/analysis/reanalysis.py` desde los JSON de `docs/`. No editar a mano.")
    print("\n## Estudio 1 (gpt-oss:20b, exploratorio)")
    print("\nEl oráculo del Estudio 1 NO corrió `cargo check` (el campo no existe en estos JSON); la corrección funcional aquí es archivo esperado + sin timeout.")
    section_pass("Éxito: oficial frente a corrección funcional", s1, use_cargo=False)
    section_economics("Economía por tarea (rondas, tokens, break-even)", s1)
    section_unaided(s1)
    section_tool_calls(s1)
    section_holdout("gpt-oss:20b", s1)
    print("\n## Réplica (ornith:9b, pre-registrada)")
    section_pass("Éxito: oficial frente a corrección funcional (con cargo check)", rep, use_cargo=True)
    section_economics("Economía por tarea", rep)
    section_holdout("ornith:9b", rep)
    print("\n## Mecanismo (§6.1)")
    section_mechanism({"gpt-oss:20b": s1, "ornith:9b": rep})
    section_power(s1, rep)
    print("\n## Estudio 2 (pre-registrado)")
    section_study2()
    section_study2_cluster()
    section_noise_floor()
    print("\n## Inventario")
    section_ledger()
