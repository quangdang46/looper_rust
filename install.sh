#!/usr/bin/env bash
set -euo pipefail
umask 022

BINARY_NAME="grove"
OWNER="quangdang46"
REPO="grove"
DEST="${DEST:-$HOME/.local/bin}"
VERSION="${VERSION:-}"
QUIET=0
EASY=0
VERIFY=0
FROM_SOURCE=0
UNINSTALL=0
MAX_RETRIES=3
DOWNLOAD_TIMEOUT=120
LOCK_DIR="/tmp/${BINARY_NAME}-install.lock.d"
TMP=""
MCP_AGENT_MAIL_BOOTSTRAP='curl -fsSL "https://raw.githubusercontent.com/Dicklesworthstone/mcp_agent_mail/main/scripts/install.sh?$(date +%s)" | bash -s -- --yes'
INSTALL_MCP_AGENT_MAIL=0

log_info() {
    [ "$QUIET" -eq 1 ] && return
    echo "[${BINARY_NAME}] $*" >&2
}
log_warn() {
    echo "[${BINARY_NAME}] WARN: $*" >&2
}
log_success() {
    [ "$QUIET" -eq 1 ] && return
    echo "✓ $*" >&2
}
die() {
    echo "ERROR: $*" >&2
    exit 1
}

cleanup() {
    rm -rf "$TMP" "$LOCK_DIR" 2>/dev/null || true
}
trap cleanup EXIT

acquire_lock() {
    mkdir "$LOCK_DIR" 2>/dev/null || die "Another install is running. If stuck: rm -rf $LOCK_DIR"
    echo $$ > "$LOCK_DIR/pid"
}

usage() {
    cat <<'EOF'
Install grove from GitHub releases.

Options:
  --dest PATH         Install into PATH
  --version VERSION   Install a specific release tag
  --system            Install into /usr/local/bin
  --easy-mode         Append DEST to shell rc PATH
  --verify            Run grove --version after install
  --from-source       Build from source instead of downloading a release
  --with-mcp-agent-mail    Install MCP Agent Mail without prompting
  --without-mcp-agent-mail Skip MCP Agent Mail without prompting
  --quiet, -q         Reduce output
  --uninstall         Remove installed binary
  -h, --help          Show this help
EOF
    exit 0
}

while [ $# -gt 0 ]; do
    case "$1" in
        --with-mcp-agent-mail)
            INSTALL_MCP_AGENT_MAIL=1
            shift
            ;;
        --without-mcp-agent-mail)
            INSTALL_MCP_AGENT_MAIL=0
            shift
            ;;
        --dest)
            DEST="$2"
            shift 2
            ;;
        --dest=*)
            DEST="${1#*=}"
            shift
            ;;
        --version)
            VERSION="$2"
            shift 2
            ;;
        --version=*)
            VERSION="${1#*=}"
            shift
            ;;
        --system)
            DEST="/usr/local/bin"
            shift
            ;;
        --easy-mode)
            EASY=1
            shift
            ;;
        --verify)
            VERIFY=1
            shift
            ;;
        --from-source)
            FROM_SOURCE=1
            shift
            ;;
        --quiet|-q)
            QUIET=1
            shift
            ;;
        --uninstall)
            UNINSTALL=1
            shift
            ;;
        -h|--help)
            usage
            ;;
        *)
            die "Unknown argument: $1"
            ;;
    esac
done

do_uninstall() {
    rm -f "$DEST/$BINARY_NAME"
    for rc in "$HOME/.bashrc" "$HOME/.zshrc"; do
        [ -f "$rc" ] && sed -i "/${BINARY_NAME} installer/d" "$rc" 2>/dev/null || true
    done
    log_success "Uninstalled"
    exit 0
}

[ "$UNINSTALL" -eq 1 ] && do_uninstall

