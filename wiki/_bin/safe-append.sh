#!/usr/bin/env bash
# safe-append.sh — append seguro a archivos del wiki desde sesiones concurrentes
#
# Uso:
#   safe-append.sh <archivo> <texto>
#   echo "texto multilinea" | safe-append.sh <archivo> -
#
# Usa flock con un .lock por archivo. Timeout 5 segundos. Si no consigue el
# lock, falla con código 75. (Idéntico en espíritu a ~/vault/_bin/safe-append.sh
# — copia autocontenida para que el wiki viaje con el repo sin depender del vault.)

set -euo pipefail

file="${1:?Falta archivo}"
text="${2:?Falta texto o '-' para stdin}"

mkdir -p "$(dirname "$file")"
file=$(realpath "$file")

lockfile="${file}.lock"

exec 200>"$lockfile"
if ! flock -w 5 200; then
    echo "safe-append: timeout esperando lock en $lockfile" >&2
    exit 75
fi

if [ "$text" = "-" ]; then
    cat >> "$file"
else
    printf "%s\n" "$text" >> "$file"
fi
