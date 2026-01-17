#!/usr/bin/env sh
# Install agos-proxy: the self-hosted OpenAI-compatible AI gateway.
#
# Installs the latest release binary plus man pages and shell completions:
#
#   curl -fsSL https://raw.githubusercontent.com/aivoo-app/agos-proxy/main/install.sh | sh
#
# Options (env vars):
#   AGOS_VERSION   release tag, e.g. v0.1.0 (default: latest)
#   INSTALL_DIR    binary dir (default: /usr/local/bin)
#   MAN_DIR        man page dir (default: <INSTALL_DIR>/../share/man/man1)
#   SKIP_COMPLETIONS=1   skip installing shell completions
#
# After install: `agos-proxy --help`, `man agos-proxy`.

set -eu

REPO="aivoo-app/agos-proxy"
BIN="agos-proxy"
VERSION="${AGOS_VERSION:-latest}"
INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"
MAN_DIR="${MAN_DIR:-$(dirname "$INSTALL_DIR")/share/man/man1}"

need() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "error: $1 is required but not installed" >&2
        exit 1
    }
}

need curl
need tar

OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"
case "$ARCH" in
    x86_64 | amd64) ARCH="x86_64" ;;
    aarch64 | arm64) ARCH="aarch64" ;;
    *) echo "error: unsupported architecture: $ARCH" >&2; exit 1 ;;
esac
case "$OS" in
    linux) TARGET="$ARCH-unknown-linux-gnu" ;;
    darwin) TARGET="$ARCH-apple-darwin" ;;
    *) echo "error: unsupported OS: $OS (linux/macOS only)" >&2; exit 1 ;;
esac

if [ "$VERSION" = "latest" ]; then
    echo "resolving latest release..."
    VERSION="$(curl -fsSL -o /dev/null -w '%{url_effective}' \
        "https://github.com/$REPO/releases/latest" | sed 's#.*/##')"
    [ -n "$VERSION" ] || { echo "error: could not resolve latest release" >&2; exit 1; }
fi

URL="https://github.com/$REPO/releases/download/$VERSION/$BIN-$VERSION-$TARGET.tar.gz"
echo "downloading $URL ..."
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM
curl -fsSL "$URL" -o "$TMP/release.tar.gz"

echo "installing $BIN $VERSION to $INSTALL_DIR ..."
mkdir -p "$INSTALL_DIR" "$TMP/unpack"
tar -xzf "$TMP/release.tar.gz" -C "$TMP/unpack"

USE_SUDO=""
[ -w "$INSTALL_DIR" ] || { need sudo; USE_SUDO="sudo"; }
$USE_SUDO install -m 755 "$TMP/unpack/$BIN" "$INSTALL_DIR/$BIN"

# Man pages.
if [ -f "$TMP/unpack/agos-proxy.1" ]; then
    [ -w "$MAN_DIR" ] || { need sudo; USE_SUDO="sudo"; }
    $USE_SUDO mkdir -p "$MAN_DIR"
    $USE_SUDO install -m 644 "$TMP/unpack"/agos-proxy*.1 "$MAN_DIR/"
    echo "man pages -> $MAN_DIR"
fi

# Shell completions.
if [ "${SKIP_COMPLETIONS:-0}" != "1" ]; then
    for spec in "bash:$HOME/.local/share/bash-completion/completions" \
                "fish:$HOME/.config/fish/completions" \
                "zsh:$HOME/.local/share/zsh/site-functions"; do
        shell="${spec%%:*}"
        dest="${spec#*:}"
        src="$TMP/unpack/completions/agos-proxy.$shell"
        [ "$shell" = "zsh" ] && src="$TMP/unpack/completions/_agos-proxy"
        [ "$shell" = "fish" ] && src="$TMP/unpack/completions/agos-proxy.fish"
        if [ -f "$src" ]; then
            mkdir -p "$dest"
            cp "$src" "$dest/"
            echo "completion ($shell) -> $dest"
        fi
    done
fi

echo
echo "$BIN $VERSION installed. Try: $BIN --help"
