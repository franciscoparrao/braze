#!/usr/bin/env python3
"""Revisión de un documento contra una pauta, ítem por ítem, con un modelo local.

Complementa a run_review.py (map-reduce), que anota lo que el documento
DICE y por eso no ve lo que le FALTA. En una evaluación contra pauta (p.ej.
una DIA frente a la guía del SEA, donde el 84 % de las observaciones del
evaluador son omisiones) el recorrido lo manda la pauta: una corrida de
braze por ítem, con grep/read_file sobre el documento, que responde qué
requisitos faltan o están incompletos y con qué evidencia. Es la mecánica
del verificador de run_review.py, que maneja bien las ausencias porque
las busca.

Entradas:
- DOCUMENTO: texto plano (se copia como document.txt en OUTDIR).
- ITEMS: JSON [{"id", "title", "text"}, ...].

Salidas en OUTDIR: item<ID>-<k>.{json,log} por corrida, observations.json
(todas las observaciones con su ítem y el resultado del chequeo de su
evidencia) y observations.md.

Uso: run_checklist.py DOCUMENTO ITEMS OUTDIR --context "..." [--only 01,07]
     [--model M] [--ollama-url URL] [--braze target/release/braze]
"""

import argparse
import json
import math
import os
import re
import shutil
import sys
import time
import unicodedata
from collections import Counter
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from check_anchors import found_in  # noqa: E402
from run_review import REPO, fill, log, run_braze  # noqa: E402

OBS_RE = re.compile(r"^\s*\**OBS\**:\s*(.+)$", re.M)
EVID_RE = re.compile(r"^\s*\**EVIDENCIA\**:\s*(.+)$", re.M)
NONE_RE = re.compile(r"^\s*\**SIN OBSERVACIONES\**:?\s*(.*)$", re.M)
ANCHOR_RE = re.compile(r"\bL(\d+)\b[^`]*`([^`]+)`")
GREP_RE = re.compile(r"grep\b.*sin resultados|no encontrado en los pasajes", re.I)

STOP = set("""para como este esta estos estas sobre entre desde hasta según segun cada todo toda todos
todas otro otra otros otras debe deben deberá debera indicar describir señalar presentar
caso donde cual cuales cuando tipo forma parte partes obras obra proyecto fase según
también tanto mismo misma dicho dicha respecto mediante través traves corresponda ejemplo""".split())


def fold(text):
    text = unicodedata.normalize("NFKD", text.lower())
    return "".join(c for c in text if not unicodedata.combining(c))


def terms(text):
    return [t for t in re.findall(r"[a-z0-9]{4,}", fold(text)) if t not in STOP]


class Passages:
    """Ventanas de líneas del documento, recuperadas por BM25 contra el ítem.

    Sin tools no hay rondas ni JSON de argumentos: en la prueba con tools,
    gpt-oss:20b tardó ~8 min por ítem y la corrida murió una vez por read
    timeout (prefill silencioso) y otra por el 500 'error parsing tool
    call' de Ollama. El precio: una ausencia en los pasajes no prueba una
    ausencia en el documento (queda marcada para verificar).
    """

    def __init__(self, lines, window=12, step=6):
        self.lines = lines
        # Índice/TOC ("TABLA 4-5 .......... 32") solo hace ruido.
        self.skip = {i for i, ln in enumerate(lines) if re.search(r"\.{8,}\s*\d+\s*$", ln)}
        self.wins = [(a, min(a + window, len(lines))) for a in range(0, len(lines), step)]
        self.tf = [Counter(t for i in range(a, b) if i not in self.skip for t in terms(lines[i]))
                   for a, b in self.wins]
        df = Counter(t for c in self.tf for t in c)
        n = len(self.wins)
        self.idf = {t: math.log(1 + (n - d + 0.5) / (d + 0.5)) for t, d in df.items()}
        self.avg = sum(sum(c.values()) for c in self.tf) / max(n, 1)

    def select(self, query, budget):
        return self.render(self.choose(query, budget))

    def choose(self, query, budget):
        """Índices de línea de las mejores ventanas hasta `budget` caracteres."""
        q = Counter(terms(query))
        scores = []
        for k, c in enumerate(self.tf):
            dl = sum(c.values()) or 1
            s = sum(self.idf.get(t, 0) * c[t] * 2.2 / (c[t] + 1.2 * (0.25 + 0.75 * dl / self.avg))
                    for t in q if t in c)
            scores.append((s, k))
        chosen, used = set(), 0
        for s, k in sorted(scores, reverse=True):
            if s <= 0 or used >= budget:
                break
            a, b = self.wins[k]
            new = [i for i in range(a, b) if i not in chosen and i not in self.skip]
            chosen.update(new)
            used += sum(len(self.lines[i]) + 8 for i in new)
        return chosen

    def render(self, chosen):
        out, prev = [], None
        for i in sorted(chosen):
            if prev is not None and i != prev + 1:
                out.append("…")
            out.append(f"L{i + 1}| {self.lines[i]}")
            prev = i
        return "\n".join(out)


