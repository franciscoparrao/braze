#!/usr/bin/env python3
"""Reporte de rendimiento del harness sobre sesiones reales de braze.

Lee los rollouts de `~/.local/share/braze/sessions/*.jsonl` (o los que se
pasen como argumento) y resume, por pedido del usuario y en total:
rondas, tokens de entrada y salida, aciertos de caché, topes de iteración,
avisos de convergencia, confirmaciones humanas frente a decisiones de
política y del modo automático, errores de herramientas, relecturas del
mismo archivo y comandos `/…` que llegaron al modelo como texto.

Solo lee: no modifica las sesiones. El reporte contiene fragmentos de los
pedidos, así que queda en la máquina donde se corre (no va al repo).

Uso:
    scripts/braze-session-report.py                 # sesiones de los últimos 7 días
    scripts/braze-session-report.py --days 30
    scripts/braze-session-report.py sesion1.jsonl sesion2.jsonl
    scripts/braze-session-report.py --out ~/braze-reporte.md
"""

import argparse
import collections
import datetime as dt
import json
import pathlib
import re
import sys

SESSIONS_DIR = pathlib.Path.home() / ".local/share/braze/sessions"
PATH_IN_TEXT = re.compile(r"((?:[\w.-]+/)+[\w.-]+\.\w+)")


def load_events(path):
    events = []
    for line in path.open(encoding="utf-8"):
        line = line.strip()
        if not line:
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        events.append(event.get("event", event))
    return events


def files_touched(tool_call):
    """Archivos que lee una llamada a herramienta (para contar relecturas)."""
    name, args = tool_call.get("name"), tool_call.get("arguments") or {}
    if name in ("read_file", "edit_file", "write_file"):
        path = args.get("path")
        return [path] if path else []
    if name in ("shell_exec", "grep"):
        text = " ".join(args.get("command", [])) if name == "shell_exec" else str(args)
        return sorted(set(PATH_IN_TEXT.findall(text)))
    return []


def summarize_turn(events):
    """Métricas de los eventos de un pedido (desde su UserMessage)."""
    usage = [e for e in events if e.get("type") == "usage"]
    calls = [e for e in events if e.get("type") == "assistant_tool_call"]
    results = [e for e in events if e.get("type") == "tool_call_completed"]
    notes = [e for e in events if e.get("type") == "harness_note"]
    decided = [e for e in events if e.get("type") == "permission_decided"]
    requested = [e for e in events if e.get("type") == "permission_requested"]
    errors = 0
    for r in results:
        res = r.get("result")
        if isinstance(res, dict) and res.get("is_error"):
            errors += 1
    seen, rereads = set(), 0
    for c in calls:
        if c.get("name") != "read_file":
            continue
        for f in files_touched(c):
            if f in seen:
                rereads += 1
            seen.add(f)
    labels = [d.get("action", "") for d in decided]
    return {
        "rounds": len(usage),
        "input": sum(u.get("input_tokens") or 0 for u in usage),
        "output": sum(u.get("output_tokens") or 0 for u in usage),
        "cache_read": sum(u.get("cache_read_tokens") or 0 for u in usage),
        "tool_calls": len(calls),
        "tool_errors": errors,
        "tools": collections.Counter(c.get("name") for c in calls),
        "cap_hit": any(n.get("kind") == "iteration_cap" for n in notes),
        "converge_note": any(n.get("kind") == "iteration_converge" for n in notes),
        "human_prompts": len(requested),
        "auto_decisions": sum(l.startswith("[auto:") for l in labels),
        "auto_denied": sum(l.startswith("[auto:") and not d.get("allowed") for l, d in zip(labels, decided)),
        "policy_decisions": sum(l.startswith("[policy:") for l in labels),
        "compactions": sum(e.get("type") == "compaction_occurred" for e in events),
        "rereads": rereads,
    }


