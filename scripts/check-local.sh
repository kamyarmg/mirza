#!/usr/bin/env sh

set -eu

echo "[mirza] running cargo fmt --all -- --check"
cargo fmt --all -- --check

echo "[mirza] running cargo clippy --all-targets -- -D warnings"
cargo clippy --all-targets -- -D warnings

echo "[mirza] running cargo test"
cargo test