#!/usr/bin/env python3
"""wiki-html.py — renderiza wiki/index.md + wiki/paginas/*.md a un sitio HTML
estatico y navegable en wiki/_site/, resolviendo [[wikilinks]] a hrefs reales
entre las paginas generadas.

Pandoc no entiende la sintaxis [[...]] de forma nativa (no es Markdown
estandar) — por eso este script preprocesa cada pagina antes de invocar
pandoc, en vez de correr `pandoc *.md` directo.

Uso:
    python3 wiki-html.py [--open]

Requiere `pandoc` en PATH. Se autolocaliza: escribe wiki/_site/ relativo al
propio directorio wiki/ (_bin/..), sin importar desde donde se invoque.
Exit 0 siempre que el render se complete, incluso con wikilinks rotos
(se reportan, no bloquean el build — la fuente de verdad es el markdown).
"""
from __future__ import annotations

import html
import re
import shutil
import subprocess
import sys
import unicodedata
import webbrowser
from pathlib import Path

WIKILINK_RE = re.compile(r"\[\[([^\]]+)\]\]")
FRONTMATTER_RE = re.compile(r"^---\n(.*?)\n---\n", re.S)


def parse_frontmatter(text: str) -> tuple[dict, str]:
    m = FRONTMATTER_RE.match(text)
    if not m:
        return {}, text
    meta: dict[str, str] = {}
    for line in m.group(1).splitlines():
        if ":" in line:
            k, _, v = line.partition(":")
            meta[k.strip()] = v.strip()
    return meta, text[m.end():]


def extract_title(body: str, fallback: str) -> str:
    for line in body.splitlines():
        line = line.strip()
        if line.startswith("# "):
            return line[2:].strip()
    return fallback


def slugify(title: str) -> str:
    """ASCII-only, filesystem/URL-safe slug. Allowlist (keep only
    alphanumerics/whitespace/hyphens after stripping accents), not a
    blocklist of specific characters to remove — a blocklist inevitably
    misses something (parens, math symbols like `≈`, smart quotes, ...)
    that a real page title ends up using. See wiki/paginas/*.md titles
    for examples this must survive cleanly."""
    s = title.strip().lower()
    s = unicodedata.normalize("NFKD", s)
    s = "".join(c for c in s if not unicodedata.combining(c))
    s = re.sub(r"[^a-z0-9\s-]", "", s)
    s = re.sub(r"[\s_-]+", "-", s)
    s = s.strip("-")
    return s or "pagina"


def discover_pages(wiki_dir: Path) -> list[dict]:
    """Cada página: basename (para resolver wikilinks, MISMO criterio que
    wiki-lint.sh: matchea por nombre de archivo, no por el H1), title (para
    mostrar), meta (frontmatter), body (markdown sin frontmatter)."""
    pages = []
    index_path = wiki_dir / "index.md"
    if index_path.exists():
        text = index_path.read_text(encoding="utf-8")
        meta, body = parse_frontmatter(text)
        pages.append({
            "path": index_path, "basename": "index",
            "title": extract_title(body, "Inicio"),
            "meta": meta, "body": body, "is_index": True,
        })
    paginas_dir = wiki_dir / "paginas"
    if paginas_dir.is_dir():
        for p in sorted(paginas_dir.glob("*.md")):
            text = p.read_text(encoding="utf-8")
            meta, body = parse_frontmatter(text)
            pages.append({
                "path": p, "basename": p.stem,
                "title": extract_title(body, p.stem),
                "meta": meta, "body": body, "is_index": False,
            })
    return pages


def build_slug_map(pages: list[dict]) -> dict[str, str]:
    """basename -> slug. El slug sale del título (URLs legibles); la
    resolución de wikilinks usa el basename (contrato de wiki-lint.sh)."""
    used: dict[str, str] = {}
    mapping: dict[str, str] = {}
    for pg in pages:
        base_slug = "index" if pg["is_index"] else slugify(pg["title"])
        slug = base_slug
        n = 2
        while slug in used and used[slug] != pg["basename"]:
            slug = f"{base_slug}-{n}"
            n += 1
        used[slug] = pg["basename"]
        mapping[pg["basename"]] = slug
    return mapping


def resolve_wikilinks(text: str, slug_map: dict[str, str]) -> tuple[str, list[str]]:
    broken: list[str] = []

    def repl(m: re.Match) -> str:
        inner = m.group(1)
        target, _, alias = inner.partition("|")
        target = target.split("#")[0].strip()
        label = (alias or target or inner).strip()
        if target in slug_map:
            return f"[{label}]({slug_map[target]}.html)"
        broken.append(target)
        return (
            f'<span class="broken-wikilink" '
            f'title="página no existe: {html.escape(target)}">'
            f'{html.escape(label)}</span>'
        )

    return WIKILINK_RE.sub(repl, text), broken


