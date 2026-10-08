#!/usr/bin/env bash
# Revisión confidencial de un manuscrito con un modelo local (perfil
# scripts/review/profile): notas incrementales a archivo, reseña final
# anclada y chequeo de anclaje con hasta N pasadas de reparación.
#
# Uso: run-review.sh MANUSCRITO CRITERIOS OUTDIR ["Nombre del journal"]
# Env: BRAZE_BIN (default target/release/braze), REVIEW_REPAIRS (2),
#      REVIEW_CHUNK (150), BRAZE_OLLAMA_MODEL para cambiar de modelo.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
manuscript="$1"; criteria="$2"; outdir="$3"; journal="${4:-the target journal}"
braze="${BRAZE_BIN:-$repo/target/release/braze}"
repairs="${REVIEW_REPAIRS:-2}"
chunk="${REVIEW_CHUNK:-150}"

mkdir -p "$outdir"
cp "$manuscript" "$outdir/manuscript.tex"
cp "$criteria" "$outdir/criteria.md"
cd "$outdir"
rm -f notes.md review.md

n_lines=$(wc -l < manuscript.tex)
prompt=$(sed \
  -e "s|{JOURNAL}|$journal|g" -e "s|{MANUSCRIPT}|manuscript.tex|g" \
  -e "s|{CRITERIA}|criteria.md|g" -e "s|{NOTES}|notes.md|g" \
  -e "s|{REVIEW}|review.md|g" -e "s|{N_LINES}|$n_lines|g" \
  -e "s|{CHUNK}|$chunk|g" -e "s|{CHUNK_PLUS_1}|$((chunk + 1))|g" \
  "$here/review-prompt.md")

run_braze() { # $1 = etiqueta del log, $2 = prompt
  XDG_CONFIG_HOME="$here/profile" RUST_LOG="${RUST_LOG:-braze_engine=warn}" \
    "$braze" run --output-format json "$2" > "$1.json" 2> "$1.log" || true
}

check() {
  python3 -I "$here/check_anchors.py" manuscript.tex review.md > check.txt 2>&1
}

echo "[review] pasada principal ($n_lines líneas, chunks de $chunk)"
run_braze pass0 "$prompt"

attempt=0
while :; do
  if [[ ! -s review.md ]]; then
    echo "FAIL: review.md was not written" > check.txt
  elif check; then
    echo "[review] PASS"; cat check.txt; exit 0
  fi
  cat check.txt
  (( attempt >= repairs )) && { echo "[review] sin pasar tras $repairs reparaciones"; exit 1; }
  attempt=$((attempt + 1))
  echo "[review] reparación $attempt"
  repair=$(cat <<EOF
You are repairing a peer review of manuscript.tex ($n_lines lines). Your notes from reading it are in notes.md; the review is review.md (it may be missing).
An automatic grounding check rejected it:

$(cat check.txt)

Fix it: read notes.md, and use read_file on manuscript.tex at the cited lines to confirm each anchor. Every numbered issue under "## Major issues" and "## Minor issues" needs an anchor L123 and an exact phrase from those lines in backticks; correct the anchor or the phrase, or delete the issue if the manuscript does not support it. If review.md is missing, write it from notes.md following the sections Summary, Strengths, Major issues, Minor issues, Recommendation. Write the complete corrected review.md with write_file, then reply: done.
EOF
)
  run_braze "repair$attempt" "$repair"
done
