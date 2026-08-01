#!/usr/bin/env bash
# wiki-lint.sh — chequeos mecánicos de salud del wiki de un proyecto
#
# Uso: wiki-lint.sh [--verbose]  (se autolocaliza; correr desde donde sea)
#
# Detecta (capa mecánica; la capa semántica —¿está esto desactualizado?,
# ¿se contradice con otra página?— la hace el LLM al invocar /wiki grow):
#   1. Wikilinks rotos: [[target]] sin página correspondiente en paginas/
#   2. Páginas huérfanas: sin ningún link entrante
#   3. Infraestructura: index.md / log.md presentes, última entrada del log
#   4. Backlog de destilación: entradas de log posteriores a .last-distill
#   5. Conteo de páginas
#
# Salida: reporte en texto plano. Exit 0 siempre (los hallazgos no son errores).

set -euo pipefail

BIN_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WIKI="$(dirname "$BIN_DIR")"
VERBOSE="${1:-}"

cd "$WIKI"

mapfile -t PAGES < <(find paginas -name "*.md" 2>/dev/null | sort)

echo "=== WIKI LINT $(date '+%Y-%m-%d %H:%M') — $WIKI ==="
echo ""

# --- 1. Wikilinks rotos ------------------------------------------------------
declare -A BASENAMES
for p in "${PAGES[@]}"; do
    BASENAMES["$(basename "$p" .md)"]=1
done

BROKEN=0
BROKEN_LIST=""
while IFS= read -r line; do
    file="${line%%:\[\[*}"
    link="${line#*:\[\[}"; link="${link%]]}"
    target="${link%%|*}"; target="${target%%#*}"
    target="$(echo "$target" | sed 's/^ *//; s/ *$//')"
    [[ -z "$target" ]] && continue
    if [[ -z "${BASENAMES[$target]:-}" ]]; then
        BROKEN=$((BROKEN+1))
        BROKEN_LIST+="  $file → [[$target]]"$'\n'
    fi
done < <(grep -ro --include="*.md" '\[\[[^]]*\]\]' paginas index.md 2>/dev/null | sort -u || true)

echo "1. WIKILINKS ROTOS: $BROKEN"
[[ $BROKEN -gt 0 ]] && printf "%s" "$BROKEN_LIST"
echo ""

# --- 2. Páginas huérfanas -----------------------------------------------------
ORPHANS=0
ORPHAN_LIST=""
for p in "${PAGES[@]}"; do
    b="$(basename "$p" .md)"
    if ! grep -rql --include="*.md" -F "[[$b" paginas index.md --exclude="$(basename "$p")" 2>/dev/null; then
        ORPHANS=$((ORPHANS+1))
        ORPHAN_LIST+="  $p"$'\n'
    fi
done

echo "2. PÁGINAS HUÉRFANAS (sin links entrantes): $ORPHANS"
[[ $ORPHANS -gt 0 && ( "$VERBOSE" == "--verbose" || $ORPHANS -le 15 ) ]] && printf "%s" "$ORPHAN_LIST"
echo ""

# --- 3. Infraestructura -------------------------------------------------------
echo "3. INFRAESTRUCTURA:"
[[ -f index.md ]] && echo "  index.md: OK" || echo "  index.md: FALTA"
if [[ -f log.md ]]; then
    LAST="$(grep '^## \[' log.md | tail -1 || echo 'sin entradas')"
    echo "  log.md: OK — última entrada: $LAST"
else
    echo "  log.md: FALTA"
fi
echo ""

# --- 4. Backlog de destilación -------------------------------------------------
CHECKPOINT="1970-01-01 00:00"
[[ -f .last-distill ]] && CHECKPOINT="$(cat .last-distill)"
PENDING=0
if [[ -f log.md ]]; then
    PENDING=$(grep -oE '^## \[[0-9]{4}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2}\]' log.md \
        | sed -E 's/^## \[(.*)\]/\1/' \
        | awk -v cp="$CHECKPOINT" '$0 > cp' | wc -l)
fi
echo "4. BACKLOG DE DESTILACIÓN: $PENDING entradas sin procesar desde \"$CHECKPOINT\""
[[ $PENDING -ge 10 ]] && echo "   -> considerar correr /wiki grow"
echo ""

# --- 5. Conteos ----------------------------------------------------------------
echo "5. PÁGINAS: ${#PAGES[@]}"
echo ""
echo "=== FIN LINT (capa mecánica) — la capa semántica corre en /wiki grow ==="
