#!/usr/bin/env bash
set -euo pipefail

REPO="pyrex41/rho"
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

# Determine asset name pattern label
case "$(uname -m)" in
    x86_64|amd64) ARCH_LABEL="x64" ;;
    aarch64|arm64) ARCH_LABEL="arm64" ;;
esac

# Function to download and install a binary from GitHub release
install_binary() {
  local bin_name="$1"
  local asset_pattern="$2"
  local required="${3:-false}"

  info "Looking for ${bin_name} pre-build (${asset_pattern})..."

  local DOWNLOAD_URL=$(echo "$RELEASE_JSON" | grep '"browser_download_url"' | grep "$asset_pattern" | grep -v '\.sha256' | head -1 | sed 's/.*"\(https[^"]*\)".*/\1/')

  if [ -z "$DOWNLOAD_URL" ]; then
    if [ "$required" = "true" ]; then
      error "No binary found for ${asset_pattern}. Check https://github.com/${REPO}/releases for available assets."
    else
      info "No pre-built ${bin_name} available for this platform. Skipping."
      return 0
    fi
  fi

  info "Downloading ${DOWNLOAD_URL}..."

  local TMPDIR=$(mktemp -d)
  if command -v curl &>/dev/null; then
    curl -fsSL -o "${TMPDIR}/archive" "$DOWNLOAD_URL"
  else
    wget -qO "${TMPDIR}/archive" "$DOWNLOAD_URL"
  fi

  cd "$TMPDIR"
  local BINARY
  case "$DOWNLOAD_URL" in
    *.tar.gz|*.tgz)
      tar xzf archive
      BINARY=$(find . -name "$bin_name" -type f | head -1)
      ;;
    *.zip)
      unzip -q archive
      BINARY=$(find . -name "$bin_name" -type f | head -1)
      ;;
    *)
      BINARY="archive"
      ;;
  esac

  [ -z "$BINARY" ] && error "Binary '${bin_name}' not found in archive"

  mkdir -p "$INSTALL_DIR"
  cp "$BINARY" "${INSTALL_DIR}/${bin_name}"
  chmod +x "${INSTALL_DIR}/${bin_name}"

  info "Installed ${bin_name} to ${INSTALL_DIR}/${bin_name}"
  rm -rf "$TMPDIR"
}

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

# Install CLI and GUI (both required)
install_binary "rho-cli" "rho-cli-${OS}-${ARCH_LABEL}" true
install_binary "rho" "rho-${OS}-${ARCH_LABEL}" true

# Check if install dir is in PATH
if ! echo "$PATH" | tr ':' '\n' | grep -qx "$INSTALL_DIR"; then
    echo ""
    info "Add ${INSTALL_DIR} to your PATH:"
    echo ""
    echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
    echo ""
    echo "Add this to your shell profile (~/.bashrc, ~/.zshrc, etc.) to make it permanent."
fi

info "Done! Run 'rho' for the GUI or 'rho-cli --help' to get started."
