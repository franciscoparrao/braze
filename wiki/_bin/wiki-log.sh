#!/usr/bin/env bash
# wiki-log.sh — registra una entrada en el log cronológico del wiki de ESTE proyecto
#
# Uso:
#   wiki-log.sh <op> <título> [detalle]
#
# Convención de entrada (greppeable, idéntica a ~/vault/log.md):
#   ## [YYYY-MM-DD HH:MM] <op> | <título>
#   - <detalle opcional>
#
# Ops sugeridas: decision | bug | patron | hito | infra | nota
#
# Consultas típicas:
#   grep "^## \[" log.md | tail -5           # últimas 5 entradas
#   grep "^## \[.*\] decision" log.md        # todas las decisiones
#   grep "^## \[2026-07" log.md              # todo julio 2026
#
# Se autolocaliza: escribe en log.md junto al directorio _bin/ que lo contiene,
# así funciona sin importar desde dónde se invoque ni en qué máquina/clon del
# repo esté corriendo.

set -euo pipefail

OP="${1:?uso: wiki-log.sh <op> <titulo> [detalle]}"
TITLE="${2:?uso: wiki-log.sh <op> <titulo> [detalle]}"
DETAIL="${3:-}"

BIN_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WIKI_DIR="$(dirname "$BIN_DIR")"
LOG="$WIKI_DIR/log.md"

ENTRY="## [$(date '+%Y-%m-%d %H:%M')] $OP | $TITLE"
if [[ -n "$DETAIL" ]]; then
    ENTRY="$ENTRY
- $DETAIL"
fi

printf "%s\n\n" "$ENTRY" | "$BIN_DIR/safe-append.sh" "$LOG" -
