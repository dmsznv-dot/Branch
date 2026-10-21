#!/usr/bin/env python3
"""Admit only the pinned mpv base plus exact hashed downstream patches.

Build out of tree: no untracked files, including ignored files, are admitted in
source. Meson output belongs in the separate build directory, never this tree.
"""
import hashlib
import json
import os
import pathlib
import subprocess
import sys
import tempfile


def verify(source, manifest, repo, apply=False):
    pin = json.loads(manifest.read_text())['linux_development']['mpv']
    source_env = dict(os.environ, GIT_NO_REPLACE_OBJECTS='1')

    def git(*args, env=None):
        env = dict(source_env if env is None else env, GIT_NO_REPLACE_OBJECTS='1')
        return subprocess.check_output(
            ['git', '-C', str(source), *args], env=env, stderr=subprocess.PIPE)

    def require(condition, reason):
        if not condition:
            raise ValueError('setup_error_source_' + reason)

    require(git('rev-parse', 'HEAD').decode().strip() == pin['revision'], 'revision')
    require(not git('ls-files', '--others', '-z'), 'untracked')
    # Git otherwise hides assume-unchanged/skip-worktree paths from diff.
    require(all(entry.startswith(b'H ') for entry in
                git('ls-files', '-v', '-z').split(b'\0') if entry), 'index_flags')
    require(not git('diff', '--cached', '--binary', '--no-ext-diff', '--no-textconv'), 'index')
    patches = []
    for item in pin['patches']:
        patch = (repo / item['file']).read_bytes()
        require(hashlib.sha256(patch).hexdigest() == item['sha256'], 'patch_hash')
        patches.append(patch)
    require(bool(patches), 'patch_missing')
    # A disposable index computes the exact expected full-tree diff without
    # modifying the source's index, committing, or trusting a reverse-apply test.
    with tempfile.TemporaryDirectory(prefix='mpv-index-') as temp:
        env = dict(source_env, GIT_INDEX_FILE=str(pathlib.Path(temp) / 'index'))
        git('read-tree', pin['revision'], env=env)
        prefixes = [b'']
        for patch in patches:
            subprocess.run(['git', '-C', str(source), 'apply', '--cached', '-'],
                           input=patch, env=env, check=True, capture_output=True)
            prefixes.append(git('diff', '--cached', '--binary', '--no-ext-diff',
                                '--no-textconv', pin['revision'], env=env))
        expected = prefixes[-1]
    actual = git('diff', '--binary', '--no-ext-diff', '--no-textconv', 'HEAD')
    if apply and actual != expected:
        require(actual in prefixes, 'diff')
        for patch in patches[prefixes.index(actual):]:
            subprocess.run(['git', '-C', str(source), 'apply', '-'],
                           input=patch, env=source_env, check=True, capture_output=True)
        actual = git('diff', '--binary', '--no-ext-diff', '--no-textconv', 'HEAD')
    require(actual == expected, 'diff')
    print('mpv_source_patch_verified')


if __name__ == '__main__':
    try:
        if len(sys.argv) not in (4, 5) or (len(sys.argv) == 5 and sys.argv[4] != '--apply'):
            raise ValueError('usage: verify-mpv-source.py SOURCE MANIFEST REPO [--apply]')
        verify(*(pathlib.Path(p).resolve() for p in sys.argv[1:4]), apply=len(sys.argv) == 5)
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError) as error:
        # Fixed diagnostics exclude Git stderr and arbitrary source content.
        print(str(error) if isinstance(error, ValueError) else 'setup_error_source_verification', file=sys.stderr)
        sys.exit(1)
