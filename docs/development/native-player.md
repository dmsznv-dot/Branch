# Native player development

Task 2 embeds mpv 0.41.0 (client API 2.5) in the Rust process. It uses only an explicitly selected, already-open descriptor, exposed through an exact-match `branchmedia` registration. No external mpv process or HTML video is used for playback. `native-mpv` is optional; the existing core-only workflow remains usable without native libraries.

Linux headless/API tests are development evidence. macOS Apple Silicon and Windows x64 surfaces, hardware decoding, transparent WebView composition and clean-machine bundles remain separate native gates. Both target machines exist; this work has not run on them.

## Linux setup

Run `scripts/setup-core.sh` and activate Rust as its output describes. On Debian 13 x86_64:

```bash
scripts/setup-player-linux.sh --install
source target/player-linux-activate.sh
scripts/generate-player-fixtures.sh
cargo test --locked -p media-engine --features native-mpv --test native_decode -- --test-threads=1
cargo run --locked -p media-engine --features native-mpv --bin media-probe -- \
  --file target/player-fixtures/tracks.mkv --headless
```

Setup writes only its rootless SDK and repository `target` directory. Default SDK root is `target/player-toolchain`; `PLAYER_TOOLCHAIN_ROOT` can select an existing SDK root. It uses signed Debian HTTPS APT metadata, pinned package versions and archive hashes, extracts packages without sudo, verifies immutable upstream mpv commit `41f6a645068483470267271e1d09966ca3b9f413` plus the exact hashed downstream patch described below, and builds shared libmpv with Lua compiled in but every user and bundled script disabled by runtime policy. Unsupported API, options, tool hashes or source revision fail explicitly. Unavailable old package pins require an intentional manifest update; checks are never bypassed.

Some development `.so` symlinks need the existing system runtime with the same SONAME. Setup completes those links inside the SDK and records them in `target/native-linux-links.json`; they are development links and must never be used as target bundle contents. Completing these before Meson dependency resolution avoids accidentally selecting static `.a` libraries. Reconfiguration clears Meson's dependency cache.

`--verify` checks cached archive hashes, pinned FFmpeg/ffprobe binary hashes, the mpv source revision and exact expected patched diff, API, exact patched version and all headless policy options, then writes source/patch/artifact provenance and results to `target/native-linux-readiness.json`. It generates `target/player-linux-activate.sh`; there is no dependency on session-specific shared files. The install and verify modes were both executed successfully with `PLAYER_TOOLCHAIN_ROOT=/workspace/.toolchains`; verify also passed with an otherwise empty environment containing only `/usr/bin:/bin`. This reuses the prepared SDK and is not a fresh-machine replay claim.

The observed Linux FFmpeg is **7.1.5-0+deb13u1**, different from target source pin **n8.0**. Exact Debian archive hashes, observed tool/native artifact hashes, upstream mpv revision, configure flags and license identifiers are in [native-dependencies.json](native-dependencies.json). Target entries intentionally block distribution until their full native inventory is produced and checked.

## Fixtures and probe evidence

The generator's executable commands are the fixture recipe; it requires exactly the recorded Debian FFmpeg build and does not substitute encoders. It creates 160×90, 24 fps, three-second synthetic CC0 material in `target/player-fixtures`:

| Fixture | Container / video | Additional content |
| --- | --- | --- |
| `h264.mp4` | MP4 / H.264 | Asserts `moov` follows `mdat` |
| `h264.mov` | MOV / H.264 | QuickTime container |
| `tracks.mkv` | Matroska / H.264 | Two AAC audio tracks and embedded SRT |
| `hevc.mp4` | MP4 / HEVC | Software decode |
| `vp9.mkv` | Matroska / VP9 | Software decode |
| `av1.mkv` | Matroska / AV1 | Software decode |

`manifest.json` records every fixture's SHA256, CC0 license, generator hash and exact FFmpeg version. Binary movies are not committed. FFmpeg and its codec libraries keep their separate licenses; CC0 describes the synthetic media only.

`media-probe --file FILE --headless` uses `vo=null`, `ao=null`, `hwdec=no`. Within ten seconds it requires observed nonzero `video-params/w` and `video-params/h`, `estimated-frame-number >= 2`, and actual `time-pos >= 100ms`. The frame property is mpv's frame estimate, not a literal decoded-frame callback counter. Elapsed time alone is never a pass. The probe reads duration/tracks, verifies pause, exact absolute seek and all six speeds, then joins shutdown. Its output contains fixed event names, stable error codes and validated codec/numeric fields; it never emits selected paths, URIs, titles or raw decoder logs. Tests run probes as children with a twelve-second outer deadline and kill/reap failures.

