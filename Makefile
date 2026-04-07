.PHONY: all build release test clean install check doc fmt ci watch

all: test

build:
	cargo build

release:
	cargo build --release
	@echo "Release binary: target/release/blossom-lfs"

test:
	cargo test
	@echo "All tests passed"

clean:
	cargo clean
	rm -f *.log

install: release
	cargo install --path .

check:
	cargo clippy -- -D warnings
	cargo fmt -- --check
	@echo "Code quality checks passed"

doc:
	cargo doc --open

ci: check test release
	@echo "CI checks complete"

watch:
	cargo watch -x test -x build

fmt:
	cargo fmt
