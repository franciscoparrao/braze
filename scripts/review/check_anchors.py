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
ITEM_RE = re.compile(r"^\s*(\d+)[.)]\s+")
ANCHOR_RE = re.compile(r"\bL(\d+)(?:\s*[-–]\s*L?(\d+))?\b")
QUOTE_RE = re.compile(r"`([^`]+)`")
VERDICT_RE = re.compile(r"\b(accept|minor revision|major revision|reject)\b", re.IGNORECASE)

MIN_QUOTE_CHARS = 8
WINDOW = 15  # líneas de holgura alrededor del ancla


def normalize(text):
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
    q = normalize(quote)
    return q in normalize(text) or normalize(delatex(quote)) in normalize(delatex(text))


def parse_items(review_lines):
    items, section, current = [], None, None
    for line in review_lines:
        if ANY_HEADING_RE.match(line):
            if current:
                items.append(current)
                current = None
            m = SECTION_RE.match(line)
            section = m.group(1).lower() if m else None
            continue
        if section is None:
            continue
        m = ITEM_RE.match(line)
        if m:
            if current:
                items.append(current)
            current = {"section": section, "n": int(m.group(1)), "text": line.strip()}
        elif current and line.strip():
            current["text"] += " " + line.strip()
    if current:
        items.append(current)
    return items


def check_item(item, manuscript_lines):
    n_lines = len(manuscript_lines)
    anchors = [(int(a), int(b) if b else int(a)) for a, b in ANCHOR_RE.findall(item["text"])]
    quotes = [q for q in QUOTE_RE.findall(item["text"]) if len(q.strip()) >= MIN_QUOTE_CHARS]
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


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("manuscript")
    ap.add_argument("review")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--min-grounded", type=float, default=0.8)
    ap.add_argument("--min-major", type=int, default=2)
    args = ap.parse_args()

    manuscript_lines = open(args.manuscript, encoding="utf-8").read().splitlines()
    review_text = open(args.review, encoding="utf-8").read()
    items = parse_items(review_text.splitlines())
    for item in items:
        item["status"], item["why"] = check_item(item, manuscript_lines)

    rec_block = re.split(r"^#{1,4}\s*recommendation\b", review_text, flags=re.I | re.M)
    verdicts = VERDICT_RE.findall(rec_block[1]) if len(rec_block) > 1 else []
    verdict = verdicts[0].title() if verdicts else None

    grounded = sum(i["status"] == "grounded" for i in items)
    n_major = sum(i["section"] == "major" for i in items)
    ratio = grounded / len(items) if items else 0.0
    problems = []
    if n_major < args.min_major:
        problems.append(f"the review has {n_major} numbered major issues; at least {args.min_major} expected")
    if verdict is None:
        problems.append("no `## Recommendation` section with Accept / Minor Revision / Major Revision / Reject")
    if ratio < args.min_grounded:
        problems.append(f"only {grounded}/{len(items)} issues are anchored in the manuscript")
    ok = not problems

    report = {
        "ok": ok,
        "verdict": verdict,
        "items": len(items),
        "major": n_major,
        "grounded": grounded,
        "ratio": round(ratio, 3),
        "problems": problems,
        "failing": [
            {"section": i["section"], "n": i["n"], "status": i["status"], "why": i["why"]}
            for i in items
            if i["status"] != "grounded"
        ],
    }
    if args.json:
        print(json.dumps(report, indent=2, ensure_ascii=False))
    else:
        print(f"{'PASS' if ok else 'FAIL'}: {grounded}/{len(items)} anchored, "
              f"{n_major} major, verdict={verdict}")
        for p in problems:
            print(f"- {p}")
        for f in report["failing"]:
            print(f"- {f['section']} issue {f['n']}: {f['why']}")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