def markdown_to_html_fragment(md_text: str) -> str:
    result = subprocess.run(
        ["pandoc", "--from=markdown", "--to=html5", "--highlight-style=kate"],
        input=md_text, capture_output=True, text=True, encoding="utf-8",
    )
    if result.returncode != 0:
        print(f"aviso: pandoc devolvió error, se usa fallback sin formato: {result.stderr}", file=sys.stderr)
        return f"<pre>{html.escape(md_text)}</pre>"
    return result.stdout


PAGE_SHELL = """<!DOCTYPE html>
<html lang="es" data-theme="auto">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{page_title}</title>
<style>
:root {{
  --paper: oklch(0.972 0.008 85); --paper-deep: oklch(0.945 0.012 85);
  --ink: oklch(0.24 0.015 60); --ink-soft: oklch(0.42 0.02 60); --ink-faint: oklch(0.56 0.015 60);
  --copper: oklch(0.52 0.13 45); --copper-hot: oklch(0.60 0.16 40);
  --rule: oklch(0.88 0.015 80); --code-bg: oklch(0.955 0.01 85);
  --code-edge: oklch(0.52 0.13 45 / .55); --sidebar-bg: oklch(0.945 0.012 80);
  --broken: oklch(0.55 0.16 25);
  --serif: "Iowan Old Style","Palatino Linotype",Palatino,"Book Antiqua",Georgia,serif;
  --sans: "Seravek","Gill Sans Nova",Ubuntu,Calibri,"DejaVu Sans",source-sans-pro,sans-serif;
  --mono: "JetBrains Mono","Fira Code","Cascadia Code","DejaVu Sans Mono",ui-monospace,monospace;
}}
[data-theme="dark"] {{
  --paper: oklch(0.215 0.012 55); --paper-deep: oklch(0.185 0.012 55);
  --ink: oklch(0.90 0.012 80); --ink-soft: oklch(0.76 0.015 75); --ink-faint: oklch(0.60 0.015 70);
  --copper: oklch(0.72 0.13 50); --copper-hot: oklch(0.78 0.15 45);
  --rule: oklch(0.32 0.015 55); --code-bg: oklch(0.255 0.014 55);
  --code-edge: oklch(0.72 0.13 50 / .5); --sidebar-bg: oklch(0.19 0.012 55);
  --broken: oklch(0.68 0.16 25);
}}
@media (prefers-color-scheme: dark) {{
  [data-theme="auto"] {{
    --paper: oklch(0.215 0.012 55); --paper-deep: oklch(0.185 0.012 55);
    --ink: oklch(0.90 0.012 80); --ink-soft: oklch(0.76 0.015 75); --ink-faint: oklch(0.60 0.015 70);
    --copper: oklch(0.72 0.13 50); --copper-hot: oklch(0.78 0.15 45);
    --rule: oklch(0.32 0.015 55); --code-bg: oklch(0.255 0.014 55);
    --code-edge: oklch(0.72 0.13 50 / .5); --sidebar-bg: oklch(0.19 0.012 55);
    --broken: oklch(0.68 0.16 25);
  }}
}}
* {{ box-sizing: border-box; }}
body {{
  margin: 0; background: var(--paper); color: var(--ink); font-family: var(--sans);
  line-height: 1.7; display: grid; grid-template-columns: 280px minmax(0,1fr); min-height: 100vh;
}}
#sidebar {{
  background: var(--sidebar-bg); border-right: 1px solid var(--rule);
  padding: 1.6rem 0 2.5rem; position: sticky; top: 0; height: 100vh; overflow-y: auto;
}}
#sidebar .brand {{ padding: 0 1.3rem 1rem; border-bottom: 1px solid var(--rule); margin-bottom: .8rem; }}
#sidebar .brand a {{ font-family: var(--serif); font-size: 1.05rem; font-weight: 700; color: var(--ink); text-decoration: none; }}
#sidebar .brand small {{ display: block; font-size: .68rem; color: var(--ink-faint); text-transform: uppercase; letter-spacing: .12em; margin-top: .3rem; }}
#theme-btn {{
  margin-top: .7rem; font: 600 .68rem var(--sans); letter-spacing: .07em; color: var(--ink-soft);
  background: none; border: 1px solid var(--rule); border-radius: 99px; padding: .25rem .7rem; cursor: pointer;
}}
#theme-btn:hover {{ color: var(--copper-hot); border-color: var(--copper-hot); }}
#search {{
  width: calc(100% - 2rem); margin: .9rem 1rem .3rem; padding: .4rem .6rem;
  border: 1px solid var(--rule); border-radius: 6px; background: var(--paper);
  color: var(--ink); font-family: var(--sans); font-size: .82rem;
}}
#pagelist {{ list-style: none; margin: .4rem 0 0; padding: 0; }}
#pagelist li a {{
  display: block; padding: .3rem 1.3rem; color: var(--ink-soft); text-decoration: none; font-size: .86rem;
  border-left: 3px solid transparent;
}}
#pagelist li a:hover {{ color: var(--copper-hot); }}
#pagelist li a.current {{ color: var(--copper); border-left-color: var(--copper); background: color-mix(in oklch, var(--copper) 7%, transparent); font-weight: 600; }}
main {{ min-width: 0; padding: 0 clamp(1.5rem,5vw,3.5rem) 5rem; max-width: 78ch; }}
.breadcrumb {{ padding-top: 2.2rem; font-size: .8rem; color: var(--ink-faint); }}
.breadcrumb a {{ color: var(--ink-faint); }}
article h1 {{ font-family: var(--serif); font-size: clamp(1.6rem,3vw,2.1rem); margin: .4rem 0 1.4rem; border-bottom: 1px solid var(--rule); padding-bottom: .8rem; }}
article h2 {{ font-family: var(--serif); font-size: 1.2rem; margin-top: 2.1rem; }}
article h3 {{ font-size: 1rem; margin-top: 1.6rem; }}
article a {{ color: var(--copper); text-decoration-color: color-mix(in oklch, var(--copper) 40%, transparent); }}
article a:hover {{ color: var(--copper-hot); }}
.broken-wikilink {{ color: var(--broken); border-bottom: 1px dashed var(--broken); cursor: help; }}
code {{ font-family: var(--mono); font-size: .84em; background: var(--code-bg); border: 1px solid var(--rule); border-radius: 4px; padding: .08em .38em; color: var(--copper); }}
pre {{ background: var(--code-bg); border: 1px solid var(--rule); border-left: 4px solid var(--code-edge); border-radius: 6px; padding: 1rem 1.2rem; overflow-x: auto; font-size: .85rem; }}
pre code {{ background: none; border: none; padding: 0; color: var(--ink); }}
pre .kw, pre .cf {{ color: var(--copper); font-weight: 600; }}
pre .dt {{ color: oklch(0.52 0.10 250); }}
pre .st, pre .ss {{ color: oklch(0.52 0.10 145); }}
pre .co, pre .do {{ color: var(--ink-faint); font-style: italic; }}
pre .fu {{ color: oklch(0.50 0.11 300); }}
pre .dv, pre .bn, pre .fl {{ color: oklch(0.55 0.12 65); }}
[data-theme="dark"] pre .dt {{ color: oklch(0.75 0.10 250); }}
[data-theme="dark"] pre .st, [data-theme="dark"] pre .ss {{ color: oklch(0.76 0.11 145); }}
[data-theme="dark"] pre .fu {{ color: oklch(0.75 0.11 300); }}
[data-theme="dark"] pre .dv, [data-theme="dark"] pre .bn {{ color: oklch(0.78 0.12 70); }}
table {{ border-collapse: collapse; width: 100%; font-size: .88rem; margin: 1.2rem 0; }}
thead th {{ text-align: left; padding: .5rem .8rem; border-bottom: 2px solid var(--copper); font-size: .74rem; text-transform: uppercase; letter-spacing: .08em; color: var(--ink-faint); }}
tbody td {{ padding: .5rem .8rem; border-bottom: 1px solid var(--rule); }}
footer.page-footer {{ margin-top: 3rem; padding-top: 1rem; border-top: 1px solid var(--rule); font-size: .78rem; color: var(--ink-faint); }}
</style>
</head>
<body>
<aside id="sidebar">
  <div class="brand">
    <a href="index.html">{site_title}</a>
    <small>{n_pages} páginas</small>
    <div><button id="theme-btn" title="Alternar tema">TEMA: AUTO</button></div>
  </div>
  <input id="search" type="text" placeholder="Buscar página…" autocomplete="off">
  <ul id="pagelist">
{pagelist_html}
  </ul>
</aside>
<main>
  <div class="breadcrumb"><a href="index.html">{site_title}</a> {breadcrumb_sep}</div>
  <article>
{body_html}
  </article>
  <footer class="page-footer">{footer_text}</footer>
</main>
<script>
(function () {{
  var root = document.documentElement, btn = document.getElementById('theme-btn');
  var order = ['auto', 'light', 'dark'];
  var saved = localStorage.getItem('wiki-theme') || 'auto';
  apply(saved);
  btn.addEventListener('click', function () {{
    var next = order[(order.indexOf(root.dataset.theme) + 1) % order.length];
    localStorage.setItem('wiki-theme', next);
    apply(next);
  }});
  function apply(t) {{ root.dataset.theme = t; btn.textContent = 'TEMA: ' + t.toUpperCase(); }}

  var search = document.getElementById('search');
  var items = Array.prototype.slice.call(document.querySelectorAll('#pagelist li'));
  search.addEventListener('input', function () {{
    var q = search.value.trim().toLowerCase();
    items.forEach(function (li) {{
      li.style.display = li.textContent.toLowerCase().indexOf(q) === -1 ? 'none' : '';
    }});
  }});
}})();
</script>
</body>
</html>
"""


