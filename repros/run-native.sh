#!/usr/bin/env bash
# NixOS runner for tests/tools in the current session. Defaults to ordinary
# tests; use run-headless.sh for the full graphical suite and virtual input.
# Pass Cargo arguments to select individual tests or examples.
set -euo pipefail
repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd -- "$repo_root"
if (($# == 0)); then
    set -- test --locked --tests -- --nocapture --test-threads=1
fi
printf -v cargo_command '%q ' cargo "$@"
native_setup='
    if [[ -f /run/opengl-driver/share/vulkan/icd.d/lvp_icd.x86_64.json ]]; then
        export VK_DRIVER_FILES=/run/opengl-driver/share/vulkan/icd.d/lvp_icd.x86_64.json
    fi
'
exec nix-shell "$repo_root/shell.nix" --run "$native_setup
exec $cargo_command"
