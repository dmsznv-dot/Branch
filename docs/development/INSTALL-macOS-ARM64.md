# Branch for macOS ARM64

Branch 0.1.0 is an experimental build for Apple silicon Macs running macOS
11.0 or newer. It has not been through device acceptance.

## Install

1. Open the [GitHub Releases page](https://github.com/dmsznv-dot/Branch/releases)
   and download `Branch-macos-arm64.zip` from the latest macOS ARM64 preview.
2. Expand the ZIP. To verify the download, compare its SHA-256 with the value
   published in the release notes or the accompanying `.sha256` file:

   ```sh
   shasum -a 256 -c Branch-macos-arm64.zip.sha256
   ```

3. Move `Branch.app` into `/Applications` or another folder you control.
4. Open the app. This build has an ad-hoc signature; it is not signed with a
   Developer ID certificate and has not been notarized by Apple. macOS may
   block the first launch. If you trust this build, try opening it once, then
   use **System Settings → Privacy & Security → Open Anyway** for Branch. Do
   not lower the Mac's global security settings.

## Watch rooms

The host creates a room in Branch and shares the invitation. Each participant
must already have the same video file and open it locally. Branch checks the
complete file with SHA-256; the video is not transferred to the host or guests.
The host's app listens on TCP port 8787 and tries to configure the router using
UPnP. Direct hosting requires UPnP and a reachable public IPv4 address; CGNAT
or router restrictions can prevent connections from outside the local network.
Cross-network hosting has not been verified.

## Preview limitations

The app bundle contains its native runtime libraries. The native dependency
inventory is still incomplete, and distribution/license obligations have not
received separate legal review. Review the release notes and
`docs/development/native-dependencies.json` before redistributing this build.