def render_site(wiki_dir: Path) -> tuple[int, list[str]]:
    pages = discover_pages(wiki_dir)
    if not pages:
        return 0, []
    slug_map = build_slug_map(pages)
    # Única fuente de verdad para el nombre del sitio: el título real de
    # index.md (lo que puso /wiki init o lo que el usuario haya editado ahí)
    # — nunca se re-deriva del nombre del directorio, para no divergir.
    index_pg = next((p for p in pages if p["is_index"]), None)
    site_title = index_pg["title"] if index_pg else wiki_dir.parent.name

    site_dir = wiki_dir / "_site"
    site_dir.mkdir(exist_ok=True)

    all_broken: list[str] = []
    sorted_pages = sorted(pages, key=lambda p: (not p["is_index"], p["title"].lower()))

    pagelist_items = []
    for pg in sorted_pages:
        slug = slug_map[pg["basename"]]
        label = "🏠 " + pg["title"] if pg["is_index"] else pg["title"]
        pagelist_items.append((slug, label))

    for pg in pages:
        slug = slug_map[pg["basename"]]
        resolved_md, broken = resolve_wikilinks(pg["body"], slug_map)
        all_broken.extend(f'{pg["basename"]} → [[{b}]]' for b in broken)
        body_html = markdown_to_html_fragment(resolved_md)

        pagelist_html = "\n".join(
            f'    <li><a href="{s}.html"{" class=\"current\"" if s == slug else ""}>{html.escape(lbl)}</a></li>'
            for s, lbl in pagelist_items
        )
        created = pg["meta"].get("created", "")
        footer_text = f"Fuente: wiki/{'index.md' if pg['is_index'] else 'paginas/' + pg['path'].name}"
        if created:
            footer_text += f" · creada {created}"

        page_title = html.escape(site_title) if pg["is_index"] else f'{html.escape(pg["title"])} — {html.escape(site_title)}'
        html_out = PAGE_SHELL.format(
            page_title=page_title,
            site_title=html.escape(site_title),
            n_pages=len(pages),
            pagelist_html=pagelist_html,
            breadcrumb_sep="" if pg["is_index"] else f"› {html.escape(pg['title'])}",
            body_html=body_html,
            footer_text=footer_text,
        )
        (site_dir / f"{slug}.html").write_text(html_out, encoding="utf-8")

    return len(pages), all_broken


def main() -> int:
    open_browser = "--open" in sys.argv
    bin_dir = Path(__file__).resolve().parent
    wiki_dir = bin_dir.parent

    if shutil.which("pandoc") is None:
        print("error: pandoc no está en PATH — requerido para /wiki html", file=sys.stderr)
        return 1

    n, broken = render_site(wiki_dir)
    if n == 0:
        print("wiki-html: no hay páginas (¿corriste /wiki init?)")
        return 0

    index_html = wiki_dir / "_site" / "index.html"
    print(f"wiki-html: {n} páginas renderizadas -> {wiki_dir / '_site'}")
    if broken:
        print(f"wiki-html: {len(broken)} wikilinks rotos (marcados en el sitio, no bloquean el build):")
        for b in broken:
            print(f"  {b}")
    print(f"Abrir: {index_html}")

    if open_browser:
        try:
            webbrowser.open(f"file://{index_html}")
        except Exception:
            print("(no se pudo abrir el navegador automáticamente — abrí el path de arriba a mano)")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
