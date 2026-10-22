# Branch for macOS ARM64

This is an experimental Apple silicon build of Branch 0.1.0. It requires macOS
11.0 or newer and an ARM64 Mac. It has not been run through device acceptance.

## Install

1. Download `Branch-macos-arm64.zip` from the GitHub Release and expand it.
2. Optionally verify the archive against `Branch-macos-arm64.zip.sha256`:

   ```sh
   shasum -a 256 -c Branch-macos-arm64.zip.sha256
   ```

3. Move `Branch.app` into `/Applications` (or another folder you control).
4. Open the app. This build uses an ad hoc signature; it is not signed with a
   Developer ID certificate and has not been notarized by Apple. macOS may
   block the first launch. If you trust this build and choose to continue, try
   opening it once, then use **System Settings → Privacy & Security → Open
   Anyway** for this app. Do not lower the Mac's global security settings.

This release includes the native runtime libraries and source/license material
used for the build. Distribution and license obligations have not received a
separate legal review. Review the release notes before using or redistributing
it.