def parse(text):
    """Pares OBS/EVIDENCIA en orden; None si la respuesta no tiene formato."""
    obs = [(m.start(), m.group(1).strip()) for m in OBS_RE.finditer(text)]
    evid = [(m.start(), m.group(1).strip()) for m in EVID_RE.finditer(text)]
    if not obs:
        return [] if NONE_RE.search(text) else None
    pairs = []
    for k, (pos, o) in enumerate(obs):
        nxt = obs[k + 1][0] if k + 1 < len(obs) else len(text)
        e = next((e for p, e in evid if pos < p < nxt), "")
        pairs.append({"obs": o, "evidence": e})
    return pairs


def check_evidence(evidence, lines, window=3):
    """'anchored' si la frase está en la línea citada (±window); 'absence' si
    la evidencia es una búsqueda sin resultados; si no, 'unanchored'."""
    m = ANCHOR_RE.search(evidence)
    if m:
        n, phrase = int(m.group(1)), m.group(2)
        if 1 <= n <= len(lines):
            span = " ".join(lines[max(0, n - 1 - window):n + window])
            if found_in(phrase, span):
                return "anchored"
        return "unanchored"
    if GREP_RE.search(evidence):
        return "absence"
    return "unanchored"


VERDICT_RE = re.compile(r"^\s*\**V(\d+)\**\s*:\**\s*\**(PRESENTE|AUSENTE|NO APLICA|SECCI[ÓO]N|RESPONDIDA|ACEPTADA)\**[ \t]*(.*)$", re.M | re.I)


BATCH = 6
SECTION_RE = re.compile(r"^SECCI[ÓO]N\s+(\d+(?:\.\d+)*)", re.I)
TOC_LINE_RE = re.compile(r"^\s*(\d+(?:\.\d+)*)\.?\s+(\S.*?)\s*\.{8,}\s*\d+\s*$")


def toc(lines):
    """El índice de secciones numeradas del documento, compacto ("5.1.5 Construcción de zanjas")."""
    return [f"{m.group(1)} {m.group(2)}" for m in map(TOC_LINE_RE.match, lines) if m]


def section_text(lines, num, max_lines=80, max_chars=5000):
    """Las líneas de la sección `num` en el cuerpo (no en el índice), numeradas,
    hasta el próximo encabezado numerado o los topes."""
    head = re.compile(rf"^\s*{re.escape(num)}\.?\s+\S")
    any_head = re.compile(r"^\s*\d+(?:\.\d+)+\.?\s+[A-ZÁÉÍÓÚÑ]")
    for i, ln in enumerate(lines):
        if head.match(ln) and not TOC_LINE_RE.match(ln):
            out, used = [f"L{i + 1}| {ln}"], len(ln)
            for k in range(i + 1, min(i + max_lines, len(lines))):
                if any_head.match(lines[k]) or used > max_chars:
                    break
                out.append(f"L{k + 1}| {lines[k]}")
                used += len(lines[k]) + 8
            return "\n".join(out)
    return ""