detect_platform() {
    local os arch
    case "$(uname -s)" in
        Linux*)
            os="linux"
            ;;
        Darwin*)
            os="macos"
            ;;
        MINGW*|MSYS*|CYGWIN*)
            os="windows"
            ;;
        *)
            die "Unsupported OS: $(uname -s)"
            ;;
    esac
    case "$(uname -m)" in
        x86_64|amd64)
            arch="x86_64"
            ;;
        aarch64|arm64)
            arch="aarch64"
            ;;
        *)
            die "Unsupported arch: $(uname -m)"
            ;;
    esac
    echo "${os}_${arch}"
}

resolve_version() {
    [ -n "$VERSION" ] && return 0

    VERSION=$(curl -fsSL \
        --connect-timeout 10 --max-time 30 \
        -H "Accept: application/vnd.github.v3+json" \
        "https://api.github.com/repos/${OWNER}/${REPO}/releases/latest" \
        2>/dev/null | grep '"tag_name":' | sed -E 's/.*"([^"]+)".*/\1/') || true

    if [ -z "$VERSION" ]; then
        VERSION=$(curl -fsSL -o /dev/null -w '%{url_effective}' \
            "https://github.com/${OWNER}/${REPO}/releases/latest" \
            2>/dev/null | sed -E 's|.*/tag/||') || true
    fi

    [[ "$VERSION" =~ ^v[0-9] ]] || die "Could not resolve version"
}

archive_name_for_platform() {
    local platform="$1"
    case "$platform" in
        linux_x86_64)
            echo "${BINARY_NAME}-${VERSION}-linux-x86_64.tar.gz"
            ;;
        linux_aarch64)
            echo "${BINARY_NAME}-${VERSION}-linux-aarch64.tar.gz"
            ;;
        macos_x86_64)
            echo "${BINARY_NAME}-${VERSION}-macos-x86_64.tar.gz"
            ;;
        macos_aarch64)
            echo "${BINARY_NAME}-${VERSION}-macos-aarch64.tar.gz"
            ;;
        windows_x86_64)
            echo "${BINARY_NAME}-${VERSION}-windows-x86_64.zip"
            ;;
        *)
            die "Unsupported platform combination: $platform"
            ;;
    esac
}

download_file() {
    local url="$1" dest="$2"
    local partial="${dest}.part"
    local attempt=0

    while [ $attempt -lt $MAX_RETRIES ]; do
        attempt=$((attempt + 1))
        if curl -fL \
            --connect-timeout 30 \
            --max-time "$DOWNLOAD_TIMEOUT" \
            --retry 2 \
            $( [ -s "$partial" ] && echo "--continue-at -" ) \
            $( [ "$QUIET" -eq 0 ] && [ -t 2 ] && echo "--progress-bar" || echo "-sS" ) \
            -o "$partial" "$url"; then
            mv -f "$partial" "$dest"
            return 0
        fi
        [ $attempt -lt $MAX_RETRIES ] && {
            log_warn "Retrying in 3s..."
            sleep 3
        }
    done
    return 1
}

checksum_file() {
    local file="$1"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$file" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$file" | awk '{print $1}'
    else
        die "No SHA256 tool found"
    fi
}

install_binary_atomic() {
    local src="$1" dest="$2"
    local tmp="${dest}.tmp.$$"
    install -m 0755 "$src" "$tmp"
    mv -f "$tmp" "$dest" || {
        rm -f "$tmp"
        die "Failed to install binary"
    }
}

maybe_add_path() {
    case ":$PATH:" in
        *":$DEST:"*) return 0 ;;
    esac
    if [ "$EASY" -eq 1 ]; then
        for rc in "$HOME/.zshrc" "$HOME/.bashrc"; do
            [ -f "$rc" ] && [ -w "$rc" ] || continue
            grep -qF "$DEST" "$rc" && continue
            printf '\nexport PATH="%s:$PATH"  # %s installer\n' "$DEST" "$BINARY_NAME" >> "$rc"
        done
        log_warn "PATH updated — restart shell or: export PATH=\"$DEST:\$PATH\""
    else
        log_warn "Add to PATH: export PATH=\"$DEST:\$PATH\""
    fi
}

