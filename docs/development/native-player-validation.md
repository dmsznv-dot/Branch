# Native player proof validation

B1 acceptance is **BLOCKED**, not complete. The embedded player is implemented
and reviewed through commit `3b455b3`; this document separates observed Linux
checks from target builds, runtime acceptance and delivery. The user has both
required OS machines, but no remote access or native results were supplied.
B2/C/D have not been represented as working.

| Evidence class | Observed outcome | Limit |
| --- | --- | --- |
| Linux native engine | 33 passed, including seven real decoder/security tests and real EGL RGB readback | Development libmpv 0.41.0 with two exact patches; Debian FFmpeg 7.1.5, not target FFmpeg 8 |
| Linux desktop API/ACL | 11 passed; pure Rust 10 passed; actual generated Tauri capability rejects other windows, remote origin and generic commands | Native surface deliberately unavailable on Linux |
| UI | 5 passed; typecheck and production build passed | Browser DOM checks, not native composition |
| Core A | 116 passed; default workspace 127 passed; Clippy/fmt/build passed | No target GUI acceptance implied |
| Target Rust branches from Linux | macOS ARM64 and Windows MSVC cargo check passed with real LLVM host tools | No target SDK link, application launch or GPU execution |
| Task4 parser/admission | Python tests exercise synthetic Mach-O and PE bytes, including delay imports | A positive synthetic fixture is not a production bundle |
| Target SDK and bundles | BLOCKED: source closure/toolchain, artifact hashes, licenses and fixture CI inventory incomplete | Target source entries remain false/incomplete and recipes reject before download |
| CI execution | Workflow authored; no remote run observed | A checked-in workflow is not executed CI evidence |

Task3 logs and test counts are in the development report used during review.
The final task report records the exact Task4 commands and actual counts.

## Source and distribution gates

`native-dependencies.json` contains separate Linux and target entries. Linux
libraries are development evidence and cannot be copied into a target bundle.
Target primary sources are fixed to mpv commit
`41f6a645068483470267271e1d09966ca3b9f413` (v0.41.0), and FFmpeg commit
`140fd653aed8cad774f991ba083e2d01e86420c7` (n8.0). The latter was resolved from
upstream's annotated tag object `a4044e04486d1136022498891088a90baf5b2775` and
its peeled commit by `git ls-remote` on 2026-10-21.

Both mpv patches are required; their hashes are verified against the checked-in
files before any source build. `verify-mpv-source.py` admits the exact patched
base, not a similarly named prebuilt DLL. Source SDK scripts use shared builds,
Meson `--wrap-mode=nofallback`, explicit installed prefix, three build jobs, and
out-of-source build directories. They do not install target libraries on Linux.
Every non-mpv checkout must have a pristine index and working tree, including
no ignored/untracked files and no assume-unchanged or skip-worktree hints.
The check is read-only; generated outputs belong in the separate build directory.

The following are intentionally unresolved: libass/font/shaping closure,
libplacebo/GPU compiler closure, Lua 5.2 shared build, dav1d AV1 software path,
toolchain/runtime redistributable provenance, target artifact hashes/imports,
notices/corresponding-source inventory and native CI fixture generation. Empty
`build_order`, `runtime_files`, `toolchain`, `artifacts`, `dependency_graph` and
false completeness fields are blocking evidence, not placeholders that may be
set to true to bypass the gate.

To finish a source lock on a target machine, inspect the exact dependency
resolution and license files, pin every source commit and configure flag in
topological `build_order`, document the compiler's exact `--version` line and
provenance, and list each runtime file relative to the SDK prefix. Record all
compiler runtime DLLs if they are redistributed. Resolve implicit dependencies;
do not fill the list from an assumed Linux closure, Homebrew prefix or PATH.
The source runner supports CMake, Meson and configure/make recipes; its target
compilation paths have not yet been run and must be validated before acceptance.

```sh
# Current expected result: native_build_blocked:source_lock_incomplete, exit 1.
scripts/native/build-macos.sh --check-plan
python3 scripts/native/build-target.py --target x86_64-pc-windows-msvc --check-plan
```

