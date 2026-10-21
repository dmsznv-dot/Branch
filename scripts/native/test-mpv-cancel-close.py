#!/usr/bin/env python3
"""Build a bounded order test from genuine SDK objects, with one observer.

Run after SDK activation. This never edits the source/build objects or installed
libmpv. Only a copy of loadfile.c's open/playback cancellation calls receives
a forwarding hook.
The real cancellation algorithm and stream close implementation are unmodified.
"""
import json
import os
import pathlib
import shlex
import subprocess

repo = pathlib.Path(__file__).resolve().parents[2]
root = pathlib.Path(os.environ.get('PLAYER_TOOLCHAIN_ROOT', repo / 'target/player-toolchain')).resolve()
source = root / 'mpv-source-0.41.0'
build = root / 'mpv-build-0.41.0'
output = repo / 'target/player-cancel-order-test'
output.mkdir(parents=True, exist_ok=True)
subprocess.run(['python3', str(repo / 'scripts/native/verify-mpv-source.py'),
                str(source), str(repo / 'docs/development/native-dependencies.json'), str(repo)], check=True)
text = (source / 'player/loadfile.c').read_text()
calls = ['mp_cancel_trigger(mpctx->open_cancel);', 'mp_cancel_trigger(mpctx->playback_abort);']
for call in calls:
    assert text.count(call) == 1
    text = text.replace(call, call.replace('mp_cancel_trigger', 'branch_test_trigger'))
copy = output / 'loadfile-observer.c'
copy.write_text('struct mp_cancel; void branch_test_trigger(struct mp_cancel *);\n' +
                text)
entries = json.loads(subprocess.check_output(['ninja', '-C', str(build), '-t', 'compdb', 'c_COMPILER'], text=True))
entry, = [item for item in entries if item['file'].endswith('/player/loadfile.c')]
original = shlex.split(entry['command'])
args = []
i = 0
while i < len(original):
    arg = original[i]
    if arg in ('-o', '-MF', '-MQ'):
        i += 2
        continue
    if arg != '-MD' and arg != entry['file']:
        args.append(arg)
    i += 1
observer = output / 'loadfile-observer.o'
subprocess.run(args + ['-I' + str(source / 'player'), '-o', str(observer), str(copy)], cwd=build, check=True, timeout=30)
harness = output / 'test.o'
subprocess.run(['cc', '-std=c11', '-D_POSIX_C_SOURCE=200809L', '-Wall', '-Wextra', '-Werror', '-pthread',
                '-I' + str(source / 'include'), '-c', str(repo / 'scripts/native/test-mpv-cancel-close.c'),
                '-o', str(harness)], check=True, timeout=30)
link = shlex.split(subprocess.check_output(['ninja', '-C', str(build), '-t', 'commands', 'libmpv.so.2.5.0'], text=True).splitlines()[-1])
# Keep the exact configured object/dependency closure, changing only executable
# output, the observed loadfile object and flags specific to a shared library.
exe = output / 'test-mpv-cancel-close'
link[link.index('-o') + 1] = str(exe)
link = [str(observer) if arg == entry['output'] else arg for arg in link
        if arg not in ('-shared', '-Wl,-soname,libmpv.so.2', '-Wl,-Bsymbolic')]
subprocess.run(link + [str(harness)], cwd=build, check=True, timeout=30)
subprocess.run([str(exe)], check=True, timeout=12)
