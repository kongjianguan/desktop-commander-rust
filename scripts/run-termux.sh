#!/system/bin/sh
# Wrapper keeps the executable next to its own native resources.
set -eu
: "${HOME:?HOME must be set by Termux}"
exec "$HOME/.local/share/desktop-commander-rust/desktop-commander" "$@"
