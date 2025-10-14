#!/bin/sh

set -eu

ROOT_DIR=$(realpath $(pwd))
CARGO_HOME="${ROOT_DIR}/.cargo"
export CARGO_HOME
RUSTUP_HOME="${ROOT_DIR}/.rustup"
export RUSTUP_HOME
export RUSTUP_INIT_SKIP_PATH_CHECK="yes"
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs > rustup.sh
sh rustup.sh --default-host x86_64-unknown-linux-gnu \
    --default-toolchain stable \
    --no-modify-path \
    --profile default \
    -y
export PATH="${CARGO_HOME}"/bin/:"$PATH"

cargo fmt --all -- --check
cargo clippy --all-features --tests
cargo test
cargo test --release

# Some very rudimentary checks.
cd example
shellcheck harness.sh
cargo run -- b -c first
cargo run --release -- b -c second
cargo run --release -- l
cargo run --release -- d 0 1
