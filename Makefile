# gorca developer entry points. Plain make, no extra tools. The project-local toolchain is
# activated by sourcing env.sh in every recipe (see CLAUDE.md: never use a global toolchain).
SHELL := /bin/bash
ENV   := . ./env.sh &&

.PHONY: check ext fmt fmt-check clippy test deps build release bench run clean

# The gates from CLAUDE.md, plus the ADR-0001 dependency rules. All must pass.
check: fmt-check clippy ext test deps

# The shipped extensions as wasm32-wasip2 components, laid out beside the debug and release
# binaries (`target/<profile>/extensions`), where the daemon finds them by default.
ext:
	$(ENV) cargo xtask build-ext

fmt:
	$(ENV) cargo fmt --all

fmt-check:
	$(ENV) cargo fmt --all --check

clippy:
	$(ENV) cargo clippy --workspace --all-targets -- -D warnings

test:
	$(ENV) cargo test --workspace

deps:
	./tools/check-deps.sh

build:
	$(ENV) cargo build --workspace

release: ext
	$(ENV) cargo build --release -p gorca-daemon

bench:
	$(ENV) cargo bench -p gorca-bench

run:
	$(ENV) cargo run -p gorca-daemon

clean:
	$(ENV) cargo clean