def split_turns(events):
    turns, current, text = [], [], None
    for e in events:
        if e.get("type") == "user_message":
            if text is not None:
                turns.append((text, current))
            text, current = e.get("text", ""), []
        elif text is not None:
            current.append(e)
    if text is not None:
        turns.append((text, current))
    return turns


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("sessions", nargs="*", type=pathlib.Path)
    ap.add_argument("--days", type=int, default=7)
    ap.add_argument("--out", type=pathlib.Path)
    a = ap.parse_args()

    if a.sessions:
        paths = a.sessions
    else:
        cutoff = dt.datetime.now().timestamp() - a.days * 86400
        paths = sorted(
            (p for p in SESSIONS_DIR.glob("*.jsonl") if p.stat().st_mtime >= cutoff),
            key=lambda p: p.stat().st_mtime,
        )

    lines = [f"# Reporte de sesiones de braze ({dt.date.today().isoformat()})", ""]
    totals = collections.Counter()
    all_tools = collections.Counter()
    turn_rows = []
    for path in paths:
        events = load_events(path)
        for text, turn_events in split_turns(events):
            m = summarize_turn(turn_events)
            leaked = text.strip().startswith("/") and text.strip().split()[0] not in ()
            turn_rows.append((path.stem[:8], text, m, leaked))
            for k in ("rounds", "input", "output", "cache_read", "tool_calls", "tool_errors",
                      "human_prompts", "auto_decisions", "auto_denied", "policy_decisions",
                      "compactions", "rereads"):
                totals[k] += m[k]
            totals["turns"] += 1
            totals["cap_hits"] += m["cap_hit"]
            totals["leaked_slash"] += leaked
            all_tools.update(m["tools"])

    lines += [f"Sesiones: {len(paths)} · pedidos: {totals['turns']}", ""]
    if totals["turns"]:
        cache = 100 * totals["cache_read"] / totals["input"] if totals["input"] else 0
        lines += [
            "## Totales",
            "",
            f"- Rondas: {totals['rounds']} ({totals['rounds'] / totals['turns']:.1f} por pedido); "
            f"pedidos que tocaron el tope de iteraciones: {totals['cap_hits']}.",
            f"- Tokens: {totals['input']:,} de entrada ({cache:.0f} % desde caché), {totals['output']:,} de salida.",
            f"- Herramientas: {totals['tool_calls']} llamadas, {totals['tool_errors']} con error; "
            f"relecturas de un archivo ya leído en el mismo pedido: {totals['rereads']}.",
            f"- Permisos: {totals['human_prompts']} confirmaciones pedidas a una persona, "
            f"{totals['auto_decisions']} decisiones automáticas ({totals['auto_denied']} denegadas), "
            f"{totals['policy_decisions']} por política.",
            f"- Compactaciones: {totals['compactions']}. Comandos `/…` enviados al modelo como texto: {totals['leaked_slash']}.",
            f"- Herramientas más usadas: {', '.join(f'{n} {c}' for n, c in all_tools.most_common(6))}.",
            "",
            "## Por pedido",
            "",
            "| Sesión | Pedido | Rondas | Tokens entrada | Tope | Confirmaciones | Auto | Errores | Relecturas |",
            "|---|---|---|---|---|---|---|---|---|",
        ]
        for sid, text, m, _ in turn_rows:
            short = text.strip().replace("\n", " ").replace("|", "\\|")[:60]
            lines.append(
                f"| {sid} | {short} | {m['rounds']} | {m['input']:,} | {'sí' if m['cap_hit'] else ''} | "
                f"{m['human_prompts']} | {m['auto_decisions']} | {m['tool_errors']} | {m['rereads']} |"
            )
    report = "\n".join(lines) + "\n"
    if a.out:
        a.out.write_text(report, encoding="utf-8")
        print(f"reporte escrito en {a.out}")
    else:
        sys.stdout.write(report)


if __name__ == "__main__":
    main()
