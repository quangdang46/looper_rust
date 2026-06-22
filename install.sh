#!/usr/bin/env bash
# Looper Rust — install.sh
# curl -fsSL https://github.com/quangdang46/looper/releases/latest/download/install.sh | bash
#
# Installs looperd, looper-cli, and loopernet binaries from the latest
# GitHub release.  Targets: aarch64-apple-darwin, x86_64-apple-darwin,
# x86_64-unknown-linux-musl, aarch64-unknown-linux-musl.

set -eu -o pipefail

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------
REPO="quangdang46/looper"
RELEASE_URL="https://github.com/${REPO}/releases"
API_URL="https://api.github.com/repos/${REPO}/releases/latest"

BINARIES=(looperd looper-cli loopernet)

# Runtime overridable
INSTALL_DIR="${INSTALL_DIR:-${HOME}/.local/bin}"

# Colours (only when stdout is a terminal)
if [[ -t 1 ]]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    YELLOW='\033[0;33m'
    CYAN='\033[0;36m'
    BOLD='\033[1m'
    RESET='\033[0m'
else
    RED=''
    GREEN=''
    YELLOW=''
    CYAN=''
    BOLD=''
    RESET=''
fi

log()  { printf "${GREEN}==>${RESET} %s\n" "$*"; }
warn() { printf "${YELLOW}==>${RESET} %s\n" "$*" >&2; }
err()  { printf "${RED}==>${RESET} %s\n" "$*" >&2; }

# ---------------------------------------------------------------------------
# Help
# ---------------------------------------------------------------------------
usage() {
    cat <<EOF
Usage: ${0##*/} [OPTIONS]

Install the Looper Rust binaries (looperd, looper-cli, loopernet) from the
latest GitHub release.

Options:
  -d, --dir DIR    Install to DIR instead of ~/.local/bin
  -v, --version TAG  Install a specific version tag (e.g. v0.1.0)
  -h, --help       Show this help and exit

Environment:
  INSTALL_DIR      Same as --dir (default: ~/.local/bin)

Example:
  curl -fsSL https://github.com/quangdang46/looper/releases/latest/download/install.sh | bash
EOF
    exit 0
}

# Parse options
while [[ $# -gt 0 ]]; do
    case "$1" in
        -d|--dir) INSTALL_DIR="$2"; shift 2 ;;
        -v|--version) VERSION_TAG="$2"; shift 2 ;;
        -h|--help) usage ;;
        *) err "Unknown option: $1"; usage ;;
    esac
done

# ---------------------------------------------------------------------------
# Platform detection
# ---------------------------------------------------------------------------
detect_arch() {
    local arch
    arch="$(uname -m)"
    case "$arch" in
        x86_64|amd64)  echo "x86_64" ;;
        aarch64|arm64) echo "aarch64" ;;
        *) err "Unsupported architecture: ${arch}"; exit 1 ;;
    esac
}

detect_os() {
    local os
    os="$(uname -s)"
    case "$os" in
        Darwin) echo "apple-darwin" ;;
        Linux)  echo "unknown-linux-musl" ;;
        *) err "Unsupported OS: ${os}"; exit 1 ;;
    esac
}

TARGET_ARCH="$(detect_arch)"
TARGET_OS="$(detect_os)"
TARGET="${TARGET_ARCH}-${TARGET_OS}"

# XDG compliance on Linux for INSTALL_DIR default
if [[ "${TARGET_OS}" == "unknown-linux-musl" && -z "${INSTALL_DIR_OVERRIDE:-}" ]]; then
    if [[ -n "${XDG_BIN_HOME:-}" ]]; then
        INSTALL_DIR="${XDG_BIN_HOME}"
    elif [[ -n "${XDG_DATA_HOME:-}" ]]; then
        INSTALL_DIR="${XDG_DATA_HOME}/../bin"
    fi
fi
# Normalise path (remove trailing /../bin)
INSTALL_DIR="$(cd "$(dirname "${INSTALL_DIR}")" 2>/dev/null && pwd)/$(basename "${INSTALL_DIR}")" 2>/dev/null || INSTALL_DIR="${INSTALL_DIR}"

