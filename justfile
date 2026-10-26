# mejl tasks. Run `just` to list them.

# List the recipes.
default:
    @just --list

# Build the debug binary.
build:
    cargo build

# Build the release binary.
release-build:
    cargo build --release --locked -p mejl

# Run mejl with any arguments, for example `just run doctor`.
run *args:
    cargo run -- {{args}}

# Try mejl without an account, on built-in demo data.
demo:
    cargo run -- --demo

# Run every test.
test:
    cargo test --workspace

# Format the code.
fmt:
    cargo fmt --all

# Lint with every warning as an error.
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# Check the oldest supported Rust.
msrv:
    cargo +1.88 check --workspace --all-targets

# The gate to pass before handing off: format, lint, test.
check:
    cargo fmt --all --check
    just lint
    just test

# Check the key setup in your config.
keys:
    cargo run -- keys --check

# Needs cargo-edit (`cargo install cargo-edit`).
# Set every crate's version and the internal dependencies on them: `just bump 0.3.0`.
bump version:
    cargo set-version --workspace {{version}}

# Package every crate and build mejl as crates.io would, without publishing.
publish-check:
    cargo publish --workspace --dry-run

# Tag a release and push the tag, which starts the release workflow. The
# version must match Cargo.toml, as in `just release 0.1.0`.
release version: check
    #!/usr/bin/env sh
    set -eu
    current=$(cargo pkgid -p mejl | sed 's/.*[#@]//')
    if [ "$current" != "{{version}}" ]; then
        echo "Cargo.toml says $current, not {{version}}" >&2
        exit 1
    fi
    if [ -n "$(git status --porcelain)" ]; then
        echo "commit or stash your changes first" >&2
        exit 1
    fi
    git tag -a "v{{version}}" -m "mejl {{version}}"
    git push origin "v{{version}}"
