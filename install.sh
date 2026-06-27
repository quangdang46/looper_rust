#!/usr/bin/env bash
set -euo pipefail
umask 022

# === Config ===
BINARY_NAME="looper"
OWNER="quangdang46"
REPO="looper_rust"
DEST="${DEST:-$HOME/.local/bin}"
VERSION="${VERSION:-}"
QUIET=0; EASY=0; VERIFY=0; FROM_SOURCE=0; UNINSTALL=0
MAX_RETRIES=3; DOWNLOAD_TIMEOUT=120
LOCK_DIR="/tmp/${BINARY_NAME}-install.lock.d"
TMP=""

# === Logging ===
log_info()    { [ "$QUIET" -eq 1 ] && return; echo "[${BINARY_NAME}] $*" >&2; }
log_warn()    { echo "[${BINARY_NAME}] WARN: $*" >&2; }
log_success() { [ "$QUIET" -eq 1 ] && return; echo "✓ $*" >&2; }
die()         { echo "ERROR: $*" >&2; exit 1; }

# === Cleanup & lock ===
cleanup() { rm -rf "$TMP" "$LOCK_DIR" 2>/dev/null || true; }
trap cleanup EXIT
acquire_lock() {
    mkdir "$LOCK_DIR" 2>/dev/null || die "Another install running. rm -rf $LOCK_DIR"
    echo $$ > "$LOCK_DIR/pid"
}

# === Args ===
while [ $# -gt 0 ]; do
    case "$1" in
        --dest)       DEST="$2";   shift 2;;
        --dest=*)     DEST="${1#*=}"; shift;;
        --version)    VERSION="$2"; shift 2;;
        --version=*)  VERSION="${1#*=}"; shift;;
        --system)     DEST="/usr/local/bin"; shift;;
        --easy-mode)  EASY=1;      shift;;
        --verify)     VERIFY=1;    shift;;
        --from-source) FROM_SOURCE=1; shift;;
        --quiet|-q)   QUIET=1;     shift;;
        --uninstall)  UNINSTALL=1; shift;;
        -h|--help)
            echo "Usage: curl -fsSL https://raw.githubusercontent.com/$OWNER/$REPO/main/install.sh | bash"
            echo "  --dest <dir>     Install directory (default: $HOME/.local/bin)"
            echo "  --version <tag>  Specific version (default: latest)"
            echo "  --system         Install to /usr/local/bin"
            echo "  --easy-mode      Auto-add to PATH"
            echo "  --verify         Run --version after install"
            echo "  --from-source    Build from source"
            echo "  --uninstall      Remove binary"
            echo "  --quiet, -q      Quiet mode"
            exit 0;;
        *) shift;;
    esac
done

# === Uninstall ===
if [ "$UNINSTALL" -eq 1 ]; then
    rm -f "$DEST/$BINARY_NAME" "$DEST/looperd" "$DEST/looper-cli"
    for rc in "$HOME/.bashrc" "$HOME/.zshrc"; do
        [ -f "$rc" ] && sed -i "/${BINARY_NAME} installer/d" "$rc" 2>/dev/null || true
    done
    echo "✓ ${BINARY_NAME} uninstalled"; exit 0
fi

# === Platform ===
detect_platform() {
    local os arch
    case "$(uname -s)" in
        Linux*)  os="linux";;   Darwin*) os="darwin";;
        MINGW*|MSYS*|CYGWIN*) os="windows";;
        *) die "Unsupported OS: $(uname -s)";;
    esac
    case "$(uname -m)" in
        x86_64|amd64)  arch="x86_64";;
        aarch64|arm64) arch="aarch64";;
        *) die "Unsupported arch: $(uname -m)";;
    esac
    echo "${os}_${arch}"
}

# === Version ===
resolve_version() {
    [ -n "$VERSION" ] && return 0
    VERSION=$(curl -fsSL --connect-timeout 10 --max-time 30 \
        "https://api.github.com/repos/${OWNER}/${REPO}/releases/latest" 2>/dev/null \
        | grep '"tag_name":' | sed -E 's/.*"([^"]+)".*/\1/') || true
    if ! [[ "$VERSION" =~ ^v[0-9] ]]; then
        VERSION=$(curl -fsSL -o /dev/null -w '%{url_effective}' \
            "https://github.com/${OWNER}/${REPO}/releases/latest" 2>/dev/null \
            | sed -E 's|.*/tag/||') || true
    fi
    [[ "$VERSION" =~ ^v[0-9] ]] || die "Could not resolve version"
    log_info "Latest: $VERSION"
}

