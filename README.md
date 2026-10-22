# Branch

Branch is a desktop video player with watch rooms. One person hosts a room from
the app; everyone opens their own local copy of the same video. Branch checks
the complete files with SHA-256 and synchronizes playback commands and room
state. Video files are not uploaded or sent to other participants.

## Download

The current experimental macOS Apple silicon build is published on the
[GitHub Releases page](https://github.com/dmsznv-dot/Branch/releases). See the
[macOS installation guide](docs/development/INSTALL-macOS-ARM64.md) for setup
and first launch. This build is for ARM64 Macs and requires macOS 11 or newer.
An Intel Mac build is not provided.

## Create a watch room

1. Open the video file in Branch and choose **Create server** in the watch
   panel.
2. Share the invitation link with guests.
3. Each guest opens the exact same video file in Branch and joins with the
   invitation.

The host's Branch app runs the room server on that Mac. It attempts to open TCP
port 8787 on the router using UPnP and discover the public IPv4 address. No
separate server program or account is required. Direct hosting depends on the
router allowing UPnP and the internet connection having a reachable public
address; CGNAT or a router with UPnP disabled can prevent hosting. macOS may ask
whether to allow incoming connections.

## Project status

This is an experimental preview. The macOS app was built locally for Apple
silicon, but the release is ad-hoc signed and is not notarized by Apple.
Network hosting across different internet connections and device acceptance
have not been verified. The dependency/license inventory is still incomplete;
review the release notes and native dependency record before redistribution.

For development setup and architecture notes, see
[docs/development](docs/development/).
