#!/usr/bin/env bash
# TalaDB CLI installer
# Usage:
#   curl -fsSL https://github.com/taladb/taladb/releases/latest/download/install.sh | bash
#
# Options (env vars):
#   VERSION     — pin a specific release tag, e.g. VERSION=v0.11.0
#   INSTALL_DIR — override install location (default: /usr/local/bin)

set -euo pipefail

REPO="taladb/taladb"
BIN="taladb"
INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"
VERSION="${VERSION:-latest}"

# ── Detect platform ──────────────────────────────────────────────────────────

OS=$(uname -s)
ARCH=$(uname -m)

case "$OS" in
  Darwin)
    case "$ARCH" in
      x86_64)        TARGET="x86_64-apple-darwin" ;;
      arm64|aarch64) TARGET="aarch64-apple-darwin" ;;
      *) echo "error: unsupported macOS architecture: $ARCH" >&2; exit 1 ;;
    esac
    ;;
  Linux)
    case "$ARCH" in
      x86_64) TARGET="x86_64-unknown-linux-gnu" ;;
      # No prebuilt arm64 Linux binary yet. Say so plainly and point at the
      # path that does work, rather than 404ing on a download.
      aarch64|arm64)
        echo "error: no prebuilt Linux arm64 binary yet." >&2
        echo "       Build it from source instead:" >&2
        echo "         cargo install --git https://github.com/$REPO taladb-cli" >&2
        exit 1
        ;;
      *) echo "error: unsupported Linux architecture: $ARCH" >&2; exit 1 ;;
    esac
    ;;
  *)
    echo "error: unsupported OS '$OS'." >&2
    echo "       For Windows, download the .zip from:" >&2
    echo "       https://github.com/$REPO/releases/latest" >&2
    exit 1
    ;;
esac

# ── Resolve download URL ─────────────────────────────────────────────────────

if [ "$VERSION" = "latest" ]; then
  BASE_URL="https://github.com/$REPO/releases/latest/download"
else
  BASE_URL="https://github.com/$REPO/releases/download/$VERSION"
fi

ARCHIVE="${BIN}-${TARGET}.tar.gz"
URL="${BASE_URL}/${ARCHIVE}"

# ── Download & extract ───────────────────────────────────────────────────────

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

echo "  Downloading ${BIN} (${TARGET})…"
if ! curl -fsSL "$URL" -o "$TMP/$ARCHIVE"; then
  echo "error: download failed: $URL" >&2
  echo "       check that a release exists at https://github.com/$REPO/releases" >&2
  exit 1
fi

tar -xzf "$TMP/$ARCHIVE" -C "$TMP"

if [ ! -f "$TMP/$BIN" ]; then
  echo "error: binary '$BIN' not found in archive" >&2
  exit 1
fi

chmod +x "$TMP/$BIN"

# ── Install ──────────────────────────────────────────────────────────────────

# `mkdir -p` first: INSTALL_DIR=~/.local/bin is the usual override and does not
# always exist yet. Only sudo when the directory genuinely is not writable.
if [ ! -d "$INSTALL_DIR" ]; then
  mkdir -p "$INSTALL_DIR" 2>/dev/null || sudo mkdir -p "$INSTALL_DIR"
fi

if [ -w "$INSTALL_DIR" ]; then
  mv "$TMP/$BIN" "$INSTALL_DIR/$BIN"
else
  echo "  Installing to $INSTALL_DIR  (sudo required)"
  sudo mv "$TMP/$BIN" "$INSTALL_DIR/$BIN"
fi

# ── Done ─────────────────────────────────────────────────────────────────────

echo ""
echo "  ✓  taladb installed → $INSTALL_DIR/$BIN"
echo ""
"$INSTALL_DIR/$BIN" --version
echo ""

# A directory that is not on PATH is the single most common reason the next
# command fails, and it is worth saying at install time rather than leaving the
# user to discover `command not found`.
case ":${PATH}:" in
  *":${INSTALL_DIR}:"*) ;;
  *)
    echo "  note: $INSTALL_DIR is not on your PATH. Add it:"
    echo "    export PATH=\"$INSTALL_DIR:\$PATH\""
    echo ""
    ;;
esac

echo "  Next steps:"
echo "    taladb studio myapp.db       # browse and edit in the browser"
echo "    taladb inspect myapp.db      # collections and document counts"
echo "    taladb export myapp.db users # dump a collection as JSON"
echo ""
echo "  taladb studio binds to localhost only and has no authentication."
echo ""