bootstrap_mcp_agent_mail() {
    log_info "Installing mcp_agent_mail helper"
    bash -lc "$MCP_AGENT_MAIL_BOOTSTRAP"
}

should_install_mcp_agent_mail() {
    if [ -t 0 ]; then
        printf "Install MCP Agent Mail too? [Y/n] " >&2
        local reply
        IFS= read -r reply || true
        case "$reply" in
            ""|[Yy]|[Yy][Ee][Ss])
                return 0
                ;;
            [Nn]|[Nn][Oo])
                return 1
                ;;
            *)
                log_warn "Unrecognized response. Installing Grove only."
                return 1
                ;;
        esac
    fi

    return "$INSTALL_MCP_AGENT_MAIL"
}

build_from_source() {
    command -v cargo >/dev/null || die "Rust/cargo not found. Install: https://rustup.rs"
    command -v git >/dev/null || die "git not found"
    git clone --depth 1 "https://github.com/${OWNER}/${REPO}.git" "$TMP/src"
    (
        cd "$TMP/src"
        CARGO_TARGET_DIR="$TMP/target" cargo build --release --locked --package grove-cli
    )
    install_binary_atomic "$TMP/target/release/$BINARY_NAME" "$DEST/$BINARY_NAME"
}

extract_archive() {
    local archive="$1"
    case "$archive" in
        *.tar.gz)
            tar -xzf "$archive" -C "$TMP"
            ;;
        *.zip)
            unzip -q "$archive" -d "$TMP"
            ;;
        *)
            die "Unsupported archive format: $archive"
            ;;
    esac
}

find_extracted_binary() {
    local candidate
    candidate=$(find "$TMP" -type f \( -name "$BINARY_NAME" -o -name "${BINARY_NAME}.exe" \) | head -n 1)
    [ -n "$candidate" ] || die "Binary not found after extract"
    echo "$candidate"
}

print_summary() {
    echo ""
    echo "✓ ${BINARY_NAME} installed → $DEST/$BINARY_NAME"
    echo "  Version: $("$DEST/$BINARY_NAME" --version 2>/dev/null || echo 'unknown')"
    echo ""
    echo "  Quick start:"
    echo "    $BINARY_NAME --help"
}

main() {
    acquire_lock
    TMP=$(mktemp -d)
    mkdir -p "$DEST"

    local platform archive url bin_path expected actual
    platform=$(detect_platform)

    log_info "Installing grove binary"
    if [ "$FROM_SOURCE" -eq 0 ]; then
        resolve_version
        archive=$(archive_name_for_platform "$platform")
        url="https://github.com/${OWNER}/${REPO}/releases/download/${VERSION}/${archive}"

        if download_file "$url" "$TMP/$archive"; then
            if download_file "${url}.sha256" "$TMP/checksum.sha256" 2>/dev/null; then
                expected=$(awk '{print $1}' "$TMP/checksum.sha256")
                actual=$(checksum_file "$TMP/$archive")
                [ "$expected" = "$actual" ] || die "Checksum mismatch"
            fi
            extract_archive "$TMP/$archive"
            bin_path=$(find_extracted_binary)
            install_binary_atomic "$bin_path" "$DEST/$BINARY_NAME"
        else
            log_warn "Binary download failed — building from source..."
            build_from_source
        fi
    else
        build_from_source
    fi

    log_success "grove installed → $DEST/$BINARY_NAME"

    if should_install_mcp_agent_mail; then
        bootstrap_mcp_agent_mail
    else
        log_info "Skipping MCP Agent Mail install"
    fi

    maybe_add_path

    if [ "$VERIFY" -eq 1 ]; then
        "$DEST/$BINARY_NAME" --version >/dev/null
    fi

    print_summary
}

if [[ "${BASH_SOURCE[0]:-}" == "${0:-}" ]] || [[ -z "${BASH_SOURCE[0]:-}" ]]; then
    { main "$@"; }
fi
