#!/usr/bin/env python3
"""Chequeo de anclaje de una revisión generada contra su manuscrito.

Cada ítem numerado de `## Major issues` y `## Minor issues` debe citar una
línea (`L123` o `L123-L130`) y copiar entre backticks una frase que exista
en el manuscrito cerca de esa línea. Un ítem sin ancla verificable no se
puede distinguir de una alucinación (calibración del revisor local,
2026-10-08: la reseña hablaba de un paper que no existe).

Uso: check_anchors.py MANUSCRITO REVISION [--json] [--min-grounded 0.8]
Sale 0 si la revisión pasa; 1 si no, con el detalle por ítem en stdout
(redactado para dárselo al modelo como pedido de reparación).
"""

import argparse
import json
import re
import sys

SECTION_RE = re.compile(r"^#{1,4}\s*(major|minor)\b", re.IGNORECASE)
ANY_HEADING_RE = re.compile(r"^#{1,4}\s")
# Numerados (`1.`, `2)`) y viñetas (`-`, `*`): la calibración 2 trajo los
# menores como viñetas y escaparon al chequeo.
ITEM_RE = re.compile(r"^\s*(?:(\d+)[.)]|[-*])\s+")
ANCHOR_RE = re.compile(r"\bL(\d+)(?:\s*[-–]\s*L?(\d+))?\b")
# Cita = backticks o comillas dobles (rectas o tipográficas): la síntesis 4
# citó como *“...”* con el ancla en backticks.
QUOTE_RE = re.compile(r"`([^`]+)`|“([^”]+)”|\"([^\"]+)\"")
ANCHOR_ONLY_RE = re.compile(r"^L\d+(\s*[-–]\s*L?\d+)?$")
VERDICT_RE = re.compile(r"\b(accept|minor revision|major revision|reject)\b", re.IGNORECASE)

MIN_QUOTE_CHARS = 8
WINDOW = 15  # líneas de holgura alrededor del ancla


# El modelo cita el texto visible con tipografía Unicode (`same‑prompt` con
# U+2011, `×`, `≈`) donde el .tex tiene ASCII o comandos (`same-prompt`,
# `\times`, `$\approx$`) — calibración 3: 7 de 10 citas fallaban solo por
# eso. Se pliegan ambos lados a la misma forma.
FOLD = str.maketrans({
    "‐": "-", "‑": "-", "‒": "-", "–": "-", "—": "-",
    "−": "-", " ": " ", "‘": "'", "’": "'", "“": '"',
    "”": '"', "×": " ", "≈": " ", "∼": " ", "$": "",
    # `\alpha`, `\Delta`… desaparecen con delatex; su forma Unicode también.
    "α": " ", "β": " ", "Δ": " ", "δ": " ", "σ": " ", "μ": " ",
})


def normalize(text):
    text = text.translate(FOLD).replace("``", '"').replace("''", '"')
    text = re.sub(r"-{2,}", "-", text)
    return re.sub(r"\s+", " ", text).strip().lower()


def delatex(text):
    """Quita comandos LaTeX conservando su argumento: el modelo suele
    citar el texto visible (`noise floor`) y no `\\emph{noise} floor`."""
    text = re.sub(r"\\[a-zA-Z]+\*?(\[[^\]]*\])?\{([^{}]*)\}", r"\2", text)
    text = re.sub(r"\\[a-zA-Z]+\*?", " ", text)
    text = re.sub(r"[{}~]", " ", text)
    text = text.replace("\\%", "%").replace("\\_", "_").replace("\\&", "&")
    return text


def found_in(quote, text):
    """Una cita con elipsis (`a ... b`) vale si cada fragmento largo aparece."""
    quote = quote.strip().rstrip(".,;:")  # la puntuación final la pone el revisor
    fragments = [f for f in re.split(r"\.\.\.|…", quote) if len(f.strip()) >= MIN_QUOTE_CHARS]
    if not fragments:
        fragments = [quote]
    plain, latexless = normalize(text), normalize(delatex(text))
    return all(normalize(f) in plain or normalize(delatex(f)) in latexless for f in fragments)


def parse_items(review_lines, notes=False):
    """Ítems bajo `## Major`/`## Minor`; con `notes=True`, toda viñeta del
    archivo (notas por bloque, bajo `## Lines A-B`)."""
    items, current = [], None
    section = "notes" if notes else None
    counts = {}
    for line in review_lines:
        if ANY_HEADING_RE.match(line):
            if current:
                items.append(current)
                current = None
            if not notes:
                m = SECTION_RE.match(line)
                section = m.group(1).lower() if m else None
            continue
        if section is None:
            continue
        m = ITEM_RE.match(line)
        indent = len(line) - len(line.lstrip())
        # Una sub-viñeta (más indentada que el ítem abierto) es parte de él:
        # la síntesis 4 puso cada ancla en `   - Anchor: ...` bajo `1. ...`.
        if m and not (current and indent > current["indent"]):
            if current:
                items.append(current)
            counts[section] = counts.get(section, 0) + 1
            n = int(m.group(1)) if m.group(1) else counts[section]
            current = {"section": section, "n": n, "text": line.strip(), "indent": indent}
        elif current and line.strip():
            current["text"] += " " + line.strip()
    if current:
        items.append(current)
    return items


