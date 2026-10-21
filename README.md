# Branch — watch together prototype

An in-memory room authority, deterministic playback synchronization and a local
adapter that streams original bytes from one explicitly selected file. Rooms
support five participants and `everyone` / `host-only` playback controls.

For managed-cloud setup, run `./scripts/setup-core.sh`, then activate the
workspace-local toolchain as shown in [core development](docs/development/core.md).

```sh
cargo test --locked -p shared-protocol -p sync-engine -p room-server -p media-stream --all-features
cargo run --locked -p media-stream --bin core-harness -- --file /path/to/selected.mp4 --duration-ms 120000
```

The local harness prints `room-created`, then `range-server-ready` only after an
internal authenticated HTTP HEAD succeeds. Press Ctrl+C for `room-closed` and
joined shutdown. It prints no file path, capability or playable URL. This is a
local development check; no external service is started.

See [core development](docs/development/core.md) for environment setup, the full
validation commands and acceptance limits. WebRTC/TURN transport and real decoder synchronization remain follow-up
projects C/D. The B1 local libmpv/Tauri proof is implemented; target delivery
and native acceptance remain blocked. The current tests do not establish native playback or a real-network
synchronization result of ≤250 ms.

The [native player proof (B1)](docs/development/native-player.md) includes a typed
selected-file engine, native surface hosts and a minimal local desktop UI. See
[native validation](docs/development/native-player-validation.md) for observed
Linux checks, blocked target source/bundle inventories, and the mandatory macOS
ARM64/Windows 10/11 runtime and clean-machine gates. The application is not yet a
finished shared-viewing MVP; B2/C/D follow only after the required gates.
