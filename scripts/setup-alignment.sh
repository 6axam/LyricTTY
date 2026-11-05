#!/usr/bin/env bash
set -euo pipefail
umask 077
model="${1:-base}"
# Old ru/en invocations select the same multilingual model.
case "$model" in ru|en) model=base ;; base) ;; *) printf 'Usage: %s [base]\n' "$0" >&2; exit 2 ;; esac
if (( $# > 1 )); then printf 'Usage: %s [base]\n' "$0" >&2; exit 2; fi
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
root="${XDG_DATA_HOME:-$HOME/.local/share}/lyrictty/alignment"
venv="$root/venv"
mkdir -p -- "$root/models/stable-ts"
if command -v uv >/dev/null 2>&1; then
    if [[ ! -x "$venv/bin/python" ]]; then uv venv --python 3.12 "$venv"; fi
    uv pip install --python "$venv/bin/python" --index-url https://download.pytorch.org/whl/cpu torch torchaudio
    uv pip install --python "$venv/bin/python" 'stable-ts==2.19.1'
else
    if [[ ! -x "$venv/bin/python" ]]; then python3 -m venv "$venv"; fi
    "$venv/bin/python" -m pip install --index-url https://download.pytorch.org/whl/cpu torch torchaudio
    "$venv/bin/python" -m pip install 'stable-ts==2.19.1'
fi
"$venv/bin/python" "$script_dir/align_lyrics.py" --setup-model --model "$model" --models-dir "$root/models/stable-ts"
printf 'Stable-ts CPU alignment ready: %s\n' "$root/models/stable-ts/$model.pt"