All six fixture probes passed software decode and controls on the prepared Linux mpv/FFmpeg build. The mandatory native tests fail on missing libmpv, wrong API or unavailable fixtures; generate fixtures before running them.

## Scaler padding correction and Linux render evidence

The real `native_render::linux_offscreen_render_lifecycle` test creates a surfaceless EGL 1.5/OpenGL 3.3 context, passes an owned texture FBO to `MpvRenderer`, and asserts nonblack RGB from `glReadPixels` alongside decoded dimensions/frame/time and GL_NO_ERROR. It frees the renderer before GL teardown and joins engine shutdown. A child process has a twelve-second deadline. This exercises Linux development rendering only; CGL/WGL, visible default-window framebuffers, WebView composition and target bundles remain untested.

Unpatched mpv 0.41.0 intermittently produced black RGB with alpha255 despite decoded 160×90 frames, frame estimate71, position2958ms, valid GL calls and no GL error. Diagnosis traced this to `video/out/gpu/video.c`: a six-tap filter uses an eight-float padded LUT row, but `mp_compute_lut` initializes only six coefficients before the complete allocation is uploaded. Nonfinite heap padding contaminates chroma interpolation. Controlled diagnostic interposition found finite useful coefficients and nonfinite padding; poisoning only padding reproduced the genuine pixel assertion failure. Poisoning then zeroing that same padding passed **3/3** controls with finite intermediates. Those diagnostic probes are not acceptance runs.

The checked-in [patch](../../scripts/native/patches/mpv-0.41.0-zero-lut-padding.patch) changes only `talloc_array` to `talloc_zero_array` for this allocation. It is a local downstream correction; no upstream merge is claimed. Patch SHA256: `6b06d553c42ff0483dad3b5eb065cd7d22620020bee3b4bb79922e388adc64ea`. The immutable base and patch must also be used by eventual macOS/Windows dependency source builds; their manifest entries record this obligation and remain blocked pending target builds/inventory.

[Source verification](../../scripts/native/verify-mpv-source.py) computes the expected complete diff with a disposable Git index and compares it byte-for-byte with the actual diff. It rejects other tracked changes, staged changes, untracked files (including ignored files), and assume-unchanged/skip-worktree flags. Installation applies the patch only to the pristine base, then verifies it; repeated installation accepts only the exact already-patched tree. Meson artifacts live in a separate build directory. TLS, signed APT metadata, archive/tool checksums and API checks remain enabled. The accepted patched version is exactly `mpv v0.41.0-dirty`; arbitrary version variants are rejected.

Actual recovery checks on 2026-10-21, with `PLAYER_TOOLCHAIN_ROOT=/workspace/.toolchains`:

```bash
# --install performed signed APT checks, hash verification, patch application,
# rootless Meson rebuild/install and native API/policy verification.
PLAYER_TOOLCHAIN_ROOT=/workspace/.toolchains scripts/setup-player-linux.sh --install
env -i PATH=/usr/bin:/bin PLAYER_TOOLCHAIN_ROOT=/workspace/.toolchains \
  scripts/setup-player-linux.sh --verify
source target/player-linux-activate.sh
cargo test --locked -p media-engine --features native-mpv
# Then five additional invocations of:
cargo test --locked -p media-engine --features native-mpv --test native_render -- --nocapture
python3 -B -m unittest discover -s scripts/tests -p test_verify_mpv_source.py
```

The genuine uninstrumented test failed before patching with `no_rendered_pixels`. After rebuild, the full native suite passed **30/30**, including native decode/security6 and rendered pixels1, followed by **5/5** additional pixel passes (0.83–0.87s). No LD_PRELOAD or diagnostic interposition was present in these acceptance runs. The five real-Git source-integrity tests passed, including deliberate hash/revision, tracked/staged/untracked/ignored and hidden-index tampering. Both setup modes passed; `--verify` used only the empty environment shown above and needed no ephemeral shared activation. APT reported a nonfatal permission warning for `/etc/apt/apt.conf.d/80-applied-apt-retries`; signed metadata and archive verification still completed.

Observed rebuilt Linux `libmpv.so.2.5.0` SHA256: `a35129a76f10415c968feea58e42a81a03072394494f7758f0a9c3cc309e9df2`; API2.5, exact patched mpv version above, FFmpeg7.1.5-0+deb13u1. This is a local build artifact hash, not a claim that all hosts produce identical binaries. Target rendering and clean-machine acceptance remain separate gates.

