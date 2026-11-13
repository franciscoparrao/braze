#!/usr/bin/env python3
"""Reanálisis del Paper 2 tras el review simulado EMSE PROMPT-SE (2026-11-06).

Regenera desde los JSON commiteados en docs/ cada estadístico que el review
cuestionó, y separa lo que se arregla con los datos existentes de lo que pide
corridas nuevas. Plan: paper2/revision-plan-emse-prompt-se-2026-11-06.md.

Uso (desde la raíz del repo):
    python3 paper2/analysis/reanalysis.py > docs/paper2-reanalysis-2026-11-06.md

Sin estado ni red: solo lee docs/. Deterministico.
"""
import glob
import itertools
import json
import statistics as st
from collections import defaultdict

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


def per_round(r):
    return r["input_tokens"] / r["rounds"] if r["rounds"] else 0.0


def section_pass(title, rows, use_cargo):
    print(f"\n### {title}\n")
    print("| Tarea | Pass oficial (none→pb) | p Fisher | Holm | Corrección funcional (none→pb) | p Fisher | Holm | Fallas por tope de tokens (none→pb) | Fallas por tope de rondas (none→pb) | Timeouts (none→pb) |")
    print("|---|---|---|---|---|---|---|---|---|---|")
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
        p_off.append(fisher(a, len(n0), b, len(n1)))
        p_cor.append(fisher(c0, len(n0), c1, len(n1)))
        lines.append((name, f"{a}/{len(n0)}→{b}/{len(n1)}", f"{c0}/{len(n0)}→{c1}/{len(n1)}", tok, rnd, to))
    for (name, off, cor, tok, rnd, to), po, ho, pc, hc in zip(lines, p_off, holm(p_off), p_cor, holm(p_cor)):
        print(f"| {name} | {off} | {po:.3f} | {ho:.3f} | {cor} | {pc:.3f} | {hc:.3f} | {tok[0]}→{tok[1]} | {rnd[0]}→{rnd[1]} | {to[0]}→{to[1]} |")


def section_economics(title, rows):
    print(f"\n### {title}\n")
    print("| Tarea | ΔR (IC 95 %) | p Holm ΔR | T_base | T_mem | ΔT | memory_tokens | ΔT residual | R_mem | ΔR* = ΔT·R_mem/T_base | Net Δtokens (IC 95 %) | p Holm net |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|")
    res = []
    for name, base in TASKS:
        n0, n1 = arms(rows, base)
        r0, r1 = [r["rounds"] for r in n0], [r["rounds"] for r in n1]
        dR, ciR, pR = welch(r1, r0)  # rondas ahorradas = base - mem
        t0, t1 = st.mean(per_round(r) for r in n0), st.mean(per_round(r) for r in n1)
        dT = t1 - t0
        mem = st.mean(r.get("memory_tokens") or 0 for r in n1)
        Rm = st.mean(r1)
        be = dT * Rm / t0
        dN, ciN, pN = welch([r["input_tokens"] for r in n0], [r["input_tokens"] for r in n1])
        res.append((name, dR, ciR, pR, t0, t1, dT, mem, Rm, be, dN, ciN, pN))
    hR, hN = holm([x[3] for x in res]), holm([x[12] for x in res])
    for (name, dR, ciR, pR, t0, t1, dT, mem, Rm, be, dN, ciN, pN), hr, hn in zip(res, hR, hN):
        print(f"| {name} | {dR:+.2f} ({ciR[0]:+.2f}, {ciR[1]:+.2f}) | {hr:.3f} | {t0:.0f} | {t1:.0f} | {dT:+.0f} | {mem:.0f} | {dT - mem:+.0f} | {Rm:.2f} | {be:.2f} | {dN:+.0f} ({ciN[0]:+.0f}, {ciN[1]:+.0f}) | {hn:.3f} |")
    print("\nNota: ΔR > 0 = el playbook ahorra rondas. `memory_tokens` es la estimación del bench (caracteres / 4, `crates/braze-bench/src/memory.rs::estimate_tokens`), no un conteo del tokenizador del modelo; por eso «ΔT residual» NO separa inyección de trayectoria y no debe reportarse.")


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
    print("| Tarea | Corridas con edit (none→pb) | p Fisher | Llamadas antes del primer edit (none→pb) | p Mann-Whitney | Llamadas totales (none→pb) | p Mann-Whitney |")
    print("|---|---|---|---|---|---|---|")
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
        print(f"| {name} | {len(e0)}/{len(n0)}→{len(e1)}/{len(n1)} | {pf:.2f} | {st.mean(b0):.2f}→{st.mean(b1):.2f} | {pb:.3f} | {st.mean(t0):.2f}→{st.mean(t1):.2f} | {pt:.3f} |")


def section_study2():
    data = {a: {(r["task_id"], r["repetition"]): r for r in json.load(open(p))["results"]} for a, p in STUDY2.items()}
    print("\n### Estudio 2: timeouts y rendimiento por brazo\n")
    print("| Brazo | Pass | Timeouts (900 s) | Pass entre no-timeouts | Mediana wall (s) | Mediana s/ronda | Media tokens de entrada |")
    print("|---|---|---|---|---|---|---|")
    for a, d in data.items():
        R = list(d.values())
        nt = [r for r in R if r.get("failure_cause") != "timeout"]
        spr = [r["wall_time_ms"] / 1000 / r["rounds"] for r in R if r["rounds"]]
        print(f"| {a} | {sum(r['passed'] for r in R)}/{len(R)} | {len(R) - len(nt)} | {sum(r['passed'] for r in nt)}/{len(nt)} | {st.median(r['wall_time_ms'] for r in R) / 1000:.0f} | {st.median(spr):.0f} | {st.mean(r['input_tokens'] for r in R):.0f} |")
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


if __name__ == "__main__":
    s1, rep = load(STUDY1), load(REPLICATION)
    print("# Reanálisis del Paper 2 (2026-11-06)\n")
    print("Generado por `paper2/analysis/reanalysis.py` desde los JSON de `docs/`. No editar a mano.")
    print("\n## Estudio 1 (gpt-oss:20b, exploratorio)")
    print("\nEl oráculo del Estudio 1 NO corrió `cargo check` (el campo no existe en estos JSON); la corrección funcional aquí es archivo esperado + sin timeout.")
    section_pass("Éxito: oficial frente a corrección funcional", s1, use_cargo=False)
    section_economics("Economía por tarea (rondas, tokens, break-even)", s1)
    section_unaided(s1)
    section_tool_calls(s1)
    print("\n## Réplica (ornith:9b, pre-registrada)")
    section_pass("Éxito: oficial frente a corrección funcional (con cargo check)", rep, use_cargo=True)
    section_economics("Economía por tarea", rep)
    print("\n## Estudio 2 (pre-registrado)")
    section_study2()
    section_noise_floor()
    print("\n## Inventario")
    section_ledger()
