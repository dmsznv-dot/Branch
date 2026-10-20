#!/usr/bin/env bash
set -euo pipefail

# This cloud checkout is already isolated; do not create another worktree.
export RUSTUP_HOME=/workspace/.toolchains/rustup
export CARGO_HOME=/workspace/.toolchains/cargo
export PATH="$CARGO_HOME/bin:$PATH"
export CARGO_BUILD_JOBS=3
version=1.99.1
host=x86_64-unknown-linux-gnu

if [[ "$(uname -s)" != Linux || "$(uname -m)" != x86_64 ]]; then
    echo 'This cloud setup script requires Linux x86_64.' >&2
    exit 1
fi

if [[ ! -x "$CARGO_HOME/bin/rustup" ]]; then
    mkdir -p "$RUSTUP_HOME" "$CARGO_HOME"
    download_dir=$(mktemp -d)
    trap 'rm -rf "$download_dir"' EXIT
    base_url="https://static.rust-lang.org/rustup/dist/$host/rustup-init"
    curl --proto '=https' --tlsv1.2 --fail --show-error --location \
        "$base_url" --output "$download_dir/rustup-init"
    curl --proto '=https' --tlsv1.2 --fail --show-error --location \
        "$base_url.sha256" --output "$download_dir/rustup-init.sha256"
    (cd "$download_dir" && sha256sum -c rustup-init.sha256)
    chmod +x "$download_dir/rustup-init"
    "$download_dir/rustup-init" --profile minimal --default-toolchain "$version" \
        --default-host "$host" --no-modify-path -y
fi

if ! rustup run "$version" rustc --version 2>/dev/null | grep -Eq "^rustc ${version//./\\.} "; then
    rustup toolchain install "$version" --profile minimal --component rustfmt --component clippy
fi
# Installing already-present components is a no-op and does not update Rust.
rustup component add --toolchain "$version" rustfmt clippy
rustup run "$version" rustc --version
rustup run "$version" cargo --version
cat <<'ACTIVATE'
For subsequent commands in this shell:
export RUSTUP_HOME=/workspace/.toolchains/rustup
export CARGO_HOME=/workspace/.toolchains/cargo
export PATH="$CARGO_HOME/bin:$PATH"
export CARGO_BUILD_JOBS=3
ACTIVATE
