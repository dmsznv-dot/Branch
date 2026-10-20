# Core development

Use the existing `/workspace/Branch` checkout in the cloud environment; do not
create a worktree. The repository pins Rust 1.99.1 with rustfmt and Clippy in
`rust-toolchain.toml`. In this managed Linux environment activate the installed
persistent toolchain before every new shell:

```sh
export RUSTUP_HOME=/workspace/.toolchains/rustup
export CARGO_HOME=/workspace/.toolchains/cargo
export PATH="$CARGO_HOME/bin:$PATH"
export CARGO_BUILD_JOBS=3
cd /workspace/Branch
rustup show active-toolchain
rustup component add --toolchain 1.99.1 rustfmt clippy
cargo build --locked --workspace --all-targets
```

For an empty toolchain store, install the pinned release with
`rustup toolchain install 1.99.1 --profile minimal --component rustfmt --component clippy`.
Keep the managed HTTPS proxy and CA environment intact. Cargo verifies package
checksums; rustup retains its signature and checksum checks. No application
credentials, VPN, external services or public listener are required.

Run the same core checks as CI:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-features
cargo build --locked --workspace --all-targets
```

`--all-features` is required for the sync engine's ManualClock tests and
media-stream's deterministic lifecycle/backpressure probes. Those probes and
chunk-allocation counters are absent from default production builds. To focus
on the final lifecycle suite:

```sh
cargo test --locked -p media-stream --all-features --test revocation --test backpressure --test workflow
```

The body is pulled by the consumer, with no producer task or queue. The counters
track allocated chunks until the final `Bytes` owner is dropped. Tests pause
between chunks before the next source read, revoke room/session access and
verify that later reads and yields are suppressed. Already delivered socket
bytes cannot be recalled. Separate pull-consumer tests check that holding one
chunk allocates one chunk, rather than reading the entire selected file.

`RoomHandle::close` authenticates the host session. `CreatedRoom.owner` is a
nonserializable, room-bound, close-only capability for local lifecycle ownership;
it remains usable after host session expiry. Dropping legacy `CreatedRoom`
fixtures does not close their rooms. `LocalRoomSession` retains this capability,
creates the room from the selected descriptor before starting its adapter, and
explicitly shuts down in this order: revoke room authority, cancel readers, join
HTTP tasks, release the selected source. The composed wait uses one two-second
deadline and returns `ShutdownTimeout` if exceeded; abort cleanup still joins
owned tasks before returning. No shutdown guarantee depends on a guest's TTL.

For a reproducible local smoke check, generate a temporary fixture and run:

```sh
python3 - <<'PY'
from pathlib import Path
Path('/tmp/watch-together-core-fixture.bin').write_bytes(bytes(range(256)) * 1024)
PY
cargo run --locked -p media-stream --bin core-harness -- --file /tmp/watch-together-core-fixture.bin --duration-ms 10000
```

Wait for `range-server-ready`, which follows a real authenticated HEAD with the
exact Host, private capability header and expected Content-Length. Press Ctrl+C;
expect `room-closed` and a successful exit. Repeat the same command and shutdown
once more. The three stdout events are the only public readiness/shutdown
output. Errors are generic to avoid exposing paths, input or capabilities. A PID
or open port alone does not demonstrate readiness; do not copy a token from logs
or create user-facing localhost links. Delete the temporary fixture when done.
Processes do not survive an environment restart: rerun the harness in the
existing checkout when needed. Dependencies and build outputs can persist.

The CI matrix runs these Rust core checks on Linux, Windows and macOS
(`macos-14`). Only Linux has been executed in this cloud task. A green matrix
checks portability of the Rust core; native UI, decoder integration and packaged
macOS ARM64 / Windows x64 acceptance remain separate gates. The >4 GiB sparse
file test executes on Linux after checking the fixture allocation; other OSes
explicitly ignore it until platform-specific sparse allocation is validated.
ManualClock projections and local HTTP workflows do not measure real decoder
synchronization. Stable real-network ≤250 ms acceptance, WebRTC/TURN, native
libmpv/Tauri and signed bundle checks remain for B/C/D.