def substantive(evidence, min_words=8):
    """Una defensa citando un título ("Compatibilidad con los instrumentos de
    planificación territorial.") no prueba que el contenido esté: se exige una
    frase de al menos `min_words` palabras o con una cifra (fila de tabla)."""
    m = re.search(r"`([^`]+)`", evidence)
    quote = m.group(1) if m else ""
    return len(quote.split()) >= min_words or bool(re.search(r"\d", quote))


def parse_verdicts(text, n):
    """{k: (veredicto, resto)} para V1..Vn; None si falta alguna."""
    # El prompt de defensa usa RESPONDIDA/ACEPTADA; se normaliza a PRESENTE/AUSENTE.
    alias = {"RESPONDIDA": "PRESENTE", "ACEPTADA": "AUSENTE"}
    got = {int(m.group(1)): (alias.get(v, v), m.group(3).strip())
           for m in VERDICT_RE.finditer(text) for v in [m.group(2).upper().replace("Ó", "O")]}
    return got if all(k in got for k in range(1, n + 1)) else None


def verify(args, out, lines, items, results, passages, extra):
    """Segunda etapa: re-busca en el documento completo lo que cada observación
    da por ausente (o cita mal) y le pide al modelo un veredicto por observación.

    Una ausencia del primer paso solo prueba que el pasaje no se recuperó: en
    Purranque la mitad de las observaciones falsas de una muestra eran de ese
    tipo. Aquí la consulta es la observación misma, no el ítem, así que los
    pasajes son otros. Se descarta una observación si el verificador la marca
    PRESENTE con una cita anclada, o NO APLICA; una PRESENTE sin ancla se
    conserva (no hay prueba).
    """
    tpl = Path(args.verify_prompt or HERE / "checklist-defense-prompt.md").read_text(encoding="utf-8")
    state = out / "verify-state.json"           # checkpoint: solo ítems verificados
    done = json.loads(state.read_text()) if state.exists() else {}
    by_id = {it["id"]: it for it in items}
    limit = {x.strip() for x in args.verify_ids.split(",") if x.strip()}
    todo = [iid for iid, r in results.items()
            if any(p["check"] != "anchored" for p in r["observations"]) and (not limit or iid in limit)]
    for k, iid in enumerate(todo, 1):
        if iid in done:
            continue
        item, r = by_id[iid], results[iid]

        def ask(js, passages_txt, index, tag=""):
            obs_txt = "\n".join(f"V{n}: {r['observations'][j]['obs']}" for n, j in enumerate(js, 1))
            prompt = fill(tpl, CONTEXT=args.context, N=len(lines), TITLE=item["title"], TEXT=item["text"],
                          OBSERVATIONS=obs_txt, PASSAGES=passages_txt,
                          TOC=f"El índice del documento (solo títulos):\n\n{index}\n\n" if index else "",
                          SECTION_OPTION=("- SECCION <número>: el índice muestra una sección que probablemente lo "
                                          "entrega, pero su texto no está en los pasajes.\n") if index else "",
                          SECTION_EXAMPLE="\nV4: SECCION 5.1.5" if index else "")
            for attempt in range(3):
                text = run_braze(args.braze, out, f"verify{iid}{tag}-{attempt}", prompt, extra=extra)
                got = parse_verdicts(text, len(js))
                if got is not None:
                    return got
                if not text:                  # error de transporte (WiFi, Ollama reiniciando)
                    time.sleep(60)
            return None

        targets = [j for j, p in enumerate(r["observations"]) if p["check"] != "anchored"]
        index = "\n".join(toc(lines))
        verdicts = {}
        # Lotes chicos: con 12-15 observaciones por llamada el modelo responde
        # AUSENTE en serie sin mirar.
        for b in range(0, len(targets), BATCH):
            group = targets[b:b + BATCH]
            share = max(2500, args.budget // len(group))
            chosen = set()
            for j in group:
                chosen |= passages.choose(r["observations"][j]["obs"], share)
            got = ask(group, passages.render(chosen), index, tag=f"-b{b // BATCH}")
            if got is None:
                continue
            want = {}
            for n, j in enumerate(group, 1):
                v, rest = got[n]
                m = SECTION_RE.match(f"{v} {rest}")
                if m:
                    want.setdefault(m.group(1), []).append(j)
                verdicts[j] = got[n]
            # Segunda ronda: lo que el índice ubica en una sección no recuperada.
            for num, js in want.items():
                sec = section_text(lines, num)
                again = ask(js, sec, "", tag=f"-b{b // BATCH}s{num}") if sec else None
                for n2, j in enumerate(js, 1):
                    verdicts[j] = (again or {}).get(n2, ("AUSENTE", f"sección {num} no resolvió"))
        obs = [dict(p) for p in r["observations"]]
        for j in targets:
            v, rest = verdicts.get(j, ("SIN VEREDICTO", ""))
            v = "AUSENTE" if v == "SECCION" else v
            anchored = v == "PRESENTE" and check_evidence(rest, lines) == "anchored" and substantive(rest)
            obs[j]["verify"] = {"verdict": v, "detail": rest, "anchored": anchored}
            obs[j]["keep"] = not (anchored or v == "NO APLICA")
        for p in obs:
            p.setdefault("keep", True)
        if len(verdicts) < len(targets):
            # Sin checkpoint: una corrida posterior lo reintenta (un corte de red
            # a mitad de la corrida dejó 59 ítems sin veredicto en segundos).
            log(f"verificado {iid} ({k}/{len(todo)}): {len(targets) - len(verdicts)} sin veredicto, queda pendiente")
            continue
        done[iid] = {"title": r["title"], "observations": obs}
        state.write_text(json.dumps(done, ensure_ascii=False, indent=1))
        dropped = sum(not p["keep"] for p in obs)
        log(f"verificado {iid} ({k}/{len(todo)}): {dropped}/{len(targets)} descartadas")
    # Salida: los ítems sin verificar (sin ausencias, o fuera de --verify-ids) pasan enteros.
    done = {iid: done.get(iid) or {"title": r["title"], "observations": [dict(p, keep=True) for p in r["observations"]]}
            for iid, r in results.items()}
    (out / "observations.verified.json").write_text(json.dumps(done, ensure_ascii=False, indent=1))
    kept = [p for r in done.values() for p in r["observations"] if p["keep"]]
    md = [f"# Observaciones verificadas ({len(kept)})\n"]
    for iid, r in done.items():
        ks = [p for p in r["observations"] if p["keep"]]
        if ks:
            md.append(f"## {iid}. {r['title']}\n")
            md.extend(f"- [{p['check']}] {p['obs']}\n  - {p['evidence']}" for p in ks)
            md.append("")
    (out / "observations.verified.md").write_text("\n".join(md) + "\n")
    total = sum(len(r["observations"]) for r in done.values())
    log(f"verificación: {len(kept)}/{total} observaciones conservadas")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("document")
    ap.add_argument("items")
    ap.add_argument("outdir")
    ap.add_argument("--context", required=True,
                    help="qué se evalúa, para el prompt (p.ej. 'el capítulo 1 de una DIA ...')")
    ap.add_argument("--only", default="", help="ids de ítems separados por coma")
    ap.add_argument("--tools", action="store_true",
                    help="modo con grep/read_file (lento; ver Passages). Por defecto, recuperación sin tools")
    ap.add_argument("--budget", type=int, default=14000,
                    help="caracteres de pasajes por ítem en el modo sin tools (default 14000)")
    ap.add_argument("--verify", action="store_true",
                    help="segunda etapa: verificar en el documento completo las ausencias y citas sin ancla "
                         "(observations.verified.json); sin --tools")
    ap.add_argument("--verify-prompt", default="",
                    help="plantilla del verificador (default checklist-defense-prompt.md; "
                         "alternativa checklist-verify-prompt.md)")
    ap.add_argument("--verify-ids", default="", help="con --verify, limitar a estos ítems (prueba)")
    ap.add_argument("--prompt", default="")
    ap.add_argument("--model", default="")
    ap.add_argument("--ollama-url", default="")
    ap.add_argument("--braze", default=str(REPO / "target" / "release" / "braze"))
    args = ap.parse_args()
    extra = tuple((["--model", args.model] if args.model else [])
                  + (["--ollama-url", args.ollama_url] if args.ollama_url else []))

    out = Path(args.outdir)
    out.mkdir(parents=True, exist_ok=True)
    shutil.copy(args.document, out / "document.txt")
    lines = (out / "document.txt").read_text(encoding="utf-8").splitlines()
    items = json.loads(Path(args.items).read_text(encoding="utf-8"))
    only = {x.strip() for x in args.only.split(",") if x.strip()}
    prompt_path = args.prompt or str(HERE / ("checklist-prompt.md" if args.tools
                                              else "checklist-retrieval-prompt.md"))
    tpl = Path(prompt_path).read_text(encoding="utf-8")
    passages = None if args.tools else Passages(lines)
    # Ollama no manda bytes durante el prefill (ver run_review / night-judge).
    os.environ.setdefault("BRAZE_HTTP_READ_TIMEOUT_SECS", "1800")

    results_path = out / "observations.json"
    results = json.loads(results_path.read_text()) if results_path.exists() else {}
    for k, item in enumerate(items, 1):
        if only and item["id"] not in only:
            continue
        # Reanudar: un ítem ya respondido con formato no se repite (--only lo fuerza).
        if not only and results.get(item["id"], {}).get("status", "NONE") != "NONE":
            continue
        prompt = fill(tpl, CONTEXT=args.context, N=len(lines), TITLE=item["title"], TEXT=item["text"],
                      PASSAGES=passages.select(item["title"].split(" > ")[-1] + " " + item["text"],
                                               args.budget) if passages else "")
        # Como en el verificador: un reintento si la respuesta sale sin formato
        # (args de una tool call como texto, razonamiento filtrado).
        for attempt in range(2):
            text = run_braze(args.braze, out, f"item{item['id']}-{attempt}", prompt, extra=extra)
            pairs = parse(text)
            if pairs is not None:
                break
        status = "NONE" if pairs is None else (f"{len(pairs)} obs" if pairs else "sin observaciones")
        for p in pairs or []:
            p["check"] = check_evidence(p["evidence"], lines)
        results[item["id"]] = {"title": item["title"], "status": status, "observations": pairs or [],
                               "raw": text if pairs is None else ""}
        results_path.write_text(json.dumps(results, ensure_ascii=False, indent=1))
        log(f"ítem {item['id']} ({k}/{len(items)}): {status} — {item['title'][-70:]}")

    md = []
    total = 0
    for iid, r in results.items():
        md.append(f"## {iid}. {r['title']} — {r['status']}\n")
        for p in r["observations"]:
            total += 1
            md.append(f"- [{p['check']}] {p['obs']}\n  - {p['evidence']}")
        md.append("")
    (out / "observations.md").write_text(f"# Observaciones ({total})\n\n" + "\n".join(md) + "\n")
    log(f"total: {total} observaciones en {len(results)} ítems")
    if args.verify:
        verify(args, out, lines, items, results, passages or Passages(lines), extra)


if __name__ == "__main__":
    main()
