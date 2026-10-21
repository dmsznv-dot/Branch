#!/usr/bin/env bash
# Linux-only, rootless development SDK. No privileged installation or target
# delivery claims. Signed HTTPS APT and pinned source/hash checks stay enabled.
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
toolroot=${PLAYER_TOOLCHAIN_ROOT:-"$repo/target/player-toolchain"}
sdk="$toolroot/player-linux"
aptstate="$toolroot/player-apt"
source_dir="$toolroot/mpv-source-0.41.0"
build_dir="$toolroot/mpv-build-0.41.0"
prefix="$toolroot/mpv-0.41.0"
mode=${1:---install}
[[ $# -le 1 && ( "$mode" == --install || "$mode" == --verify ) ]] || { echo 'usage: setup-player-linux.sh [--install|--verify]' >&2; exit 2; }
[[ $(uname -s) == Linux && $(uname -m) == x86_64 ]] || { echo 'setup_error_platform' >&2; exit 1; }
mkdir -p "$toolroot" "$repo/target"
if [[ "$mode" == --install ]]; then
    mkdir -p "$aptstate/lists/partial" "$aptstate/archives/partial" "$sdk"
    [[ -f "$aptstate/status" ]] || cp /var/lib/dpkg/status "$aptstate/status"
    cat > "$aptstate/sources.list" <<'SOURCES'
deb [signed-by=/usr/share/keyrings/debian-archive-keyring.gpg] https://deb.debian.org/debian trixie main
deb [signed-by=/usr/share/keyrings/debian-archive-keyring.gpg] https://deb.debian.org/debian-security trixie-security main
SOURCES
    apt_options=(-o "Dir::Etc::sourcelist=$aptstate/sources.list" -o Dir::Etc::sourceparts=- -o "Dir::State=$aptstate" -o "Dir::State::status=$aptstate/status" -o "Dir::State::lists=$aptstate/lists" -o "Dir::Cache::archives=$aptstate/archives")
    apt-get "${apt_options[@]}" update
    package_lines=$(python3 - "$repo/docs/development/native-dependencies.json" <<'PY'
import json,sys,urllib.parse
wanted={'libmpv-dev','libwebkit2gtk-4.1-dev','libgtk-3-dev','librsvg2-dev','libssl-dev','cmake','ninja-build','meson','ffmpeg','pkgconf','pkgconf-bin','libpkgconf3'}
for p in json.load(open(sys.argv[1]))['linux_development']['package_archives']:
 name,version,_=p['name'].split('_')
 if name in wanted: print(name+'='+urllib.parse.unquote(version))
PY
)
    mapfile -t packages <<< "$package_lines"
    [[ ${#packages[@]} == 12 ]] || { echo 'setup_error_package_pins' >&2; exit 1; }
    apt-get "${apt_options[@]}" --download-only --reinstall --no-install-recommends -y install "${packages[@]}"
fi
# Verify every archive before extraction. New/unrecorded dependency resolutions
# fail explicitly, rather than silently changing the SDK. Current APT metadata
# must still be signed; stale unavailable pins are a real setup failure.
python3 - "$repo/docs/development/native-dependencies.json" "$aptstate/archives" <<'PY'
import hashlib,json,pathlib,sys
pins={p['name']:p['sha256'] for p in json.load(open(sys.argv[1]))['linux_development']['package_archives']}
files=list(pathlib.Path(sys.argv[2]).glob('*.deb'))
assert files, 'setup_error_no_archives'
for p in files:
 assert p.name in pins and hashlib.sha256(p.read_bytes()).hexdigest()==pins[p.name], 'setup_error_archive_hash'
print('package_hashes_verified')
PY
if [[ "$mode" == --install ]]; then
    for archive in "$aptstate/archives/"*.deb; do dpkg-deb -x "$archive" "$sdk"; done
    # Installed runtime packages are absent from APT's download closure. Complete
    # missing developer SONAME targets using existing same-SONAME system files.
    # These links are DEVELOPMENT ONLY, never packaged into target applications.
    python3 - "$sdk" "$repo/target/native-linux-links.json" <<'PY'
import hashlib,json,os,pathlib,sys
sdk=pathlib.Path(sys.argv[1]); evidence=[]
for link in (sdk/'usr/lib/x86_64-linux-gnu').glob('*.so'):
 if link.is_symlink() and not link.exists():
  target=link.parent/os.readlink(link)
  system=pathlib.Path('/usr/lib/x86_64-linux-gnu')/target.name
  if system.exists() and not target.exists():
   target.symlink_to(system.resolve())
   evidence.append({'soname':target.name,'sha256':hashlib.sha256(system.read_bytes()).hexdigest()})
pathlib.Path(sys.argv[2]).write_text(json.dumps(evidence,indent=2)+'\n')
PY
fi
# Stable activation file has no dependency on session /workspace/shared files.
python3 - "$toolroot" "$repo/target/player-linux-activate.sh" <<'PY'
import pathlib,shlex,sys
root=pathlib.Path(sys.argv[1]).resolve(); sdk=root/'player-linux'; mpv=root/'mpv-0.41.0'
values={'PATH':f'{sdk}/usr/bin', 'LD_LIBRARY_PATH':f'{mpv}/lib/x86_64-linux-gnu:{sdk}/usr/lib/x86_64-linux-gnu:{sdk}/usr/lib','LIBRARY_PATH':f'{mpv}/lib/x86_64-linux-gnu:{sdk}/usr/lib/x86_64-linux-gnu:{sdk}/usr/lib','PKG_CONFIG_PATH':f'{sdk}/usr/lib/x86_64-linux-gnu/pkgconfig:{sdk}/usr/lib/pkgconfig:{sdk}/usr/share/pkgconfig','PYTHONPATH':f'{sdk}/usr/lib/python3/dist-packages'}
lines=['# Generated rootless Linux development activation. Rust is activated separately.']
for key,value in values.items(): lines.append(f'export {key}={shlex.quote(value)}:"${{{key}:-}}"')
lines.append('export PKG_CONFIG_SYSROOT_DIR='+shlex.quote(str(sdk)))
pathlib.Path(sys.argv[2]).write_text('\n'.join(lines)+'\n')
PY
source "$repo/target/player-linux-activate.sh"
# Tauri is a separate workspace, but Linux still needs its real development
# headers/tools for API/build checks. Nothing here installs macOS/Windows SDKs.
for binary in cmake pkg-config; do
    command -v "$binary" >/dev/null || { echo "setup_error_tool_$binary" >&2; exit 1; }
done
pkg-config --exists gtk+-3.0 webkit2gtk-4.1 librsvg-2.0 openssl || { echo 'setup_error_tauri_development_modules' >&2; exit 1; }
python3 - "$aptstate/archives" "$repo/target/desktop-linux-readiness.json" <<'PYTOOLS'
import json,pathlib,subprocess,sys
archives=pathlib.Path(sys.argv[1])
mpv=list(archives.glob('libmpv-dev_*.deb'))
assert len(mpv)==1, 'setup_error_distro_mpv_archive'
distro=subprocess.check_output(['dpkg-deb','-f',str(mpv[0]),'Version'],text=True).strip()
modules={key:subprocess.check_output(['pkg-config','--modversion',key],text=True).strip() for key in ['gtk+-3.0','webkit2gtk-4.1','librsvg-2.0','openssl']}
pathlib.Path(sys.argv[2]).write_text(json.dumps({'scope':'Linux development checks only; native surface unavailable','cmake':subprocess.check_output(['cmake','--version'],text=True).splitlines()[0],'pkg_config':subprocess.check_output(['pkg-config','--version'],text=True).strip(),'modules':modules,'distro_libmpv_dev':distro,'player_mpv':'Pinned patched upstream 0.41.0 below; distro libmpv does not satisfy API2.5'},indent=2)+'\n')
print('tauri_development_modules_verified')
PYTOOLS
if [[ "$mode" == --install ]]; then
    if [[ ! -d "$source_dir/.git" ]]; then git clone --branch v0.41.0 --depth 1 https://github.com/mpv-player/mpv.git "$source_dir"; fi
    python3 "$repo/scripts/native/verify-mpv-source.py" "$source_dir" "$repo/docs/development/native-dependencies.json" "$repo" --apply
    flags=(--prefix="$prefix" -Dlibmpv=true -Dcplayer=false -Dtests=false -Dlua=lua5.2 -Djavascript=disabled -Dlibarchive=disabled -Dbuild-date=false -Dauto_features=disabled -Dgl=enabled -Dplain-gl=enabled)
    if [[ -f "$build_dir/meson-private/coredata.dat" ]]; then
        meson setup --reconfigure --clearcache "$build_dir" "$source_dir" "${flags[@]}"
    else
        meson setup "$build_dir" "$source_dir" "${flags[@]}"
    fi
    meson compile -C "$build_dir" -j3
    meson install -C "$build_dir"
fi
python3 "$repo/scripts/native/verify-mpv-source.py" "$source_dir" "$repo/docs/development/native-dependencies.json" "$repo"
python3 - "$repo/target/native-linux-readiness.json" "$repo/docs/development/native-dependencies.json" "$prefix" <<'PY'
import ctypes,hashlib,json,pathlib,shutil,subprocess,sys
for item in json.load(open(sys.argv[2]))['linux_development']['installed_tool_hashes']:
 assert hashlib.sha256(pathlib.Path(shutil.which(item['name'])).read_bytes()).hexdigest()==item['sha256'], 'setup_error_tool_hash'
pin=json.load(open(sys.argv[2]))['linux_development']['mpv']
artifact=pathlib.Path(sys.argv[3])/'lib/x86_64-linux-gnu/libmpv.so.2.5.0'
m=ctypes.CDLL(str(artifact)); m.mpv_client_api_version.restype=ctypes.c_ulong
m.mpv_create.restype=ctypes.c_void_p
m.mpv_set_option_string.argtypes=[ctypes.c_void_p,ctypes.c_char_p,ctypes.c_char_p]
m.mpv_initialize.argtypes=[ctypes.c_void_p]; m.mpv_terminate_destroy.argtypes=[ctypes.c_void_p]
m.mpv_get_property_string.argtypes=[ctypes.c_void_p,ctypes.c_char_p]; m.mpv_get_property_string.restype=ctypes.c_void_p
m.mpv_free.argtypes=[ctypes.c_void_p]
assert m.mpv_client_api_version()==0x20005, 'setup_error_client_api'
h=m.mpv_create(); assert h, 'setup_error_create'
options=[('config','no'),('access-references','no'),('autoload-files','no'),('load-scripts','no'),('ytdl','no'),('load-stats-overlay','no'),('load-console','no'),('load-auto-profiles','no'),('load-select','no'),('load-positioning','no'),('load-commands','no'),('input-default-bindings','no'),('input-vo-keyboard','no'),('input-terminal','no'),('osc','no'),('osd-level','0'),('terminal','no'),('msg-level','all=no'),('input-ipc-server',''),('cache','no'),('cache-on-disk','no'),('sub-auto','no'),('audio-file-auto','no'),('demuxer','lavf'),('demuxer-lavf-o','format_whitelist=%12%mov,matroska'),('idle','yes'),('keep-open','yes'),('vo','null'),('ao','null'),('hwdec','no')]
results={}
try:
 for key,value in options:
  results[key]=m.mpv_set_option_string(h,key.encode(),value.encode())
  assert results[key]==0, 'setup_error_option_'+key
 assert m.mpv_initialize(h)==0, 'setup_error_initialize'
 versions={}
 for key in ['mpv-version','ffmpeg-version']:
  p=m.mpv_get_property_string(h,key.encode()); assert p
  versions[key]=ctypes.string_at(p).decode(); m.mpv_free(p)
 assert versions['mpv-version']=='mpv v0.41.0-dirty', 'setup_error_mpv_version'
 assert versions['ffmpeg-version']=='7.1.5-0+deb13u1', 'setup_error_ffmpeg_version'
 assert subprocess.check_output(['ffmpeg','-version'],text=True).splitlines()[0].startswith('ffmpeg version 7.1.5-0+deb13u1 ')
 pathlib.Path(sys.argv[1]).write_text(json.dumps({'scope':'Linux development only','api':'2.5','versions':versions,'options':results,'mpv_source':{'revision':pin['revision'],'patches':pin['patches']},'mpv_artifact':{'file':artifact.name,'sha256':hashlib.sha256(artifact.read_bytes()).hexdigest()}},indent=2)+'\n')
finally:
 m.mpv_terminate_destroy(h)
print('native_api_policy_verified')
PY
printf 'Activation: source %s/target/player-linux-activate.sh\n' "$repo"
