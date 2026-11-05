#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
command -v cargo >/dev/null || { echo 'Rust/Cargo is required to build LyricTTY.' >&2; exit 1; }
cargo build --release --locked
lyrictty_prefix="${LYRICTTY_PREFIX:-$HOME/.local}"
install -Dm755 target/release/lyrictty "$lyrictty_prefix/bin/lyrictty"
printf 'Installed LyricTTY to %s/bin/lyrictty\n' "$lyrictty_prefix"
for dependency in mpv yt-dlp; do
  command -v "$dependency" >/dev/null || printf 'Optional native playback dependency missing: %s\n' "$dependency"
done
