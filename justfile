# Common tasks. Run `just` to list them.

[private]
default:
    @just --list --unsorted

# Build and run typ from source; extra arguments are passed through
run *args: build
    @./target/debug/typ {{ args }}

# Build the typ binary (debug)
build:
    cargo build -q -p typ-rs

# Build the typ binary (release)
release:
    cargo build --release -p typ-rs

# Run the whole test suite
test:
    cargo test --workspace

# Format all code
fmt:
    cargo fmt --all

# Lint with clippy; warnings are errors
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# Format check, lint, and test everything, as before a commit
check:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace

# Run the simulator (release); extra arguments are passed through, e.g. --learner awkward
sim *args:
    cargo run -q --release -p typ-sim -- {{ args }}

# Run the simulator's integration tests with optimizations on
sim-gate:
    cargo test --release -p typ-sim

# Build and open the API docs for the workspace crates
doc:
    cargo doc --no-deps --workspace --open

# Install typ into ~/.cargo/bin from this checkout
install:
    cargo install --path crates/typ-rs

# Remove the installed typ
uninstall:
    cargo uninstall typ-rs

alias r := run
alias b := build
alias t := test
alias c := check