def check_item(item, manuscript_lines):
    n_lines = len(manuscript_lines)
    anchors = [(int(a), int(b) if b else int(a)) for a, b in ANCHOR_RE.findall(item["text"])]
    spans = (next(g for g in m.groups() if g is not None) for m in QUOTE_RE.finditer(item["text"]))
    quotes = [q for q in spans
              if len(q.strip()) >= MIN_QUOTE_CHARS and not ANCHOR_ONLY_RE.match(q.strip())]
    whole = "\n".join(manuscript_lines)
    if not anchors:
        return "no_anchor", "no line anchor like L123"
    bad = [a for a in anchors if not (1 <= a[0] <= n_lines and a[0] <= a[1] <= n_lines)]
    if bad:
        return "anchor_out_of_range", f"anchor L{bad[0][0]} is outside 1-{n_lines}"
    if not quotes:
        return "no_quote", "no exact phrase in backticks (at least 8 characters)"
    for start, end in anchors:
        lo, hi = max(0, start - 1 - WINDOW), min(n_lines, end + WINDOW)
        window = "\n".join(manuscript_lines[lo:hi])
        if any(found_in(q, window) for q in quotes):
            return "grounded", ""
    if any(found_in(q, whole) for q in quotes):
        return "quote_elsewhere", "the quoted phrase exists, but not near the cited line"
    return "quote_not_found", f"the quoted phrase `{quotes[0][:60]}` does not appear in the manuscript"


def check_text(manuscript_lines, review_text, min_grounded=0.8, min_major=2, notes=False):
    """Chequeo completo; devuelve el reporte (dict). Importable desde
    run_review.py, que lo usa también para validar cada bloque de notas."""
    items = parse_items(review_text.splitlines(), notes=notes)
    for item in items:
        item["status"], item["why"] = check_item(item, manuscript_lines)
    grounded = sum(i["status"] == "grounded" for i in items)
    ratio = grounded / len(items) if items else 0.0
    failing = [
        {"section": i["section"], "n": i["n"], "status": i["status"], "why": i["why"]}
        for i in items
        if i["status"] != "grounded"
    ]
    if notes:
        return {"ok": bool(items) and ratio >= min_grounded, "items": len(items),
                "grounded": grounded, "ratio": round(ratio, 3), "failing": failing}

    rec_block = re.split(r"^#{1,4}\s*recommendation\b", review_text, flags=re.I | re.M)
    verdicts = VERDICT_RE.findall(rec_block[1]) if len(rec_block) > 1 else []
    verdict = verdicts[0].title() if verdicts else None
    n_major = sum(i["section"] == "major" for i in items)
    problems = []
    if n_major < min_major:
        problems.append(f"the review has {n_major} major issues; at least {min_major} expected")
    if verdict is None:
        problems.append("no `## Recommendation` section with Accept / Minor Revision / Major Revision / Reject")
    if ratio < min_grounded:
        problems.append(f"only {grounded}/{len(items)} issues are anchored in the manuscript")
    return {"ok": not problems, "verdict": verdict, "items": len(items), "major": n_major,
            "grounded": grounded, "ratio": round(ratio, 3), "problems": problems,
            "failing": failing}


def format_report(report):
    lines = [f"{'PASS' if report['ok'] else 'FAIL'}: {report['grounded']}/{report['items']} anchored"
             + (f", {report['major']} major, verdict={report['verdict']}" if "major" in report else "")]
    lines += [f"- {p}" for p in report.get("problems", [])]
    lines += [f"- {f['section']} issue {f['n']}: {f['why']}" for f in report["failing"]]
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("manuscript")
    ap.add_argument("review")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--notes", action="store_true", help="validar un archivo de notas (toda viñeta)")
    ap.add_argument("--min-grounded", type=float, default=0.8)
    ap.add_argument("--min-major", type=int, default=2)
    args = ap.parse_args()

    manuscript_lines = open(args.manuscript, encoding="utf-8").read().splitlines()
    review_text = open(args.review, encoding="utf-8").read()
    report = check_text(manuscript_lines, review_text, args.min_grounded, args.min_major, args.notes)
    print(json.dumps(report, indent=2, ensure_ascii=False) if args.json else format_report(report))
    sys.exit(0 if report["ok"] else 1)


if __name__ == "__main__":
    main()
