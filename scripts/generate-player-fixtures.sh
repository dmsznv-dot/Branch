#!/usr/bin/env bash
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
out="$repo/target/player-fixtures"
mkdir -p "$out"
version=$(ffmpeg -version | head -n1)
[[ "$version" == 'ffmpeg version 7.1.5-0+deb13u1 '* ]] || { echo 'fixture_error_ffmpeg_version' >&2; exit 1; }
# Synthetic color bars, sine waves and caption text authored here: CC0-1.0.
printf '1\n00:00:00,000 --> 00:00:02,000\nSynthetic caption\n' > "$out/captions.srt"
video=(-f lavfi -i testsrc2=size=160x90:rate=24:duration=3)
common=(-hide_banner -loglevel error -y -threads 3 -filter_threads 1)
ffmpeg "${common[@]}" "${video[@]}" -c:v libx264 -preset ultrafast -pix_fmt yuv420p "$out/h264.mp4"
ffmpeg "${common[@]}" "${video[@]}" -c:v libx264 -preset ultrafast -pix_fmt yuv420p "$out/h264.mov"
ffmpeg "${common[@]}" "${video[@]}" -f lavfi -i sine=frequency=440:duration=3 -f lavfi -i sine=frequency=880:duration=3 -i "$out/captions.srt" -map 0:v -map 1:a -map 2:a -map 3:s -c:v libx264 -preset ultrafast -c:a aac -c:s srt "$out/tracks.mkv"
ffmpeg "${common[@]}" "${video[@]}" -c:v libx265 -x265-params pools=2:frame-threads=2:log-level=error -tag:v hvc1 "$out/hevc.mp4"
ffmpeg "${common[@]}" "${video[@]}" -c:v libvpx-vp9 -deadline realtime -cpu-used 8 "$out/vp9.mkv"
ffmpeg "${common[@]}" "${video[@]}" -c:v libaom-av1 -cpu-used 8 -row-mt 1 "$out/av1.mkv"
python3 - "$out" "$repo/scripts/generate-player-fixtures.sh" <<'PY'
import hashlib,json,pathlib,subprocess,sys
p=pathlib.Path(sys.argv[1]); files=[]
for f in sorted(p.glob('*')):
 if f.suffix not in ('.mp4','.mov','.mkv','.srt'): continue
 files.append({'file':f.name,'sha256':hashlib.sha256(f.read_bytes()).hexdigest(),'license':'CC0-1.0'})
b=(p/'h264.mp4').read_bytes(); assert b.find(b'moov')>b.find(b'mdat'), 'moov must be at end'
(p/'manifest.json').write_text(json.dumps({'ffmpeg':subprocess.check_output(['ffmpeg','-version'],text=True).splitlines()[0],'license':'CC0-1.0','generator':'scripts/generate-player-fixtures.sh','generator_sha256':hashlib.sha256(pathlib.Path(sys.argv[2]).read_bytes()).hexdigest(),'files':files},indent=2)+'\n')
PY
printf 'fixtures_generated\n'