On macOS ARM64, after a reviewed complete source lock:

```sh
scripts/native/build-macos.sh
source target/native/aarch64-apple-darwin/activation.sh
npm ci --prefix apps/desktop
npm --prefix apps/desktop test -- --run
npm --prefix apps/desktop run typecheck
npm --prefix apps/desktop run build
cargo test --locked -p media-engine --features native-mpv
cargo test --locked --manifest-path apps/desktop/src-tauri/Cargo.toml --lib --features ipc-test
npm --prefix apps/desktop run tauri -- build --target aarch64-apple-darwin --bundles app --config "$PWD/target/native/aarch64-apple-darwin/bundle-config.json"
```

Windows uses an x64 Developer PowerShell with the reviewed MinGW64 toolchain,
`MSYSTEM=MINGW64`, `gendef`, and `lib.exe` available. The SDK recipe creates the
MSVC `mpv.lib` import library from the built `libmpv-2.dll` export definition.
Run `scripts/native/build-windows.ps1 -CheckPlan`, then the script without that
switch. Source `target/native/x86_64-pc-windows-msvc/activation.ps1`; run the same
UI/Rust checks and `npm --prefix apps/desktop run tauri -- build --target
x86_64-pc-windows-msvc --bundles nsis --config <generated bundle-config.json>`.
Use the generated config's local absolute path only in the local command, never
in the shareable report. These are target recipes, not recorded successful runs.

The SDK writes local activation/config/log files plus a path-free
`sdk-receipt.json`. It copies license texts, upstream source archives, exact mpv
patches and build settings into candidate bundle resources. This does not grant
distribution permission. Review the whole license closure and corresponding
source obligations, including the application linkage, before any distribution.
Build logs can contain local paths; keep them local or redact them before sharing.
No release/upload/signing step is provided.

The intended floor is macOS 11.0 ARM64 and Windows 10 1809 x64 (build 17763);
Windows 11 is a separate required row. The macOS floor is in Tauri config and the
source recipe. The Windows floor is an intended acceptance boundary, not an
observed installer or OS-compatibility result. Exact SDK/compiler/WebView versions
must be recorded from actual target runs. Automatic unverified WebView2 download
is disabled. A Windows candidate requires a verified system WebView2 runtime;
clean-machine delivery remains blocked until a verified runtime/installer supply
and installation procedure are inventoried and tested.

## Fixtures and static bundle verification

Generate fixtures privately with `scripts/generate-player-fixtures.sh` and the
pinned Linux FFmpeg development environment. The script records generator hash,
encoder version and individual media hashes. The six required files are
`h264.mp4`, `h264.mov`, `tracks.mkv`, `hevc.mp4`, `vp9.mkv`, `av1.mkv`; native tests
fail if any is absent. Transfer for target testing only through the user's
chosen route and verify every hash against the generation manifest. No encoded
fixture binaries have been added to this repository. The native workflow's
`fixture_inventory_complete=false` records that reproducible CI generation is
not yet wired; skipped native decoder steps do not count as decoder passes.

After actual target builds, inspect libraries locally with `otool -L`/`otool -l`
on macOS or `dumpbin /imports` on Windows, and populate the reviewed target
inventory. Every bundle file needs its relative path and SHA256; every native
binary needs its component and observed dependency list. Native component
`sources` require source URL/revision, SPDX license, actual configure flags,
recipe, included notice paths and a corresponding-source/offer path. Record
actual OS/SDK/compiler in `build_evidence`; preserve source and binary receipts.
Windows additionally needs verified WebView2 evidence. Only after those facts
are reviewed can the distribution inventory become `inventory_complete`.

```sh
python3 scripts/verify-player-bundle.py --target aarch64-apple-darwin --bundle <Branch.app>
python3 scripts/verify-player-bundle.py --target x86_64-pc-windows-msvc --bundle <extracted-or-installed-payload-directory>
```

