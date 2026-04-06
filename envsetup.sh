#!/usr/bin/env bash
# envsetup.sh — set up the ShioRamen development environment.
#
# USAGE
#   Source it to wire PATH and LD_LIBRARY_PATH for the current shell:
#     source envsetup.sh          (or: . envsetup.sh)
#
#   Run it directly to do a full from-scratch build:
#     bash envsetup.sh [--cuda | --cpu]   default: auto-detect CUDA
#
# The script is safe to re-run — it skips steps that are already done.

# ── Resolve project root (works whether sourced or run directly) ──────────────
# Avoid set -u here: sourcing in zsh with rvm/conda active will fail on their
# unset variables.  set -euo pipefail is applied below, after the early return.

_shio_root() {
    # BASH_SOURCE[0] is populated in bash; fall back to $0 in zsh.
    local src="${BASH_SOURCE[0]:-$0}"
    local dir
    dir="$(cd "$(dirname "$src")" && pwd)"
    echo "$dir"
}

SHIO_ROOT="$(_shio_root)"
SHIO_BIN="$SHIO_ROOT/bin"

# ── Environment wiring (always runs, even when sourced) ───────────────────────

# Add ./bin to PATH for llama-server (and its shared libraries).
# The shio binary itself is installed to $HOME/.cargo/bin by `cargo install`.
case ":${PATH}:" in
    *":${SHIO_BIN}:"*) ;;
    *) export PATH="${SHIO_BIN}:${PATH}" ;;
esac

# Add ./bin to LD_LIBRARY_PATH for the llama.cpp shared libraries.
# Use ${VAR-} (dash, not colon-dash) so an unset var expands to "" without
# triggering nounset errors in the caller's shell.
_shio_ldp="${LD_LIBRARY_PATH-}"
case ":${_shio_ldp}:" in
    *":${SHIO_BIN}:"*) ;;
    *) export LD_LIBRARY_PATH="${SHIO_BIN}${_shio_ldp:+:${_shio_ldp}}" ;;
esac
unset _shio_ldp

# When sourced, stop here — env is wired, nothing else needed.
if [[ "${BASH_SOURCE[0]:-}" != "${0}" ]]; then
    echo "[shio] PATH and LD_LIBRARY_PATH updated (root: ${SHIO_ROOT})"
    return 0
fi

# ── Full build mode (only when run directly) ──────────────────────────────────
# Safe to enable strict mode now — we are in a subprocess, not the user's shell.

set -euo pipefail

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'
info()  { echo -e "${GREEN}[shio]${NC} $*"; }
warn()  { echo -e "${YELLOW}[shio] warning:${NC} $*"; }
die()   { echo -e "${RED}[shio] error:${NC} $*" >&2; exit 1; }
step()  { echo -e "\n${GREEN}══ $* ${NC}"; }

# ── Parse flags ───────────────────────────────────────────────────────────────

USE_CUDA=auto
for arg in "$@"; do
    case "$arg" in
        --cuda) USE_CUDA=yes ;;
        --cpu)  USE_CUDA=no  ;;
        --help|-h)
            echo "Usage: bash envsetup.sh [--cuda | --cpu]"
            echo "  --cuda  Force CUDA build of llama.cpp"
            echo "  --cpu   Force CPU-only build of llama.cpp"
            exit 0 ;;
        *) die "Unknown argument: $arg" ;;
    esac
done

# ── Dependency checks ─────────────────────────────────────────────────────────

step "Checking dependencies"

check_cmd() {
    if command -v "$1" &>/dev/null; then
        info "  $1 — found ($(command -v "$1"))"
    else
        die "$1 is required but not found. $2"
    fi
}

check_cmd git    "Install git: https://git-scm.com/"
check_cmd cmake  "Install cmake: https://cmake.org/download/"
check_cmd cargo  "Install Rust: https://rustup.rs/"

# ── CUDA detection ────────────────────────────────────────────────────────────

if [[ "$USE_CUDA" == "auto" ]]; then
    if command -v nvcc &>/dev/null || command -v nvidia-smi &>/dev/null; then
        USE_CUDA=yes
    else
        USE_CUDA=no
    fi
fi

if [[ "$USE_CUDA" == "yes" ]]; then
    info "  CUDA build selected"
    GGML_CUDA_FLAG="-DGGML_CUDA=ON"
else
    warn "  CPU-only build selected (no CUDA detected or --cpu given)"
    GGML_CUDA_FLAG="-DGGML_CUDA=OFF"
fi

# ── Submodule init ────────────────────────────────────────────────────────────

step "Initialising git submodules"
cd "$SHIO_ROOT"
git submodule update --init --recursive

# ── Build llama.cpp ───────────────────────────────────────────────────────────

LLAMA_SRC="$SHIO_ROOT/vendor/llama.cpp"
LLAMA_BUILD="$LLAMA_SRC/build"
LLAMA_SERVER="$LLAMA_BUILD/bin/llama-server"

if [[ -x "$LLAMA_SERVER" ]]; then
    info "llama-server already built — skipping (delete $LLAMA_BUILD to rebuild)"
else
    step "Building llama.cpp"
    cmake -B "$LLAMA_BUILD" \
          -S "$LLAMA_SRC" \
          $GGML_CUDA_FLAG \
          -DCMAKE_BUILD_TYPE=Release
    cmake --build "$LLAMA_BUILD" --parallel "$(nproc)"
fi

# ── Copy llama.cpp binaries and shared libs to ./bin/ ─────────────────────────

step "Installing llama.cpp binaries to ./bin/"
mkdir -p "$SHIO_BIN"

# Copy the server binary.
cp -v "$LLAMA_BUILD/bin/llama-server" "$SHIO_BIN/"

# Copy any shared libraries (.so files) produced by the build.
find "$LLAMA_BUILD" -maxdepth 3 -name '*.so*' | while read -r lib; do
    cp -v "$lib" "$SHIO_BIN/"
done

# ── Build shio ────────────────────────────────────────────────────────────────

step "Building shio"
cd "$SHIO_ROOT"
cargo install --path . --locked
info "shio installed to $(command -v shio 2>/dev/null || echo '$HOME/.cargo/bin/shio')"

# ── Smoke test ────────────────────────────────────────────────────────────────

step "Smoke test"
if shio --version &>/dev/null; then
    info "shio --version: $(shio --version)"
else
    warn "shio not found in PATH; ensure \$HOME/.cargo/bin is in your PATH"
fi

# ── Done ──────────────────────────────────────────────────────────────────────

echo
echo -e "${GREEN}╔══════════════════════════════════════════════════════╗${NC}"
echo -e "${GREEN}║  ShioRamen environment ready                         ║${NC}"
echo -e "${GREEN}╚══════════════════════════════════════════════════════╝${NC}"
echo
echo "  Wire your shell:    source envsetup.sh"
echo "  Download a model:   shio pull <hf-path or url>"
echo "  Start chatting:     shio chat --model ./models/<name>.gguf"
echo "  Check everything:   shio doctor"
echo
