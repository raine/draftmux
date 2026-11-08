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

# Build target/release/Draftmux.app, ad-hoc signed
app:
    scripts/bundle

# Build target/debug/Draftmux.app, ad-hoc signed
app-debug:
    scripts/bundle --debug

# Build and install Draftmux.app in ~/Applications and link ~/.local/bin/draftmux
install-app *ARGS:
    scripts/install "$@"

# Open a debug build of this checkout on its own data directory under /tmp
dev-app:
    scripts/dev-app

# Build a signed, notarized release disk image (needs Apple credentials)
release-macos:
    scripts/release-macos

# Print the Homebrew cask for a release disk image
homebrew-cask VERSION CHECKSUM_FILE URL HOMEPAGE:
    scripts/homebrew-cask "$@"

# Regenerate macos/dmg from background.svg (needs rsvg-convert and uv)
dmg-layout:
    scripts/generate-macos-dmg-layout

# Test the build, install and release scripts without cargo or credentials
test-scripts:
    scripts/test-assemble-macos-app
    scripts/test-install
    scripts/test-create-macos-dmg
    scripts/test-verify-macos-release
    scripts/test-homebrew-cask
    scripts/test-release-credentials

# Lint the shell scripts and hooks
shellcheck:
    shellcheck scripts/* hooks/*

# Configure Git to use the repository hooks
install-hooks:
    scripts/install-git-hook-shims

# Install local tools used by quality gates
install-quality-tools:
    cargo install checkle cargo-machete