## Commands, callbacks and shutdown

`EngineHandle::start`, `open`, `command` and consuming `shutdown` are async. `subscribe` returns a latest-state Tokio watch receiver; the command channel has exactly 64 entries and returns `Busy` on overflow. Only one primary core may be live in the process: another `start` returns `Busy`, and the permit remains held until actual native destruction, including after a shutdown timeout. This avoids mpv 0.41's process-global FFmpeg logging callback reset when overlapping cores terminate; additional render clients of the same core are allowed. The worker owns the event handle, uses `loadfile`'s returned playlist-entry ID plus start/end event IDs to correlate the current generation, and discards stale commands/events. Internal reducer tests inject delayed old completion, error and position events. Unknown track IDs, volume above 100 and invalid numeric values fail validation. Known-duration seeks clamp to duration; unknown-duration seeks retain the request.

`diagnostics(&self) -> DecodeDiagnostics` adds a narrow synchronous Rust-only snapshot with `width: u32`, `height: u32`, `estimated_frame_number: u64`, `failure: Option<EngineError>`. It exposes no arbitrary property-name getter.

Each read copies at most 65,536 bytes through a scratch buffer; any source error or cancellation discards the scratch data. The source may have modified its destination before failing. Seek/size conversions are checked, EOF returns zero, failure returns a negative native result, and panics cannot unwind across callbacks. `cancel_fn` only signals cancellation and takes no source/file/cursor lock or worker/UI wait. A stream close frees its cookie exactly once. The registration remains owned until the final core destruction joins native callbacks.

Shutdown signals source cancellation first, waits for the external renderer to be freed, destroys the primary mpv handle/core, then releases the registry/file and joins the worker. `MpvRenderer` is neither Send nor Sync. `unsafe MpvRenderer::create(endpoint, resolver)` requires a genuine GL resolver and compatible current GL context on the creation thread, kept valid/current for render, swap reporting and Drop. Task 3's native surface must own that invariant. Drop the renderer and join its thread before awaiting engine shutdown. Headless requires no endpoint; `take_render_endpoint` transfers a native client exactly once.

Unused endpoints are returned to the worker for destruction, so potentially blocking native destruction is not performed on the async shutdown caller. There is one five-second shutdown deadline. A still-held render endpoint or an indefinitely blocked OS file read returns `ShutdownTimeout`. The worker retains its core, source and registration until the native work actually exits; it does not free live callback data. A blocked regular-file syscall cannot be forcibly interrupted by a cancellation token, so this failure can retain a thread/memory until process exit. Probe processes provide an outer isolation boundary. Tests cover a cancellation-aware stalled reader and a real held render-client deadline followed by safe worker completion after endpoint release; they do not claim to forcibly cancel a blocked OS read.

## Resource policy evidence

Before init/load, policy disables configs, user and bundled scripts, IPC socket, disk caching, neighboring subtitles/audio and input bindings. It forces `demuxer=lavf`, `access-references=no`, and a lavf `format_whitelist` of only `mov,matroska`. The mpv option parser needs `%12%mov,matroska`; unsupported escaping is a hard failure. EBML sources get no MOV-only options; MOV/MP4 loads additionally set `enable_drefs=0,use_absolute_path=0`.

The actual negative test uses Linux IN_OPEN monitoring and an ephemeral loopback HTTP listener. Positive controls outside the locked decoder prove that unlocked FFmpeg follows a crafted MOV Macintosh alias dref to the spy file, unlocked mpv's native MKV demuxer follows an ordered-chapter SegmentUID to a sibling spy, and the HTTP spy records a real connection. Production opens neither spy and makes no HTTP connection. M3U/PLS disguised as `.mp4`, with file/HTTP/unselected `branchmedia` references, fail `UnsupportedMedia`. Direct ABI tests separately prove exact registered-URI matching, bounds, EOF and cancellation. Config, script and neighboring SRT opens are monitored and remain absent.

Resource-policy and decoder outcomes are checked separately. The crafted MOV reference can parse its own video parameters but fail subsequent playback controls; `DecoderFailed` for that synthetic negative input is not a resource-policy failure. Unknown video codec gives `UnsupportedMedia`; a changed selected descriptor gives `SourceChanged`; missing primary selection gives `Unavailable`. Deleting a selected pathname preserves the opened descriptor and succeeds.

Linux IN_OPEN evidence does not establish file-access policy on macOS/Windows; target resource tracing is still required. This is a fail-closed decoder policy, not an operating-system sandbox, and no native render/bundle acceptance is claimed.
