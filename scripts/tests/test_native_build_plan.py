"""Source build admission is separate from binary distribution admission."""
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
BUILDER = ROOT / 'scripts/native/build-target.py'
SPEC = importlib.util.spec_from_file_location('native_build_target', BUILDER)
BUILDER_MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BUILDER_MODULE)


class SourcePlanTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.manifest = Path(self.temp.name) / 'sources.json'
        self.base = json.loads((ROOT / 'docs/development/native-dependencies.json').read_text())
        self.target = 'aarch64-apple-darwin'
        self.pin = copy.deepcopy(self.base['targets'][self.target])
        self.pin.update(source_lock_complete=True, os_floor='11.0', build_order=['dav1d', 'libass', 'libplacebo', 'lua', 'ffmpeg', 'mpv'], toolchain={'compiler': 'clang', 'compiler_version': 'synthetic', 'provenance': 'synthetic'}, runtime_files={'mpv':['lib/libmpv.2.dylib'], 'ffmpeg':['lib/libavcodec.62.dylib']})
        self.pin['sources'] = {name:{'url':'https://example.invalid/'+name+'.git', 'revision':'b'*40, 'license':'MIT', 'license_files':['LICENSE'], 'build_system':'meson', 'configure_flags':['-Dtests=false'], 'dependencies':[]} for name in self.pin['build_order']}
        self.pin['sources']['mpv'].update(url='https://github.com/mpv-player/mpv.git',revision=self.base['linux_development']['mpv']['revision'], license='GPL-2.0-or-later', configure_flags=self.base['linux_development']['mpv']['configure_flags'], dependencies=['ffmpeg','libass','libplacebo','lua'])
        self.pin['sources']['ffmpeg'].update(url='https://github.com/FFmpeg/FFmpeg.git', revision='140fd653aed8cad774f991ba083e2d01e86420c7', license='GPL-2.0-or-later', build_system='configure', configure_flags=['--enable-shared','--disable-static','--disable-programs','--enable-gpl','--enable-libdav1d'], dependencies=['dav1d'])

    def run_plan(self):
        self.base['targets'][self.target] = self.pin
        self.manifest.write_text(json.dumps(self.base))
        return subprocess.run([sys.executable,str(BUILDER),'--target',self.target,'--check-plan','--manifest',str(self.manifest)],capture_output=True,text=True)

    def reject(self, code):
        result=self.run_plan()
        self.assertNotEqual(result.returncode,0,result.stdout)
        self.assertIn(code,result.stderr)
        self.assertNotIn(str(self.manifest.parent),result.stderr)

    def test_complete_source_plan_is_admitted_without_downloads(self):
        result=self.run_plan()
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertIn('source_plan_verified',result.stdout)

    def test_unresolved_source_lock_blocks_before_any_download(self):
        self.pin['source_lock_complete']=False
        self.reject('source_lock_incomplete')

    def test_wrong_ffmpeg_base_is_rejected(self):
        self.pin['sources']['ffmpeg']['revision']='c'*40
        self.reject('ffmpeg_pin')

    def test_wrong_mpv_patch_hash_is_rejected(self):
        self.pin['mpv_patches'][0]['sha256']='d'*64
        self.reject('mpv_patch')

    def test_missing_transitive_license_blocks_build(self):
        self.pin['sources']['dav1d'].pop('license')
        self.reject('source_license')

    def test_build_order_cannot_hide_unpinned_dependency(self):
        self.pin['sources']['libass']['dependencies']=['notpinned']
        self.reject('dependency_order')


class PristineSourceTests(unittest.TestCase):
    def test_real_git_tree_rejects_all_hidden_or_visible_changes(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            def git(*arguments):
                return subprocess.check_output(['git', '-C', str(source), *arguments], stderr=subprocess.DEVNULL)
            git('init', '-q')
            git('config', 'user.name', 'Synthetic test')
            git('config', 'user.email', 'test@example.invalid')
            (source / '.gitignore').write_text('ignored\n')
            (source / 'tracked').write_text('original\n')
            git('add', '.')
            git('commit', '-qm', 'Synthetic source')
            BUILDER_MODULE.verify_pristine_source(source)
            for state in ['tracked', 'staged', 'untracked', 'ignored', 'assume-unchanged', 'skip-worktree']:
                with self.subTest(state=state):
                    if state in ('tracked', 'staged', 'assume-unchanged', 'skip-worktree'):
                        (source / 'tracked').write_text('changed\n')
                        if state == 'staged':
                            git('add', 'tracked')
                        elif state in ('assume-unchanged', 'skip-worktree'):
                            git('update-index', '--' + state, 'tracked')
                    else:
                        (source / state).write_text('unreviewed\n')
                    with self.assertRaises(BUILDER_MODULE.Blocked) as rejected:
                        BUILDER_MODULE.verify_pristine_source(source)
                    self.assertEqual(str(rejected.exception), 'source_dirty')
                    git('update-index', '--no-assume-unchanged', 'tracked')
                    git('update-index', '--no-skip-worktree', 'tracked')
                    git('reset', '-q', 'HEAD', '--', 'tracked')
                    git('checkout', '--', 'tracked')
                    for name in ('untracked', 'ignored'):
                        (source / name).unlink(missing_ok=True)
            BUILDER_MODULE.verify_pristine_source(source)


if __name__=='__main__':
    unittest.main()
