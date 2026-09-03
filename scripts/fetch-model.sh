#!/bin/zsh
# Fetch the embedding model the daemon compiles in (rust-embed over
# crates/daemon/models/potion-base-8M). Pinned to one HF revision and verified
# by sha256, so the binary never fetches anything at runtime and a tampered
# download can't build. minishlab/potion-base-8M, MIT.
set -e
set -o pipefail
DIR="$(cd "$(dirname "$0")/.." && pwd)/crates/daemon/models/potion-base-8M"
REV="bf8b056651a2c21b8d2565580b8569da283cab23"
BASE="https://huggingface.co/minishlab/potion-base-8M/resolve/$REV"
typeset -A SHA
SHA[model.safetensors]="f65d0f325faadc1e121c319e2faa41170d3fa07d8c89abd48ca5358d9a223de2"
SHA[tokenizer.json]="e67e803f624fb4d67dea1c730d06e1067e1b14d830e2c2202569e3ef0f70bb50"
SHA[config.json]="2a6ac0e9aaa356a68a5688070db78fc3a464fefe85d2f06a1905ce3718687553"
mkdir -p "$DIR"
for f in model.safetensors tokenizer.json config.json; do
  if [[ -f "$DIR/$f" ]] && [[ "$(shasum -a 256 "$DIR/$f" | cut -d' ' -f1)" == "${SHA[$f]}" ]]; then
    continue
  fi
  echo "→ fetching $f"
  curl -sSL --fail -o "$DIR/$f.part" "$BASE/$f"
  got=$(shasum -a 256 "$DIR/$f.part" | cut -d' ' -f1)
  if [[ "$got" != "${SHA[$f]}" ]]; then
    rm -f "$DIR/$f.part"
    echo "✗ $f: sha256 mismatch (got $got)"; exit 1
  fi
  mv "$DIR/$f.part" "$DIR/$f"
done
echo "✓ model ready: $DIR"
