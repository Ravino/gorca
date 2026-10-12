# gorca developer entry points. Plain make, no extra tools. The project-local toolchain is
# activated by sourcing env.sh in every recipe (see CLAUDE.md: never use a global toolchain).
SHELL := /bin/bash
ENV   := . ./env.sh &&

.PHONY: check fmt fmt-check clippy test deps build release bench run clean

# The gates from CLAUDE.md, plus the ADR-0001 dependency rules. All must pass.
check: fmt-check clippy test deps

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

release:
	$(ENV) cargo build --release -p gorca-daemon

bench:
	$(ENV) cargo bench -p gorca-bench

run:
	$(ENV) cargo run -p gorca-daemon

clean:
	$(ENV) cargo clean