# === Download ===
download_file() {
    local url="$1" dest="$2" partial="${2}.part" attempt=0
    while [ $attempt -lt $MAX_RETRIES ]; do
        attempt=$((attempt + 1))
        curl -fL --connect-timeout 30 --max-time "$DOWNLOAD_TIMEOUT" \
             -sS --retry 2 \
             $( [ -s "$partial" ] && echo "--continue-at -") \
             -o "$partial" "$url" \
          && mv -f "$partial" "$dest" && return 0
        [ $attempt -lt $MAX_RETRIES ] && { log_warn "Retry $attempt..."; sleep 3; }
    done
    return 1
}

# === Atomic install ===
install_binary_atomic() {
    local tmp="${2}.tmp.$$"
    install -m 0755 "$1" "$tmp" && mv -f "$tmp" "$2" || { rm -f "$tmp"; die "Install failed"; }
}

# === PATH ===
maybe_add_path() {
    case ":$PATH:" in *":$DEST:"*) return 0;; esac
    if [ "$EASY" -eq 1 ]; then
        for rc in "$HOME/.zshrc" "$HOME/.bashrc"; do
            [ -f "$rc" ] && [ -w "$rc" ] || continue
            grep -qF "$DEST" "$rc" && continue
            printf '\nexport PATH="%s:$PATH"  # %s installer\n' "$DEST" "$BINARY_NAME" >> "$rc"
        done
    fi
    log_warn "Restart shell or: export PATH=\"$DEST:\$PATH\""
}

# === Source build ===
build_from_source() {
    command -v cargo >/dev/null || die "cargo not found — install Rust: https://rustup.rs"
    git clone --depth 1 "https://github.com/${OWNER}/${REPO}.git" "$TMP/src"
    (cd "$TMP/src" && CARGO_TARGET_DIR="$TMP/target" cargo build --release -p looperd -p looper-cli)
    for bin in looperd looper-cli; do
        [ -f "$TMP/target/release/$bin" ] && install_binary_atomic "$TMP/target/release/$bin" "$DEST/$bin"
    done
    # Symlink looper -> looper-cli for convenience
    ln -sf "$DEST/looper-cli" "$DEST/looper"
}

# === Main ===
main() {
    acquire_lock
    TMP=$(mktemp -d)
    mkdir -p "$DEST"

    local platform; platform=$(detect_platform)
    log_info "Platform: $platform | Dest: $DEST"

    if [ "$FROM_SOURCE" -eq 0 ]; then
        resolve_version
        local ext="tar.gz"; [[ "$platform" == windows* ]] && ext="zip"
        local archive="${BINARY_NAME}-${VERSION}-${platform}.${ext}"
        local url="https://github.com/${OWNER}/${REPO}/releases/download/${VERSION}/${archive}"

        if download_file "$url" "$TMP/$archive"; then
            if download_file "${url}.sha256" "$TMP/checksum.sha256" 2>/dev/null; then
                local expected actual
                expected=$(awk '{print $1}' "$TMP/checksum.sha256")
                actual=$(sha256sum "$TMP/$archive" 2>/dev/null | awk '{print $1}' \
                      || shasum -a 256 "$TMP/$archive" | awk '{print $1}')
                [ "$expected" = "$actual" ] || die "Checksum mismatch"
                log_info "Checksum verified"
            fi
            case "$archive" in
                *.tar.gz) tar -xzf "$TMP/$archive" -C "$TMP";;
                *.zip)    unzip -q "$TMP/$archive" -d "$TMP";;
            esac
            for bin in looperd looper-cli; do
                local b; b=$(find "$TMP" -name "$bin" -type f -perm -111 2>/dev/null | head -1)
                [ -n "$b" ] && install_binary_atomic "$b" "$DEST/$bin"
            done
            ln -sf "$DEST/looper-cli" "$DEST/looper"
        else
            log_warn "Download failed — building from source..."
            build_from_source
        fi
    else
        build_from_source
    fi

    maybe_add_path

    [ "$VERIFY" -eq 1 ] && "$DEST/looper" --version 2>/dev/null || true

    echo ""
    echo "✓ looper installed"
    echo "  Daemon: $DEST/looperd"
    echo "  CLI:    $DEST/looper"
    echo ""
    echo "  Usage: looper daemon start"
    echo "         looper --help"
}

if [[ "${BASH_SOURCE[0]:-}" == "${0:-}" ]] || [[ -z "${BASH_SOURCE[0]:-}" ]]; then
    { main "$@"; }
fi
