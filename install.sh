#!/usr/bin/env bash
set -euo pipefail
cargo install --path . --locked
echo "Installed lyrictty to ~/.cargo/bin/lyrictty"
