# Watch together — Rust core

An in-memory room authority, deterministic playback synchronization and a local
adapter that streams original bytes from one explicitly selected file. Rooms
support five participants and `everyone` / `host-only` playback controls.

For managed-cloud setup, run `./scripts/setup-core.sh`, then activate the
workspace-local toolchain as shown in [core development](docs/development/core.md).

```sh
cargo test --locked --workspace --all-features
cargo run --locked -p media-stream --bin core-harness -- --file /path/to/selected.mp4 --duration-ms 120000
```

The local harness prints `room-created`, then `range-server-ready` only after an
internal authenticated HTTP HEAD succeeds. Press Ctrl+C for `room-closed` and
joined shutdown. It prints no file path, capability or playable URL. This is a
local development check; no external service is started.

See [core development](docs/development/core.md) for environment setup, the full
validation commands and acceptance limits. WebRTC/TURN transport, native
libmpv/Tauri integration and distribution bundles belong to follow-up projects
B/C/D. The current tests do not establish native playback or a real-network
synchronization result of ≤250 ms.
