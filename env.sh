# Source this file: `. ./env.sh` — activates project-local toolchains.
A11Y_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export GOROOT="$A11Y_ROOT/.toolchain/go"
export GOPATH="$A11Y_ROOT/.toolchain/gopath"
export GOMODCACHE="$GOPATH/pkg/mod"
export GOTOOLCHAIN=local
export RUSTUP_HOME="$A11Y_ROOT/.toolchain/rustup"
export CARGO_HOME="$A11Y_ROOT/.toolchain/cargo"
export PATH="$GOROOT/bin:$GOPATH/bin:$CARGO_HOME/bin:$PATH"
# Several agents build in parallel on a 16-thread / 30 GB host; unbounded
# parallel linking exhausts memory. Override per shell if needed.
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}"
export RUST_TEST_THREADS="${RUST_TEST_THREADS:-6}"
export VIRTUAL_ENV="$A11Y_ROOT/.toolchain/venv"; export PATH="$VIRTUAL_ENV/bin:$PATH"
