#!/usr/bin/env bash
# Install CDeckJ for this user: the app, auto-launch when the controller is plugged in,
# the applications-menu entry with its icon, and Steam library artwork.
#
#   ./install.sh               install or update (or double-click it and choose Execute)
#   ./install.sh --uninstall   remove (settings, cues, history and recordings are kept)
#
# No root or password needed. Run it again after downloading a new version to update.
set -euo pipefail

# Double-clicked in the file manager (no terminal): reopen in a terminal window so you can
# see what happened.
if [ ! -t 1 ] && [ -z "${CDECKJ_IN_TERMINAL:-}" ] && command -v konsole >/dev/null 2>&1; then
    export CDECKJ_IN_TERMINAL=1
    exec konsole --hold -e "$0" "$@"
fi

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
src="$here/cdeckj"
default_dir="${CDECKJ_DIR:-$HOME/Applications}"

say() { printf '\033[1;36m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!!\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31mxx\033[0m %s\n' "$*" >&2; exit 1; }

# Where the app lives. If CDeckJ is already a Steam shortcut, keep using the file the
# shortcut points at, so Steam keeps working without editing the shortcut.
steam_target() {
    local vdf
    for vdf in "$HOME"/.local/share/Steam/userdata/*/config/shortcuts.vdf "$HOME"/.steam/steam/userdata/*/config/shortcuts.vdf; do
        [ -f "$vdf" ] || continue
        grep -aoE '"[^"]*/cdeckj"' "$vdf" 2>/dev/null | head -n1 | tr -d '"' && return 0
    done
    return 1
}

target="$(steam_target || true)"
if [ -n "$target" ] && [ "$(dirname "$target")" != "$here" ] && [ ! -d "$(dirname "$target")" ]; then
    target=""
fi
[ -n "$target" ] || target="$default_dir/cdeckj"

if [ "${1:-}" = "--uninstall" ]; then
    app=""
    for f in "$target" "$default_dir/cdeckj" "$src"; do
        if [ -x "$f" ]; then app="$f"; break; fi
    done
    [ -n "$app" ] || die "Couldn't find cdeckj to run the uninstall steps."
    say "Turning off auto-launch"
    "$app" --uninstall-autolaunch || warn "auto-launch wasn't set up"
    say "Removing the menu entry and icon"
    "$app" --uninstall-desktop || true
    # The copy this script installs; never the one in the download folder.
    if [ -f "$default_dir/cdeckj" ] && [ "$(readlink -f "$default_dir/cdeckj")" != "$(readlink -f "$src" 2>/dev/null || true)" ]; then
        rm -f "$default_dir/cdeckj" "$default_dir/cdeckj.png"
        say "Removed $default_dir/cdeckj"
    fi
    echo
    echo "CDeckJ is uninstalled. Your settings, cues and history (~/.config/cdeckj, ~/.local/share/cdeckj)"
    echo "and recordings (~/Music/CDeckJ Recordings) are still there."
    if steam_target >/dev/null; then
        echo "Remove CDeckJ from your Steam library too."
        if [ "$target" != "$default_dir/cdeckj" ] && [ -f "$target" ]; then
            echo "Its Steam shortcut runs $target, which is left in place: delete it if you like."
        fi
    fi
    exit 0
fi

[ -f "$src" ] || die "cdeckj isn't next to this script. Extract the whole download, then run install.sh from that folder."

# Copy the app (unless it's already running from where it should live). Write to a
# temporary file and rename, so this works even while CDeckJ or its watcher is running.
if [ "$(readlink -f "$src")" != "$(readlink -f "$target" 2>/dev/null || echo "$target")" ]; then
    say "Installing CDeckJ to $target"
    mkdir -p "$(dirname "$target")"
    tmp="$(mktemp "$(dirname "$target")/.cdeckj.XXXXXX")"
    cp "$src" "$tmp"
    chmod 755 "$tmp"
    mv -f "$tmp" "$target"
else
    chmod 755 "$target"
fi
out=""
[ -f "$here/cdeckj.png" ] && cp -f "$here/cdeckj.png" "$(dirname "$target")/cdeckj.png" 2>/dev/null || true

say "Setting up auto-launch, the menu entry and Steam artwork"
if ! out="$("$target" --install-autolaunch 2>&1)"; then
    printf '%s\n' "$out" >&2
    warn "Auto-launch couldn't be set up (see above). Adding the menu entry on its own."
    "$target" --install-desktop
fi

echo
if steam_target >/dev/null; then
    echo "Done. CDeckJ opens when you plug in the controller, in Game Mode and Desktop Mode."
    case "$out" in *artwork*) echo "Restart Steam once to see its new library artwork." ;; esac
else
    echo "Done. CDeckJ is in the applications menu and opens when you plug in the controller."
    echo
    echo "For Game Mode: in Steam choose Games > Add a Non-Steam Game to My Library, tick CDeckJ"
    echo "and press Add Selected Programs. CDeckJ adds its library artwork the next time it starts."
fi
