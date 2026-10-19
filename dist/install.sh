#!/bin/sh
# Installs the tree this script lives in.
#
#   ./install.sh                  install into $HOME/.local (no root)
#   PREFIX=/usr ./install.sh      install system-wide (sudo)
#   ./install.sh --uninstall      remove exactly what install put there
#   PREFIX=/usr ./install.sh --uninstall
#
# Upgrade is the same as install: run it again and the files are replaced.
# Your configuration (~/.config/keyvibes) and your own packs
# (~/.local/share/keyvibes/packs) are never touched by either operation.
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PREFIX="${PREFIX:-$HOME/.local}"
BINDIR="$PREFIX/bin"
DATADIR="$PREFIX/share"
SYSTEMD_USER_DIR="$PREFIX/lib/systemd/user"
PACKDIR="$DATADIR/keyvibes/packs"
DOCDIR="$DATADIR/doc/keyvibes"
RULE_SRC="$here/lib/udev/rules.d/70-keyvibes-uaccess.rules"
RULE_DST="$PREFIX/lib/udev/rules.d/70-keyvibes-uaccess.rules"

# udev reload: never fatal. Missing udevadm (containers, chroots) or a
# reload failure must not fail an otherwise successful install/uninstall.
reload_udev() {
    command -v udevadm >/dev/null 2>&1 || return 0
    udevadm control --reload-rules 2>/dev/null || true
    udevadm trigger --subsystem-match=input 2>/dev/null || true
}

# ------------------------------------------------------------------ uninstall
if [ "${1:-}" = "--uninstall" ] || [ "${1:-}" = "uninstall" ]; then
    removed=0
    for f in \
        "$BINDIR/keyvibes" \
        "$BINDIR/keyvibes-ui" \
        "$DATADIR/applications/keyvibes-ui.desktop" \
        "$DATADIR/icons/hicolor/scalable/apps/keyvibes.svg" \
        "$SYSTEMD_USER_DIR/keyvibes.service"; do
        if [ -e "$f" ]; then
            rm -f "$f"
            echo "  removed $f"
            removed=$((removed + 1))
        fi
    done
    # Only the udev rule inside this PREFIX - and only if it is byte-for-byte
    # the one this tree ships. An admin-edited or distro-owned rule is not
    # ours to delete, so cmp keeps us from removing someone else's file.
    if [ -f "$RULE_DST" ] && [ -f "$RULE_SRC" ] && cmp -s "$RULE_SRC" "$RULE_DST"; then
        rm -f "$RULE_DST"
        echo "  removed $RULE_DST"
        removed=$((removed + 1))
        reload_udev
    fi
    if [ -d "$PACKDIR" ]; then
        rm -rf "$PACKDIR"
        echo "  removed $PACKDIR"
        removed=$((removed + 1))
    fi
    if [ -d "$DOCDIR" ]; then
        rm -rf "$DOCDIR"
        echo "  removed $DOCDIR"
        removed=$((removed + 1))
    fi
    # Directories we may have created, but only while empty - and never
    # $PREFIX itself or $DATADIR: those hold other people's files.
    rmdir "$SYSTEMD_USER_DIR" "$PREFIX/lib/systemd" \
        "$PREFIX/lib/udev/rules.d" "$PREFIX/lib/udev" \
        "$DATADIR/applications" "$DATADIR/icons/hicolor/scalable/apps" \
        "$DATADIR/icons/hicolor/scalable" "$DATADIR/icons/hicolor" \
        "$DATADIR/icons" 2>/dev/null || true
    echo
    if [ "$removed" -eq 0 ]; then
        echo "Nothing to remove under $PREFIX - KeyVibes does not appear to be installed there."
    else
        echo "Removed KeyVibes from $PREFIX."
    fi
    echo "Left alone: your configuration (typically ~/.config/keyvibes)"
    echo "and any packs you added to ~/.local/share/keyvibes/packs."
    echo "Remove them yourself if you want a truly clean slate:"
    echo "  rm -rf \"\${XDG_CONFIG_HOME:-\$HOME/.config}/keyvibes\""
    echo "  rm -rf \"\${XDG_DATA_HOME:-\$HOME/.local/share}/keyvibes\""
    exit 0
fi

