#!/system/bin/sh
# Run in Termux from the extracted release directory.
set -eu
: "${HOME:?HOME must be set by Termux}"
: "${PREFIX:?PREFIX must be set by Termux}"
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
INSTALL_DIR="$HOME/.local/share/desktop-commander-rust"
mkdir -p "$INSTALL_DIR" "$PREFIX/bin"
cp "$SCRIPT_DIR/desktop-commander" "$INSTALL_DIR/desktop-commander"
chmod 700 "$INSTALL_DIR/desktop-commander"
rm -rf "$INSTALL_DIR/resources"
cp -R "$SCRIPT_DIR/resources" "$INSTALL_DIR/resources"
cp "$SCRIPT_DIR/run-termux.sh" "$PREFIX/bin/desktop-commander"
chmod 700 "$PREFIX/bin/desktop-commander"
printf 'Installed native Android agent to %s\n' "$INSTALL_DIR"
printf 'Start: desktop-commander remote --headless\n'
