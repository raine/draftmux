set positional-arguments
set shell := ["bash", "-euo", "pipefail", "-c"]

# List available commands
default:
    @just --list

# Run cheap read-only checks in parallel
check-fast-readonly:
    @checkle run fast-readonly

# Run commit-time checks without mutating files
pre-commit: check-fast-readonly

# Run clippy and fail on any warnings
clippy:
    @checkle run clippy

# Auto-fix clippy warnings
clippy-fix:
    cargo clippy --workspace --fix --allow-dirty --target-dir target/clippy --all-targets -- -D warnings -W clippy::all

# Configure Git to use the repository hooks
install-hooks:
    scripts/install-git-hook-shims

# Install local tools used by quality gates
install-quality-tools:
    cargo install checkle cargo-machete