log "Detected platform: ${TARGET}"
log "Install directory: ${INSTALL_DIR}"

# ---------------------------------------------------------------------------
# Prerequisites
# ---------------------------------------------------------------------------
need_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        err "Required command not found: $1"
        exit 1
    fi
}

need_cmd curl
need_cmd install
need_cmd uname
need_cmd mktemp

# sha256sum on Linux, shasum on macOS
if command -v sha256sum >/dev/null 2>&1; then
    SHA_CMD="sha256sum"
elif command -v shasum >/dev/null 2>&1; then
    SHA_CMD="shasum -a 256"
else
    SHA_CMD=""
    warn "No sha256sum or shasum found — skipping checksum verification"
fi

# ---------------------------------------------------------------------------
# Resolve release version and download URLs
# ---------------------------------------------------------------------------
resolve_release() {
    if [[ -n "${VERSION_TAG:-}" ]]; then
        log "Resolving release: ${VERSION_TAG}"
        # Use the tag-specific API; fall back to tag download URL
        RELEASE_INFO_URL="https://api.github.com/repos/${REPO}/releases/tags/${VERSION_TAG}"
    else
        log "Resolving latest release..."
        RELEASE_INFO_URL="${API_URL}"
    fi

    RELEASE_JSON="$(curl -fsSL "${RELEASE_INFO_URL}" 2>/dev/null || true)"

    if [[ -z "${RELEASE_JSON}" ]]; then
        # If API fails (rate-limited, etc.), construct the download URL directly
        if [[ -n "${VERSION_TAG:-}" ]]; then
            TAG="${VERSION_TAG}"
        else
            TAG="latest"
        fi
        warn "GitHub API unavailable; falling back to tag '${TAG}'"
        DOWNLOAD_BASE="${RELEASE_URL}/download/${TAG}"
        echo "${TAG}"
        return
    fi

    # Extract tag name from JSON
    TAG="$(printf '%s' "${RELEASE_JSON}" | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')"
    if [[ -z "${TAG}" ]]; then
        err "Could not parse release tag from API response"
        exit 1
    fi

    # Extract download URL for checksums (prefer the platform-specific bundle)
    CHECKSUMS_ASSET="$(printf '%s' "${RELEASE_JSON}" \
        | grep '"browser_download_url"' \
        | grep "checksums.txt" \
        | head -1 \
        | sed 's/.*"browser_download_url": *"\([^"]*\)".*/\1/')"

    echo "${TAG}"
}

TAG="$(resolve_release)"
log "Release tag: ${TAG}"

# ---------------------------------------------------------------------------
# Download & verify
# ---------------------------------------------------------------------------
download_checksums() {
    local tag="$1"
    local out="$2"

    # Try API-discovered URL first
    if [[ -n "${CHECKSUMS_ASSET:-}" ]]; then
        if curl -fsSL "${CHECKSUMS_ASSET}" -o "${out}" 2>/dev/null; then
            return 0
        fi
    fi

    # Fallback: construct from tag
    local url="${RELEASE_URL}/download/${tag}/checksums.txt"
    if curl -fsSL "${url}" -o "${out}" 2>/dev/null; then
        return 0
    fi

    return 1
}

verify_hash() {
    local file="$1"
    local expected_hash="$2"

    if [[ -z "${SHA_CMD}" || -z "${expected_hash}" ]]; then
        return 0
    fi

    local actual_hash
    actual_hash="$(${SHA_CMD} "${file}" 2>/dev/null | awk '{print $1}')"

    if [[ "${actual_hash}" != "${expected_hash}" ]]; then
        err "Checksum mismatch for $(basename "${file}")"
        err "  Expected: ${expected_hash}"
        err "  Actual:   ${actual_hash}"
        return 1
    fi
    log "Checksum verified: $(basename "${file}")"
}

TMPDIR="$(mktemp -d /tmp/looper-install-XXXXXX)"
trap 'rm -rf "${TMPDIR}"' EXIT

# Download checksums (best-effort)
CHECKSUMS_FILE="${TMPDIR}/checksums.txt"
HAVE_CHECKSUMS=false
if download_checksums "${TAG}" "${CHECKSUMS_FILE}"; then
    HAVE_CHECKSUMS=true
    log "Checksums downloaded"
fi

# Download and verify each binary
FAILED=0
for bin in "${BINARIES[@]}"; do
    # Determine platform-specific binary name
    # Release assets are named: looperd-aarch64-apple-darwin.tar.gz
    ARCHIVE_NAME="${bin}-${TARGET}.tar.gz"
    URL="${RELEASE_URL}/download/${TAG}/${ARCHIVE_NAME}"

    log "Downloading ${bin} (${TARGET})..."
    BIN_TMPDIR="${TMPDIR}/${bin}"
    mkdir -p "${BIN_TMPDIR}"
    ARCHIVE_PATH="${BIN_TMPDIR}/${ARCHIVE_NAME}"

    if ! curl -fsSL "${URL}" -o "${ARCHIVE_PATH}"; then
        err "Failed to download ${ARCHIVE_NAME}"
        warn "Binary '${bin}' may not be available for this platform; skipping"
        continue
    fi

    # Checksum verification
    EXPECTED_HASH=""
    if "${HAVE_CHECKSUMS}"; then
        EXPECTED_HASH="$(grep "${ARCHIVE_NAME}" "${CHECKSUMS_FILE}" 2>/dev/null | awk '{print $1}' || true)"
        if ! verify_hash "${ARCHIVE_PATH}" "${EXPECTED_HASH}"; then
            err "Aborting due to checksum mismatch"
            exit 1
        fi
    fi

    # Extract
    if ! tar xzf "${ARCHIVE_PATH}" -C "${BIN_TMPDIR}"; then
        err "Failed to extract ${ARCHIVE_NAME}"
        exit 1
    fi

    # Find the binary inside the archive
    FOUND_BIN="$(find "${BIN_TMPDIR}" -type f -name "${bin}" 2>/dev/null | head -1)"
    if [[ -z "${FOUND_BIN}" ]]; then
        err "Binary '${bin}' not found inside the archive"
        exit 1
    fi

    # Install
    mkdir -p "${INSTALL_DIR}"
    install -m 755 "${FOUND_BIN}" "${INSTALL_DIR}/${bin}"
    log "Installed ${INSTALL_DIR}/${bin}"
done

if [[ "${FAILED}" -gt 0 ]]; then
    err "${FAILED} binary(ies) failed to install"
    exit 1
fi

# ---------------------------------------------------------------------------
# Post-install: PATH check & instructions
# ---------------------------------------------------------------------------
RC_FILE=""
FOUND_IN_PATH=false
if command -v looperd >/dev/null 2>&1; then
    FOUND_IN_PATH=true
fi

if ! "${FOUND_IN_PATH}"; then
    # Detect shell rc file
    case "${SHELL:-}" in
        */zsh) RC_FILE="${ZDOTDIR:-${HOME}}/.zshrc" ;;
        */bash) RC_FILE="${HOME}/.bashrc" ;;
        *) RC_FILE="${HOME}/.profile" ;;
    esac

    cat <<EOF

${YELLOW}━━━ PATH Setup ━━━${RESET}
${BOLD}${INSTALL_DIR}${RESET} is not in your PATH.

To add it, run:

    echo 'export PATH="\${PATH}:${INSTALL_DIR}"' >> "${RC_FILE}"
    source "${RC_FILE}"

Or for the current shell session:

    export PATH="\${PATH}:${INSTALL_DIR}"
EOF
fi

# Print version banner
cat <<EOF

${GREEN}━━━ Installation Complete ━━━${RESET}
  Binaries installed in: ${BOLD}${INSTALL_DIR}${RESET}
  Release:              ${BOLD}${TAG}${RESET}
  Platform:             ${BOLD}${TARGET}${RESET}

  Run ${CYAN}looperd --version${RESET}   to start the daemon
  Run ${CYAN}looper-cli --help${RESET}   for CLI usage
  Run ${CYAN}loopernet --help${RESET}    for net usage

  ${YELLOW}Need help?${RESET}  ${RELEASE_URL}
EOF
