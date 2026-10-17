#!/bin/sh
# Installs the tree this script lives in.
#
#   ./install.sh                 into $HOME/.local
#   PREFIX=/usr ./install.sh     system-wide (sudo)
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PREFIX="${PREFIX:-$HOME/.local}"

mkdir -p "$PREFIX/bin" "$PREFIX/share" "$PREFIX/lib/systemd/user"
cp -a "$here/bin/." "$PREFIX/bin/"
cp -a "$here/share/." "$PREFIX/share/"
cp -a "$here/lib/systemd/user/." "$PREFIX/lib/systemd/user/"

echo "Installed KeyVibes into $PREFIX"
echo
echo "Next:"
echo "  keyvibes doctor                      # is this machine ready?"
echo "  keyvibes-ui                          # open the desktop app"
echo "  systemctl --user daemon-reload"
echo "  systemctl --user enable --now keyvibes"
echo
case "$(id -nG)" in
    *input*) ;;
    *)
        echo "If doctor cannot read /dev/input:"
        echo "  sudo usermod -aG input \$USER   (then log out and back in)"
        ;;
esac
