# Branch desktop player and watch rooms

This standalone Tauri workspace renders libmpv below a transparent sibling
WebView in one parent window. macOS ARM64 uses NSView/NSOpenGLContext/CGL;
Windows x64 uses a child HWND/own DC/WGL. Linux exposes an unavailable surface
and still builds/tests the API and UI. There is no HTML video fallback.

Use Node 24.19.0/npm 11.9.0 and the repository's Rust 1.99.1. Install the
pinned **patched** libmpv 0.41.0 SDK described in
[the native development guide](../../docs/development/native-player.md).
Both source patches in `scripts/native/patches` are required on both target
OSes; an arbitrary system mpv installation does not satisfy this prerequisite.
Target build/bundle recipes and clean-machine gates belong to Task4.

```sh
npm ci --prefix apps/desktop
npm --prefix apps/desktop test -- --run
npm --prefix apps/desktop run typecheck
npm --prefix apps/desktop run build
cargo test --locked --manifest-path apps/desktop/src-tauri/Cargo.toml --lib
cargo test --locked --manifest-path apps/desktop/src-tauri/Cargo.toml --lib --features ipc-test
cargo check --locked --manifest-path apps/desktop/src-tauri/Cargo.toml
npm --prefix apps/desktop run tauri -- dev
```

For Linux development first run `PLAYER_TOOLCHAIN_ROOT=... scripts/setup-player-linux.sh
--install`, then source `target/player-linux-activate.sh`. The script records
actual GTK/WebKit/librsvg/OpenSSL/CMake/pkg-config and distro libmpv versions in
`target/desktop-linux-readiness.json`. Distro libmpv is 0.40.0-3+deb13u1; playback
uses the separately built pinned upstream 0.41.0/API2.5 with both exact patches.
No target OS libraries are installed by this Linux script. Pure API/geometry/
shutdown tests also run with `--no-default-features`, without GTK/libmpv.

The production CLI build enables `custom-protocol` and embeds local UI assets;
`npm --prefix apps/desktop run tauri -- build --no-bundle` builds that executable.
The dev server binds only `127.0.0.1:1420`; its WebSocket is permitted only by
`devCsp`. Production script policy is self-only, and navigation rejects remote
origins. No shell, fs, HTTP or process plugins are installed. Only the main
window's typed app commands and event listen/unlisten are granted. File
selection, descriptor ownership, active generation and native geometry stay in
Rust. Metadata is React text, counters and IDs use strings, and player commands
expose no paths or arbitrary mpv arguments.

The watch panel starts the authoritative signaling server inside the host's
Branch process, opens TCP port 8787 through UPnP on the router, discovers its
public IPv4 address, and creates a short-lived TLS certificate. The certificate
is pinned through the invitation; no separate server program, TLS proxy, or
account is used. If the router does not support UPnP or the ISP uses CGNAT,
direct hosting is unavailable and the app reports that it could not create the
server. macOS may ask to allow incoming connections. A guest opens a local copy
of the same file before pasting the invitation. Branch hashes the whole file
and rejects a mismatch. The server relays playback messages only; it does not
receive video.

Opening another film supersedes a pending load without waiting for FILE_LOADED.
Cancelling the chooser retains the current selection. Generation/selection
checks protect both command execution and late open replies. Close cancels
pending Rust requests/source reads, joins the renderer with its GL context
current, then releases the native hierarchy on the UI thread. A five-second
shutdown timeout keeps live ownership and prevents parent-window destruction;
it is a failure state, and the OS may be used to terminate that stuck process.

The panel is explicitly shown/hidden for native composition and focus checks.
Auto-hide and the full hotkey set remain B2 work. This proof provides local
playback only. Actual visible frames, transparent-panel clicks/focus, aspect
ratio, minimize/restore, ten fullscreen cycles, mixed-DPI moves, hardware/software
decode and clean-target bundles must still be checked on **both** target
machines. Linux-hosted cross-target Rust checks do not execute those gates.
