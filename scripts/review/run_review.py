#!/usr/bin/env python3
"""Revisión confidencial de un manuscrito con un modelo local, por map-reduce.

El recorrido del documento lo dirige este script, no el modelo
(calibración 2, 2026-10-08: gpt-oss:20b, encargado de navegar 9 bloques
con read_file, se quedó en el primero y escribió la reseña desde el
abstract). Fases, todas sin tools — el texto va en el prompt y la
respuesta se guarda tal cual, así que nada viaja como argumento JSON
(donde `\\times` llegaba corrupto):

1. map: una corrida de braze por bloque de líneas numeradas → notas
   ancladas (`L123` + frase exacta); se valida cada bloque con
   check_anchors y se reintenta una vez si sale mal anclado.
2. reduce: una corrida que escribe la reseña desde notes.md.
3. repair: si el chequeo de anclaje falla, se le devuelve la reseña con
   los fallos y las líneas del manuscrito que citó; hasta N veces.

Uso: run_review.py MANUSCRITO CRITERIOS OUTDIR [--journal NOMBRE]
     [--chunk 120] [--repairs 2] [--braze target/release/braze]
Usa el perfil scripts/review/profile (XDG_CONFIG_HOME): modelo local,
sin shell ni red. Sale 0 si la reseña pasa el chequeo.
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from check_anchors import check_text, format_report  # noqa: E402

REPO = HERE.parent.parent


def log(msg):
    print(f"[review {time.strftime('%H:%M:%S')}] {msg}", flush=True)


def fill(template, **values):
    for key, value in values.items():
        template = template.replace("{" + key + "}", str(value))
    return template


def numbered(lines, start, end):
    """Líneas [start, end] (1-indexadas) como `L123| texto`."""
    return "\n".join(f"L{i}| {lines[i - 1]}" for i in range(start, end + 1))


def run_braze(braze, workdir, label, prompt, max_tokens=None):
    """Una corrida one-shot; devuelve el texto final (o "")."""
    env = dict(os.environ, XDG_CONFIG_HOME=str(HERE / "profile"))
    if max_tokens:
        env["BRAZE_MAX_TOKENS"] = str(max_tokens)
    env.setdefault("RUST_LOG", "braze_engine=warn")
    with open(workdir / f"{label}.log", "w") as err:
        proc = subprocess.run(
            [braze, "run", "--output-format", "json", prompt],
            cwd=workdir, env=env, stdout=subprocess.PIPE, stderr=err, text=True,
        )
    (workdir / f"{label}.json").write_text(proc.stdout)
    try:
        data = json.loads(proc.stdout)
    except json.JSONDecodeError:
        return ""
    return data.get("text") or ""


def from_heading(text, heading_re):
    """Descarta lo previo al primer encabezado esperado: gpt-oss vía
    Ollama a veces filtra su razonamiento antes de la respuesta."""
    m = re.search(heading_re, text, flags=re.M)
    return text[m.start():].strip() if m else ""


def cited_lines(review, manuscript_lines, context=3):
    """Las líneas que la reseña cita (±context), numeradas, para la reparación."""
    n = len(manuscript_lines)
    wanted = set()
    for a, b in re.findall(r"\bL(\d+)(?:\s*[-–]\s*L?(\d+))?\b", review):
        a = int(a)
        b = int(b) if b else a
        if 1 <= a <= n and a <= b <= n and b - a <= 40:
            wanted.update(range(max(1, a - context), min(n, b + context) + 1))
    return "\n".join(f"L{i}| {manuscript_lines[i - 1]}" for i in sorted(wanted))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("manuscript")
    ap.add_argument("criteria")
    ap.add_argument("outdir")
    ap.add_argument("--journal", default="the target journal")
    ap.add_argument("--chunk", type=int, default=120)
    ap.add_argument("--repairs", type=int, default=2)
    ap.add_argument("--braze", default=str(REPO / "target" / "release" / "braze"))
    ap.add_argument("--only", default="",
                    help="re-correr solo estos bloques (p.ej. 3,7,9) reusando OUTDIR/notes.md")
    args = ap.parse_args()
    only = {int(x) for x in args.only.split(",") if x.strip()}

    out = Path(args.outdir)
    out.mkdir(parents=True, exist_ok=True)
    shutil.copy(args.manuscript, out / "manuscript.tex")
    shutil.copy(args.criteria, out / "criteria.md")
    lines = (out / "manuscript.tex").read_text(encoding="utf-8").splitlines()
    criteria = (out / "criteria.md").read_text(encoding="utf-8")
    n = len(lines)
    chunks = [(a, min(a + args.chunk - 1, n)) for a in range(1, n + 1, args.chunk)]
    map_tpl = (HERE / "map-prompt.md").read_text()
    reduce_tpl = (HERE / "reduce-prompt.md").read_text()

    # --- map ---------------------------------------------------------------
    notes_parts = []
    previous_notes = {}  # inicio de bloque → partes ya escritas (para --only)
    if only and (out / "notes.md").exists():
        text = (out / "notes.md").read_text(encoding="utf-8")
        for part in re.split(r"(?m)^(?=## Lines )", text):
            m = re.match(r"## Lines (\d+)", part)
            if m:
                previous_notes.setdefault(int(m.group(1)), []).append(part.strip())

    def map_chunk(label, a, b, i, attempts):
        """Mejor nota de `attempts` = [max_tokens, ...] (None = el del perfil)."""
        previous = "\n".join(p.splitlines()[0] for p in notes_parts) or "(none yet)"
        prompt = fill(map_tpl, JOURNAL=args.journal, A=a, B=b, N=n, I=i, K=len(chunks),
                      CRITERIA=criteria, PREVIOUS=previous, LINES=numbered(lines, a, b))
        best, best_report = "", check_text(lines, "", notes=True)
        for k, max_tokens in enumerate(attempts):
            text = run_braze(args.braze, out, f"{label}-{k}", prompt, max_tokens)
            part = from_heading(text, r"^## Lines")
            report = check_text(lines, part, min_grounded=0.6, notes=True)
            if report["grounded"] > best_report["grounded"]:
                best, best_report = part, report
            if report["ok"]:
                break
        return best, best_report

    for i, (a, b) in enumerate(chunks, 1):
        if only and i not in only:
            kept = [p for start, parts in sorted(previous_notes.items())
                    if a <= start <= b for p in parts]
            notes_parts.extend(kept)
            log(f"map {i}/{len(chunks)} L{a}-{b}: reusado ({len(kept)} parte(s))")
            continue
        # Calibración 3: el bloque de Related Work (citas densas) agotó los
        # 6144 tokens de salida dos veces seguidas. Segundo intento con más
        # salida; si tampoco, el bloque se parte en dos mitades.
        best, report = map_chunk(f"map{i:02d}", a, b, i, [None, 12288])
        pieces = [(best, report, a, b)]
        if not best and b > a:
            mid = (a + b) // 2
            pieces = [(*map_chunk(f"map{i:02d}{s}", x, y, i, [12288]), x, y)
                      for s, (x, y) in (("a", (a, mid)), ("b", (mid + 1, b)))]
        for part, rep, x, y in pieces:
            log(f"map {i}/{len(chunks)} L{x}-{y}: {rep['grounded']}/{rep['items']} anclados")
            if part:
                notes_parts.append(part)
        (out / "notes.md").write_text("\n\n".join(notes_parts) + "\n")

    notes = "\n\n".join(notes_parts)
    notes_report = check_text(lines, notes, notes=True)
    log(f"notas: {notes_report['grounded']}/{notes_report['items']} viñetas ancladas")

    # --- reduce ------------------------------------------------------------
    prompt = fill(reduce_tpl, JOURNAL=args.journal, N=n, CRITERIA=criteria, NOTES=notes)
    review = from_heading(run_braze(args.braze, out, "reduce", prompt), r"^## Summary")
    (out / "review.md").write_text(review + "\n")

    # --- repair ------------------------------------------------------------
    for attempt in range(args.repairs + 1):
        report = check_text(lines, review)
        (out / "check.txt").write_text(format_report(report) + "\n")
        log(format_report(report).replace("\n", "\n    "))
        if report["ok"]:
            log("PASS")
            return 0
        if attempt == args.repairs:
            break
        repair = (
            f"You are repairing a peer review of a {n}-line manuscript for {args.journal}. "
            "Do not use tools.\n\nAn automatic grounding check rejected the review:\n\n"
            f"{format_report(report)}\n\nThe manuscript lines the review cites:\n\n"
            f"{cited_lines(review, lines)}\n\nYour reading notes:\n\n{notes}\n\n"
            f"The review:\n\n{review}\n\n"
            "Rewrite the complete review with the same sections. Every numbered issue under "
            "'## Major issues' and '## Minor issues' needs an anchor L123 and an exact phrase in "
            "backticks copied from the cited lines or the notes; fix the anchor or the phrase, or "
            "delete the issue if nothing supports it. Start your answer with `## Summary`."
        )
        fixed = from_heading(run_braze(args.braze, out, f"repair{attempt + 1}", repair),
                             r"^## Summary")
        if fixed:
            review = fixed
            (out / "review.md").write_text(review + "\n")
    log(f"sin pasar tras {args.repairs} reparaciones")
    return 1


if __name__ == "__main__":
    sys.exit(main())
