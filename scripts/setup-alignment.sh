#!/usr/bin/env bash
set -euo pipefail
umask 077

language="${1:-ru}"
if (( $# > 1 )) || [[ "$language" != ru && "$language" != en ]]; then
    printf 'Usage: %s [ru|en]\n' "$0" >&2
    exit 2
fi

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
root="${XDG_DATA_HOME:-$HOME/.local/share}/lyrictty/alignment"
venv="$root/venv"
mkdir -p -- "$root/models"

if command -v uv >/dev/null 2>&1; then
    if [[ ! -x "$venv/bin/python" ]]; then
        uv venv --python 3.12 "$venv"
    fi
    uv pip install --python "$venv/bin/python" 'vosk>=0.3.45,<0.4'
else
    if [[ ! -x "$venv/bin/python" ]]; then
        python3 -m venv "$venv"
    fi
    "$venv/bin/python" -m pip install 'vosk>=0.3.45,<0.4'
fi

"$venv/bin/python" "$script_dir/align_lyrics.py" \
    --download-model "$language" --models-dir "$root/models"

case "$language" in
    ru) model=vosk-model-small-ru-0.22 ;;
    en) model=vosk-model-small-en-us-0.15 ;;
esac
printf 'Python: %s\nModel: %s\n' "$venv/bin/python" "$root/models/$model" >&2
printf '%s\n' \
    'Requires pactl and parec (PulseAudio or PipeWire PulseAudio compatibility).' \
    'Official small models need roughly 300 MB at runtime. Singing accuracy is limited; only credible acoustic matches are emitted.' >&2