The verifier reads the actual Mach-O load commands and PE normal/delay-import
tables; it does not execute bundled code and does not trust a prewritten file
list. It checks architecture, hashes, declared versus actual imports, non-system
closure, loader-relative resolution, notices, unlisted files and escaping
symlinks. Absolute developer library/rpath references and known developer build
path markers are rejected with fixed diagnostic codes. Windows libraries must
be beside the executable; the NSIS installer itself is not a DLL search folder.
Mac install names/rpaths must resolve within the `.app` or to approved system
locations. If bundler relocation is wrong, correct it and rerun this gate; no
successful target relocation is claimed here. Unsupported Mach-O/FAT variants
cannot bypass architecture/import checks by being declared as a notice rather
than a native binary.

An exit0 is **static completeness only** (`runtime_tested:false`). Current target
inventories reject with `inventory_incomplete`; no actual target bundle has
passed. The workflow runs static/Python/pure API/UI checks, reports the blocked
supply gate explicitly, and conditionally contains the source/build/decode/
bundle stages. It does not upload native artifacts. Provisioning target tools,
fixture generation and Windows payload extraction must be completed and reviewed
before the blocked fields are enabled.

## Required target acceptance record

Each cell is currently **BLOCKED — target execution not available to this task**.
Use PASS/FAIL/BLOCKED plus observed evidence, never only a checkbox.

| Required behavior | macOS ARM64 | Windows 10 x64 | Windows 11 x64 |
| --- | --- | --- | --- |
| Native SDK link/build and complete static bundle inventory | BLOCKED | BLOCKED | BLOCKED |
| MP4/MOV/MKV actual video and sound in one embedded window | BLOCKED | BLOCKED | BLOCKED |
| Two audio tracks; subtitles on/off; real track names | BLOCKED | BLOCKED | BLOCKED |
| Pause/play; seek start/middle/end; EOF play restarts | BLOCKED | BLOCKED | BLOCKED |
| Speeds .5/.75/1/1.25/1.5/2 | BLOCKED | BLOCKED | BLOCKED |
| Software H.264/HEVC/VP9/AV1 and separate ProRes observation | BLOCKED | BLOCKED | BLOCKED |
| Observed hardware path on suitable GPU; Windows copy-back labelled | BLOCKED | BLOCKED | BLOCKED |
| Transparent panel click/focus/show-hide above real video | BLOCKED | BLOCKED | BLOCKED |
| Ten fullscreen/window cycles and unchanged aspect ratio | BLOCKED | BLOCKED | BLOCKED |
| Resize, minimize/restore and zero-area handling | BLOCKED | BLOCKED | BLOCKED |
| Fractional/mixed DPI and moving between displays | BLOCKED | BLOCKED | BLOCKED |
| Cancel dialog preserves film; delayed A→B; close during decode/render | BLOCKED | BLOCKED | BLOCKED |
| Empty profile: no config/scripts/sibling or referenced resource access | BLOCKED | BLOCKED | BLOCKED |
| Clean-machine launch/open/tracks/fullscreen/close without SDK tools | BLOCKED | BLOCKED | BLOCKED |
| Verified WebView runtime installation and no missing libraries | n/a | BLOCKED | BLOCKED |

For every actual run record OS version/build, CPU/GPU, application commit,
fixture hashes, native versions/licenses, compiler/SDK/WebView, codec,
`hwdec-current`, renderer and static inventory result. Evidence must omit exact
user paths and secrets. Images/video support composition observations; they do
not replace decoder logs. Clean-machine proof uses machines without mpv/Rust/
Node. Record normal OS handling of an unsigned test application, without
turning off system protection or fabricating signatures.

Only successful mandatory native and clean-machine rows allow B1 PASS. A
reproducible defect gives FAIL and triggers systematic-debugging before a
composition redesign. If Tauri cannot satisfy the gate, document the reason and
prepare the planned Qt alternative for review while preserving MediaEngine/A.
Until that decision, B2 UI expansion remains blocked.
