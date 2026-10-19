#!/usr/bin/env bash
# Run native tests on a private compositor, with no desktop or hardware input.
# Usage: bash repros/run-headless.sh [cargo arguments...]
set -euo pipefail
# Request no core dumps for intentionally aborting negative regressions.
ulimit -c 0
repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
if [[ ${1:-} != --inside ]]; then
    printf -v command '%q ' bash "$repo_root/repros/run-headless.sh" --inside "$@"
    exec nix-shell "$repo_root/shell.nix" --arg withHeadless true --run "$command"
fi
shift
cd -- "$repo_root"
export XDG_RUNTIME_DIR
XDG_RUNTIME_DIR=$(mktemp -d /tmp/wayapp-test-runtime.XXXXXX)
# WAYLAND_SOCKET takes precedence over WAYLAND_DISPLAY in libwayland clients.
# Never let an inherited socket connect tests to the user's desktop instead.
unset WAYLAND_DISPLAY WAYLAND_SOCKET SWAYSOCK DISPLAY
env WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 WLR_RENDERER=pixman \
    WLR_LIBINPUT_NO_DEVICES=1 sway --config "$repo_root/repros/sway-test.conf" \
    >"$XDG_RUNTIME_DIR/compositor.log" 2>&1 &
compositor_pid=$!
cleanup() {
    kill "$compositor_pid" 2>/dev/null || true
    wait "$compositor_pid" 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
deadline=$((SECONDS + 10))
while [[ -z ${WAYLAND_DISPLAY:-} ]]; do
    for socket in "$XDG_RUNTIME_DIR"/wayland-*; do
        if [[ -S $socket ]]; then
            export WAYLAND_DISPLAY=${socket##*/}
            break
        fi
    done
    if ! kill -0 "$compositor_pid" 2>/dev/null || ((SECONDS >= deadline)); then
        cat "$XDG_RUNTIME_DIR/compositor.log" >&2
        exit 1
    fi
    [[ -n ${WAYLAND_DISPLAY:-} ]] || sleep 0.05
done
printf 'Headless compositor log: %s/compositor.log\n' "$XDG_RUNTIME_DIR"
export WAYAPP_HEADLESS_TEST=1
# Instance creation may enumerate GLES even when Vulkan is selected.
export LIBGL_ALWAYS_SOFTWARE=1
if [[ -f /run/opengl-driver/share/vulkan/icd.d/lvp_icd.x86_64.json ]]; then
    export VK_DRIVER_FILES=/run/opengl-driver/share/vulkan/icd.d/lvp_icd.x86_64.json
fi
if (($# == 0)); then
    cargo test --locked --all-targets --all-features -- --include-ignored --test-threads=1
    # --all-targets excludes rustdoc tests.
    cargo test --locked --doc --all-features
else
    cargo "$@"
fi