# ---------------------------------------------------------------- prerequisite
# A missing PipeWire session is the one prerequisite that cannot be fixed by
# this script: it is a host service. Say so before copying anything, so the
# user is not surprised after install.
if [ -z "${PIPEWIRE_RUNTIME_DIR:-}${XDG_RUNTIME_DIR:-}" ]; then
    echo "note: no XDG_RUNTIME_DIR set - PipeWire (your desktop audio session)"
    echo "      is probably not running. KeyVibes will install, but it cannot"
    echo "      make sound until you log into a desktop session."
    echo "      Run 'keyvibes doctor' after install for a full diagnosis."
    echo
fi

# --------------------------------------------------------------------- install
mkdir -p "$BINDIR" "$DATADIR" "$SYSTEMD_USER_DIR"
cp -a "$here/bin/." "$BINDIR/"
cp -a "$here/share/." "$DATADIR/"
if [ -d "$here/lib/systemd/user" ]; then
    cp -a "$here/lib/systemd/user/." "$SYSTEMD_USER_DIR/"
fi

# udev rule: only meaningful for a system PREFIX, where the running udev
# actually reads /usr/lib/udev/rules.d or /usr/local/lib/udev/rules.d. A
# per-user tree under ~/.local is invisible to udev, so we install nothing
# there and print the copy step in the message below instead.
rule_installed=no
if [ -f "$RULE_SRC" ] && { [ "$PREFIX" = "/usr" ] || [ "$PREFIX" = "/usr/local" ]; }; then
    if mkdir -p "$(dirname "$RULE_DST")" && cp -f "$RULE_SRC" "$RULE_DST"; then
        chmod 644 "$RULE_DST"
        rule_installed=yes
        reload_udev
    else
        echo "note: could not install the udev rule into $RULE_DST"
        echo "      (no write permission?). Continuing without it - see the"
        echo "      keyboard-access message at the end of this install."
    fi
fi

# Refresh the desktop database when we can, so the app appears in the menu
# without a logout. Failure is not fatal: the .desktop file is already in
# place and most desktops notice it within a session anyway.
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$DATADIR/applications" 2>/dev/null || true
fi

packs=$(find "$PACKDIR" -name '*.kvpack' 2>/dev/null | wc -l | tr -d ' ')

echo "Installed KeyVibes into $PREFIX ($packs sound pack(s))"
echo
echo "Next:"
echo "  keyvibes doctor                      # is this machine ready?"
echo "  keyvibes pack list                   # which packs are installed?"
echo "  keyvibes-ui                          # open the desktop app"
echo "  systemctl --user daemon-reload"
echo "  systemctl --user enable --now keyvibes"
echo

# PATH: only warn when the install location is not already on PATH, and only
# for a per-user install (a system PREFIX is almost always on PATH already).
case ":$PATH:" in
    *":$BINDIR:"*) ;;
    *)
        echo "$BINDIR is not on your PATH. Add it, e.g.:"
        echo "  echo 'export PATH=\"$BINDIR:\$PATH\"' >> ~/.profile   # then re-login"
        echo
        ;;
esac

# Keyboard access: the two supported models, in order of preference.
if [ -r /dev/input/event0 ] 2>/dev/null || [ -r /dev/input/event1 ] 2>/dev/null; then
    : # already readable - nothing to do
elif id -nG | tr ' ' '\n' | grep -qx input; then
    echo "You are in the 'input' group but the nodes are not readable yet:"
    echo "  log out and back in for the group change to take effect."
elif [ "$rule_installed" = yes ]; then
    echo "The udev rule was installed at $RULE_DST and udev reloaded."
    echo "ACLs apply to new sessions - log out and back in, then run"
    echo "'keyvibes doctor' to confirm /dev/input is readable."
else
    echo "KeyVibes cannot read /dev/input yet. Two supported options:"
    echo
    if [ -f "$RULE_SRC" ]; then
        echo "  1. Udev rule (recommended - no group change, session-scoped):"
        echo "     sudo install -Dm644 '$RULE_SRC' /etc/udev/rules.d/70-keyvibes-uaccess.rules"
        echo "     sudo udevadm control --reload-rules && sudo udevadm trigger"
    else
        echo "  1. Udev rule (recommended - no group change, session-scoped):"
        echo "     see docs/linux-permissions.md - the rule ships with packages"
        echo "     and in this tree under lib/udev/rules.d/ when present"
        echo "     sudo udevadm control --reload-rules && sudo udevadm trigger"
    fi
    echo
    echo "  2. input group (classic, permanent until you leave it):"
    echo "     sudo usermod -aG input \$USER   (then log out and back in)"
    echo
    echo "Either way, 'keyvibes doctor' re-checks access and tells you if it worked."
fi
