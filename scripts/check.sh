#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo test -p tidkod --example ltc_in --locked
python3 scripts/check_ltc_dependencies.py
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --locked
buf lint
buf format --diff --exit-code
buf build -o /tmp/tidkod-descriptor.bin
python3 scripts/generate_docs.py --check
python3 scripts/smoke_examples.py
