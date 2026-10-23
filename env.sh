# Source this file: `. ./env.sh` — activates project-local toolchains.
A11Y_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export GOROOT="$A11Y_ROOT/.toolchain/go"
export GOPATH="$A11Y_ROOT/.toolchain/gopath"
export GOMODCACHE="$GOPATH/pkg/mod"
export GOTOOLCHAIN=local
export RUSTUP_HOME="$A11Y_ROOT/.toolchain/rustup"
export CARGO_HOME="$A11Y_ROOT/.toolchain/cargo"
export PATH="$GOROOT/bin:$GOPATH/bin:$CARGO_HOME/bin:$PATH"
# Use the whole 16-thread / 30 GB host for builds and tests, but at low CPU and
# I/O priority so the owner's live desktop and screen reader always win.
# Builds share one cargo lock, so parallel agents queue rather than multiply.
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-14}"
export RUST_TEST_THREADS="${RUST_TEST_THREADS:-14}"
renice -n 10 -p $$ >/dev/null 2>&1 || true
ionice -c 2 -n 7 -p $$ >/dev/null 2>&1 || true
export VIRTUAL_ENV="$A11Y_ROOT/.toolchain/venv"; export PATH="$VIRTUAL_ENV/bin:$PATH"
