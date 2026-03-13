#!/usr/bin/env bash
set -euo pipefail

REPO="pyrex41/rho"
BINARY_NAME="rho-cli"
INSTALL_DIR="${RHO_INSTALL_DIR:-$HOME/.local/bin}"

info() { printf '\033[1;34m%s\033[0m\n' "$*"; }
error() { printf '\033[1;31merror: %s\033[0m\n' "$*" >&2; exit 1; }

# Detect OS
case "$(uname -s)" in
    Linux*)  OS="linux" ;;
    Darwin*) OS="macos" ;;
    *)       error "Unsupported OS: $(uname -s)" ;;
esac

# Detect architecture
case "$(uname -m)" in
    x86_64|amd64)  ARCH="x86_64" ;;
    aarch64|arm64)  ARCH="aarch64" ;;
    *)              error "Unsupported architecture: $(uname -m)" ;;
esac

info "Detected: ${OS} ${ARCH}"

# Determine asset name pattern (must match CI asset names)
case "$(uname -m)" in
    x86_64|amd64) ARCH_LABEL="x64" ;;
    aarch64|arm64) ARCH_LABEL="arm64" ;;
esac

ASSET_PATTERN="rho-cli-${OS}-${ARCH_LABEL}"

# Get latest release tag
info "Fetching latest release..."
RELEASE_URL="https://api.github.com/repos/${REPO}/releases/latest"
if command -v curl &>/dev/null; then
    RELEASE_JSON=$(curl -fsSL "$RELEASE_URL")
elif command -v wget &>/dev/null; then
    RELEASE_JSON=$(wget -qO- "$RELEASE_URL")
else
    error "curl or wget is required"
fi

TAG=$(echo "$RELEASE_JSON" | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')
[ -z "$TAG" ] && error "Could not determine latest release tag"
info "Latest release: ${TAG}"

# Find matching asset URL
DOWNLOAD_URL=$(echo "$RELEASE_JSON" | grep '"browser_download_url"' | grep "$ASSET_PATTERN" | grep -v '\.sha256' | head -1 | sed 's/.*"\(https[^"]*\)".*/\1/')
[ -z "$DOWNLOAD_URL" ] && error "No binary found for ${TARGET}. Check https://github.com/${REPO}/releases for available assets."

info "Downloading ${DOWNLOAD_URL}..."

# Download to temp location
TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

if command -v curl &>/dev/null; then
    curl -fsSL -o "${TMPDIR}/archive" "$DOWNLOAD_URL"
else
    wget -qO "${TMPDIR}/archive" "$DOWNLOAD_URL"
fi

# Extract if archive, otherwise treat as raw binary
cd "$TMPDIR"
case "$DOWNLOAD_URL" in
    *.tar.gz|*.tgz)
        tar xzf archive
        BINARY=$(find . -name "$BINARY_NAME" -type f | head -1)
        [ -z "$BINARY" ] && error "Binary '${BINARY_NAME}' not found in archive"
        ;;
    *.zip)
        unzip -q archive
        BINARY=$(find . -name "$BINARY_NAME" -type f | head -1)
        [ -z "$BINARY" ] && error "Binary '${BINARY_NAME}' not found in archive"
        ;;
    *)
        BINARY="archive"
        ;;
esac

# Install
mkdir -p "$INSTALL_DIR"
cp "$BINARY" "${INSTALL_DIR}/${BINARY_NAME}"
chmod +x "${INSTALL_DIR}/${BINARY_NAME}"

info "Installed ${BINARY_NAME} to ${INSTALL_DIR}/${BINARY_NAME}"

# Check if install dir is in PATH
if ! echo "$PATH" | tr ':' '\n' | grep -qx "$INSTALL_DIR"; then
    echo ""
    info "Add ${INSTALL_DIR} to your PATH:"
    echo ""
    echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
    echo ""
    echo "Add this to your shell profile (~/.bashrc, ~/.zshrc, etc.) to make it permanent."
fi

info "Done! Run 'rho-cli --help' to get started."
