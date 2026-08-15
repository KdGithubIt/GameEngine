#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
GAME_ENGINE="$(cd -- "$SCRIPT_DIR/.." && pwd)"
cd "$GAME_ENGINE"
cargo run -p engine-editor --bin animation_motion_designer
