"""Exercise source admission with real Git trees and deliberate tampering."""
import hashlib
import json
import pathlib
import subprocess
import tempfile
import unittest

HELPER = pathlib.Path(__file__).resolve().parents[1] / 'native/verify-mpv-source.py'


class SourceIntegrity(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = pathlib.Path(self.tmp.name)
        self.source = self.root / 'source'
        self.source.mkdir()
        self.git('init', '-q')
        self.git('config', 'user.name', 'Fixture')
        self.git('config', 'user.email', 'fixture@example.invalid')
        self.file = self.source / 'video.c'
        self.file.write_text('allocate_uninitialized();\n')
        (self.source / 'other.c').write_text('original();\n')
        self.git('add', '.')
        self.git('commit', '-qm', 'immutable base')
        revision = self.git('rev-parse', 'HEAD').strip()
        self.file.write_text('allocate_zeroed();\n')
        self.patch = self.root / 'fix.patch'
        self.patch.write_text(self.git('diff', '--binary'))
        self.file.write_text('allocate_uninitialized();\n')
        self.manifest = self.root / 'manifest.json'
        self.manifest.write_text(json.dumps({'linux_development': {'mpv': {
            'revision': revision, 'patches': [{'file': 'fix.patch',
            'sha256': hashlib.sha256(self.patch.read_bytes()).hexdigest()}]}}}))

    def git(self, *args):
        return subprocess.check_output(['git', '-C', str(self.source), *args], text=True)

    def check(self, apply=False, success=True):
        cmd = ['python3', str(HELPER), str(self.source), str(self.manifest), str(self.root)]
        if apply:
            cmd.append('--apply')
        result = subprocess.run(cmd, capture_output=True, text=True)
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
        return result

    def test_only_exact_patch_and_idempotent_apply(self):
        self.check(success=False)
        self.check(apply=True)
        self.check(apply=True)
        self.check()
        self.assertEqual(self.file.read_text(), 'allocate_zeroed();\n')

    def test_reject_unrelated_tracked_or_staged_change(self):
        self.check(apply=True)
        self.file.write_text('unexpected();\n')
        self.check(success=False)
        self.git('add', 'video.c')
        self.check(apply=True, success=False)

    def test_reject_untracked_even_ignored(self):
        self.check(apply=True)
        (self.source / 'extra.c').write_text('injected')
        self.check(success=False)
        self.git('config', 'core.excludesFile', str(self.root / 'ignore'))
        (self.root / 'ignore').write_text('extra.c\n')
        self.check(apply=True, success=False)

    def test_reject_hidden_tracked_modification(self):
        self.check(apply=True)
        self.git('update-index', '--assume-unchanged', 'other.c')
        (self.source / 'other.c').write_text('hidden_change();\n')
        self.check(success=False)
        self.git('update-index', '--no-assume-unchanged', 'other.c')
        self.git('update-index', '--skip-worktree', 'other.c')
        self.check(success=False)

    def test_complete_exact_prior_patch_prefix_only(self):
        other = self.source / 'other.c'
        other.write_text('corrected();\n')
        second = self.root / 'second.patch'
        second.write_text(self.git('diff', '--binary', '--', 'other.c'))
        other.write_text('original();\n')
        data = json.loads(self.manifest.read_text())
        data['linux_development']['mpv']['patches'].append({
            'file': 'second.patch',
            'sha256': hashlib.sha256(second.read_bytes()).hexdigest()})
        self.manifest.write_text(json.dumps(data))
        self.git('apply', str(second))
        self.check(apply=True, success=False)  # out-of-order subset is rejected
        other.write_text('original();\n')
        self.git('apply', str(self.patch))
        self.check(success=False)  # verification requires the complete patch set
        self.check(apply=True)  # install may complete an exact prior prefix
        self.check()
        self.assertEqual(other.read_text(), 'corrected();\n')

    def test_reject_wrong_patch_hash_and_revision(self):
        self.patch.write_text(self.patch.read_text() + '\n')
        self.check(apply=True, success=False)
        data = json.loads(self.manifest.read_text())
        data['linux_development']['mpv']['patches'][0]['sha256'] = hashlib.sha256(self.patch.read_bytes()).hexdigest()
        data['linux_development']['mpv']['revision'] = '0' * 40
        self.manifest.write_text(json.dumps(data))
        self.check(apply=True, success=False)


if __name__ == '__main__':
    unittest.main()
