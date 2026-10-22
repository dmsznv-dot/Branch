"""Source build admission is separate from binary distribution admission."""
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest import mock

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
        self.pin.update(source_lock_complete=True, os_floor='11.0', build_order=['dav1d', 'libass', 'libplacebo', 'lua', 'ffmpeg', 'mpv'], toolchain={'compiler': 'clang', 'compiler_version': 'synthetic', 'compiler_sha256': 'a'*64, 'provenance': 'synthetic', 'cxx_compiler': 'clang++', 'cxx_compiler_version': 'synthetic C++', 'cxx_compiler_sha256': 'b'*64, 'cxx_provenance': 'synthetic C++'}, runtime_files={'mpv':['lib/libmpv.2.dylib'], 'ffmpeg':['lib/libavcodec.62.dylib']})
        self.pin['sources'] = {name:{'url':'https://example.invalid/'+name+'.git', 'revision':'b'*40, 'license':'MIT', 'license_files':['LICENSE'], 'build_system':'meson', 'configure_flags':['-Dtests=false'], 'dependencies':[]} for name in self.pin['build_order']}
        self.pin['sources']['mpv'].update(url='https://github.com/mpv-player/mpv.git',revision=self.base['linux_development']['mpv']['revision'], license='GPL-2.0-or-later', configure_flags=self.base['linux_development']['mpv']['configure_flags'], dependencies=['ffmpeg','libass','libplacebo','lua'])
        self.pin['sources']['ffmpeg'].update(url='https://github.com/FFmpeg/FFmpeg.git', revision='140fd653aed8cad774f991ba083e2d01e86420c7', license='GPL-2.0-or-later', build_system='configure', configure_flags=['--enable-shared','--disable-static','--disable-programs','--enable-gpl','--enable-libdav1d'], dependencies=['dav1d'])
        self.pin['sources']['lua'].update(build_system='lua-make', configure_flags=['-fPIC','-DLUA_USE_MACOSX'])

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

    def test_ffmpeg_plan_does_not_use_removed_postproc_option(self):
        self.assertNotIn('--disable-postproc',self.pin['sources']['ffmpeg']['configure_flags'])
        result=self.run_plan()
        self.assertEqual(result.returncode,0,result.stderr)
        self.pin['sources']['ffmpeg']['configure_flags'].append('--disable-postproc')
        self.reject('ffmpeg_flags')

    def test_wrong_mpv_patch_hash_is_rejected(self):
        self.pin['mpv_patches'][0]['sha256']='d'*64
        self.reject('mpv_patch')

    def test_missing_transitive_license_blocks_build(self):
        self.pin['sources']['dav1d'].pop('license')
        self.reject('source_license')

    def test_build_order_cannot_hide_unpinned_dependency(self):
        self.pin['sources']['libass']['dependencies']=['notpinned']
        self.reject('dependency_order')

    def test_missing_cxx_provenance_blocks_build(self):
        self.pin['toolchain'].pop('cxx_provenance')
        self.reject('toolchain_provenance')

    def test_lua_uses_the_dedicated_macos_make_recipe(self):
        result=self.run_plan()
        self.assertEqual(result.returncode,0,result.stderr)
        self.pin['sources']['lua']['build_system']='make'
        self.reject('source_recipe')

    def test_plan_errors_are_reported_before_the_readiness_flag(self):
        self.pin['source_lock_complete']=False
        self.pin['sources']['lua']['build_system']='make'
        self.reject('source_recipe')

    def test_submodules_require_pins_licenses_and_dependency_order(self):
        placebo=self.pin['sources']['libplacebo']
        placebo['submodules']=[
            {'path':'thirdparty/markup','url':'https://example.invalid/markup.git','revision':'c'*40,'license':'BSD-3-Clause','license_files':['LICENSE'],'build_system':'python-source-module','configure_flags':[],'dependencies':[]},
            {'path':'thirdparty/template','url':'https://example.invalid/template.git','revision':'d'*40,'license':'BSD-3-Clause','license_files':['LICENSE'],'build_system':'python-source-module','configure_flags':[],'dependencies':['thirdparty/markup']},
            {'path':'thirdparty/glad','url':'https://example.invalid/glad.git','revision':'e'*40,'license':'MIT','license_files':['LICENSE'],'build_system':'python-build-generator','configure_flags':[],'dependencies':['thirdparty/template']},
        ]
        placebo['submodule_order']=['thirdparty/markup','thirdparty/template','thirdparty/glad']
        result=self.run_plan()
        self.assertEqual(result.returncode,0,result.stderr)
        placebo['submodules'][2]['license_files']=[]
        self.reject('submodule_pin')
        placebo['submodules'][2]['license_files']=['LICENSE']
        placebo['submodule_order']=['thirdparty/glad','thirdparty/template','thirdparty/markup']
        self.reject('submodule_order')

    def test_generated_submodule_inputs_require_safe_paths_and_hashes(self):
        glad={'path':'thirdparty/glad','url':'https://example.invalid/glad.git','revision':'e'*40,
              'license':'MIT','license_files':['LICENSE'],'build_system':'python-build-generator',
              'configure_flags':[],'dependencies':[],
              'generated_inputs':[{'source':'glad/files/gl.xml','build_file':'gl.xml','sha256':'f'*64}]}
        placebo=self.pin['sources']['libplacebo']
        placebo['submodules']=[glad]
        placebo['submodule_order']=['thirdparty/glad']
        result=self.run_plan()
        self.assertEqual(result.returncode,0,result.stderr)
        glad['generated_inputs'][0]['build_file']='../gl.xml'
        self.reject('plan_path')

    def test_recipe_cannot_override_selected_compilers(self):
        for flag in ['-DCMAKE_C_COMPILER=unreviewed', 'CMAKE_TOOLCHAIN_FILE=unreviewed', '--native-file=unreviewed', '--cc=unreviewed']:
            with self.subTest(flag=flag):
                self.pin['sources']['libass']['configure_flags']=['-Dtests=false', flag]
                self.reject('compiler_override')


class GeneratorInputTests(unittest.TestCase):
    def test_generator_inputs_are_hash_checked_and_staged(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary)
            source=root/'source'
            build=root/'build'
            generator=source/'3rdparty/glad/glad/files'
            generator.mkdir(parents=True)
            build.mkdir()
            original=b'<registry pinned />\n'
            (generator/'gl.xml').write_bytes(original)
            item={'path':'3rdparty/glad','generated_inputs':[{'source':'glad/files/gl.xml','build_file':'gl.xml','sha256':hashlib.sha256(original).hexdigest()}]}
            BUILDER_MODULE.stage_generator_inputs(source,build,[item])
            self.assertEqual((build/'gl.xml').read_bytes(),original)
            (build/'gl.xml').unlink()
            (generator/'gl.xml').write_bytes(b'<registry changed />\n')
            with self.assertRaises(BUILDER_MODULE.Blocked) as blocked:
                BUILDER_MODULE.stage_generator_inputs(source,build,[item])
            self.assertEqual(str(blocked.exception),'submodule_generated_input')


class MacOSRelocationTests(unittest.TestCase):
    def test_sdk_install_names_and_internal_loads_use_rpath(self):
        with tempfile.TemporaryDirectory() as temporary:
            prefix=Path(temporary)/'sdk'
            (prefix/'lib').mkdir(parents=True)
            first=prefix/'lib/libfirst.dylib'
            second=prefix/'lib/libsecond.dylib'
            first.touch(); second.touch()
            outputs=iter([
                f'{first}:\n  {second} (compatibility version 1.0.0, current version 1.0.0)\n',
                f'{second}:\n',
                f'{first}:\n  @rpath/libsecond.dylib (compatibility version 1.0.0, current version 1.0.0)\n',
                f'{second}:\n',
            ])
            with mock.patch.object(BUILDER_MODULE.shutil,'which',return_value='/usr/bin/tool'), \
                 mock.patch.object(BUILDER_MODULE.subprocess,'check_output',side_effect=lambda *args,**kwargs: next(outputs)), \
                 mock.patch.object(BUILDER_MODULE,'run') as run:
                BUILDER_MODULE.relocate_macos_libraries(prefix,{'first':['lib/libfirst.dylib'],'second':['lib/libsecond.dylib']},{},Path(temporary)/'relocate.log')
            commands=[call.args[0] for call in run.call_args_list]
            self.assertIn(['install_name_tool','-id','@rpath/libfirst.dylib',str(first)],commands)
            self.assertIn(['install_name_tool','-id','@rpath/libsecond.dylib',str(second)],commands)
            self.assertIn(['install_name_tool','-change',str(second),'@rpath/libsecond.dylib',str(first)],commands)

class ToolchainTests(unittest.TestCase):
    def setUp(self):
        # A harmless real executable exercises identity/version selection. This
        # test does not compile media libraries or claim native build success.
        executable=Path(sys.executable).resolve()
        version=subprocess.check_output([str(executable),'--version'],text=True).splitlines()[0]
        digest=hashlib.sha256(executable.read_bytes()).hexdigest()
        self.toolchain={'compiler':str(executable),'compiler_version':version,'compiler_sha256':digest,'provenance':'synthetic C driver','cxx_compiler':str(executable),'cxx_compiler_version':version,'cxx_compiler_sha256':digest,'cxx_provenance':'synthetic C++ driver'}

    def test_selected_c_and_cxx_are_forced_and_recorded(self):
        env,receipt=BUILDER_MODULE.select_toolchain(self.toolchain,{'PATH':''})
        for variable,key in [('CC','compiler'),('CXX','cxx_compiler')]:
            self.assertEqual(env.get(variable),self.toolchain[key])
        self.assertEqual(receipt.get('cc',{}).get('sha256'),self.toolchain['compiler_sha256'])
        self.assertEqual(receipt.get('cxx',{}).get('version'),self.toolchain['cxx_compiler_version'])
        self.assertNotIn(str(Path(sys.executable).parent),json.dumps(receipt))

    def test_different_inherited_compilers_are_rejected(self):
        for variable in ['CC','CXX']:
            with self.subTest(variable=variable):
                with self.assertRaises(BUILDER_MODULE.Blocked) as rejected:
                    BUILDER_MODULE.select_toolchain(self.toolchain,{variable:'unreviewed-driver'})
                self.assertEqual(str(rejected.exception),'inherited_compiler_mismatch')

    def test_cxx_hash_and_version_are_both_enforced(self):
        for key,value,code in [('cxx_compiler_sha256','0'*64,'compiler_hash'),('cxx_compiler_version','different','compiler_version')]:
            with self.subTest(key=key):
                changed=dict(self.toolchain)
                changed[key]=value
                with self.assertRaises(BUILDER_MODULE.Blocked) as rejected:
                    BUILDER_MODULE.select_toolchain(changed,{})
                self.assertEqual(str(rejected.exception),code)

    def test_inherited_cmake_toolchain_is_rejected(self):
        with self.assertRaises(BUILDER_MODULE.Blocked) as rejected:
            BUILDER_MODULE.select_toolchain(self.toolchain,{'CMAKE_TOOLCHAIN_FILE':'unreviewed.cmake'})
        self.assertEqual(str(rejected.exception),'compiler_override')


class LuaMakeRecipeTests(unittest.TestCase):
    def test_lua_make_recipe_builds_pic_archive_and_installs_pkg_config_metadata(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary)
            source=root/'source'
            source.mkdir()
            for name in ('lua.h','luaconf.h','lauxlib.h','lualib.h','lapi.c','ldebug.c','lctype.c','lbitlib.c','lcorolib.c'):
                (source/name).write_text('fixture\n')
            makefile='CORE_O= lapi.o ltests.o \\\n+  ldebug.o\nAUX_O= lauxlib.o\nLIB_O= linit.o\n'
            (source/'makefile').write_text(makefile.replace('+  ldebug', '  ldebug'))
            build=root/'build'
            build.mkdir()
            prefix=root/'sdk'
            prefix.mkdir()
            commands=[]

            def fake_run(command, cwd, env, log):
                commands.append(command)
                staged=Path(command[2])
                (staged/'liblua.a').write_bytes(b'fixture static archive')

            with mock.patch.object(BUILDER_MODULE,'run',side_effect=fake_run):
                BUILDER_MODULE.build_lua_make(source,build,prefix,
                    {'PATH':os.environ['PATH'],'CC':'/usr/bin/clang','CFLAGS':'-O2'},
                    ['-fPIC','-DLUA_USE_MACOSX'],root/'lua.log')

            command=commands[0]
            self.assertIn('a',command)
            self.assertIn('CORE_O=lapi.o ldebug.o lctype.o',command)
            self.assertIn('LIB_O=linit.o lbitlib.o lcorolib.o',command)
            self.assertIn('TESTS=',command)
            self.assertIn('MYCFLAGS=-fPIC -DLUA_USE_MACOSX -O2',command)
            self.assertEqual((prefix/'lib/liblua.a').read_bytes(),b'fixture static archive')
            self.assertIn('Version: 5.2.4',(prefix/'lib/pkgconfig/lua5.2.pc').read_text())
            self.assertIn('-llua -lm',(prefix/'lib/pkgconfig/lua5.2.pc').read_text())
            self.assertEqual((source/'lapi.c').read_text(),'fixture\n')


class SymlinkDriverTests(unittest.TestCase):
    def setUp(self):
        if os.name != 'posix':
            self.skipTest('POSIX argv0 driver fixture')
        self.temp=tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root=Path(self.temp.name)
        self.driver=root/'driver'
        self.driver.write_text('#!/bin/sh\nif [ "$1" = "--version" ]; then echo "synthetic shared compiler 1"; else case "${0##*/}" in *++) echo cxx;; *) echo c;; esac; fi\n')
        self.driver.chmod(0o755)
        self.cc,self.cxx=root/'cc',root/'c++'
        self.cc.symlink_to(self.driver.name)
        self.cxx.symlink_to(self.driver.name)
        digest=hashlib.sha256(self.driver.read_bytes()).hexdigest()
        self.toolchain={'compiler':str(self.cc),'compiler_version':'synthetic shared compiler 1','compiler_sha256':digest,'provenance':'argv0 dispatch fixture','cxx_compiler':str(self.cxx),'cxx_compiler_version':'synthetic shared compiler 1','cxx_compiler_sha256':digest,'cxx_provenance':'argv0 dispatch fixture'}

    def test_symlink_invocation_preserves_cxx_driver_mode(self):
        env,receipt=BUILDER_MODULE.select_toolchain(self.toolchain,{})
        self.assertEqual(subprocess.check_output([env['CC']],text=True).strip(),'c')
        self.assertEqual(subprocess.check_output([env['CXX']],text=True).strip(),'cxx')
        self.assertEqual(env['CXX'],str(self.cxx))
        self.assertEqual(receipt['cxx']['executable'],'c++')
        self.assertEqual(receipt['cc']['sha256'],receipt['cxx']['sha256'])

    def test_inherited_same_referent_with_different_driver_name_is_rejected(self):
        with self.assertRaises(BUILDER_MODULE.Blocked) as rejected:
            BUILDER_MODULE.select_toolchain(self.toolchain,{'CXX':str(self.cc)})
        self.assertEqual(str(rejected.exception),'inherited_compiler_mismatch')


class GeneratedOutputTests(unittest.TestCase):
    def test_existing_outputs_are_rejected_without_altering_sources_or_outputs(self):
        for name in ['sdk','build','logs','notices','sdk-receipt.json','bundle-config.json','activation.sh','activation.ps1','libmpv-2.def']:
            with self.subTest(output=name), tempfile.TemporaryDirectory() as temporary:
                work=Path(temporary)
                source=work/'source'/'reviewed'
                source.mkdir(parents=True)
                (source/'keep').write_bytes(b'reviewed source')
                output=work/name
                if name in ('sdk','build','logs','notices'):
                    output.mkdir()
                    retained=output/'keep'
                else:
                    retained=output
                retained.write_bytes(b'previous output')
                with self.assertRaises(BUILDER_MODULE.Blocked) as rejected:
                    BUILDER_MODULE.prepare_generated_outputs(work)
                self.assertEqual(str(rejected.exception),'generated_outputs_exist')
                self.assertEqual(retained.read_bytes(),b'previous output')
                self.assertEqual((source/'keep').read_bytes(),b'reviewed source')


class PristineSourceTests(unittest.TestCase):
    def replacement_fixture(self):
        temporary=tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root=Path(temporary.name)
        source=root/'source'
        source.mkdir()
        env=dict(os.environ)
        env.pop('GIT_NO_REPLACE_OBJECTS',None)
        def git(*arguments):
            return subprocess.check_output(['git','-C',str(source),*arguments],text=True,env=env,stderr=subprocess.DEVNULL)
        git('init','-q')
        git('config','user.name','Synthetic replacement fixture')
        git('config','user.email','test@example.invalid')
        (source/'other.c').write_text('original source\n')
        git('add','.')
        git('commit','-qm','original pinned source')
        original=git('rev-parse','HEAD').strip()
        (source/'other.c').write_text('unreviewed replacement\n')
        git('commit','-qam','replacement source')
        replacement=git('rev-parse','HEAD').strip()
        git('checkout','--detach',original)
        git('replace',original,replacement)
        git('reset','--hard',original)
        self.assertEqual(git('rev-parse','HEAD').strip(),original)
        return root,source,env

    def test_replacement_commit_cannot_hide_generic_source_changes(self):
        _,source,env=self.replacement_fixture()
        code='import importlib.util,sys; s=importlib.util.spec_from_file_location("builder",sys.argv[1]); m=importlib.util.module_from_spec(s); s.loader.exec_module(m); m.verify_pristine_source(m.Path(sys.argv[2]))'
        result=subprocess.run([sys.executable,'-B','-c',code,str(BUILDER),str(source)],env=env,capture_output=True,text=True)
        self.assertNotEqual(result.returncode,0,'replacement tree was admitted')
        self.assertIn('source_dirty',result.stderr)
        self.assertEqual((source/'other.c').read_text(),'unreviewed replacement\n')

    def test_source_receipt_archive_uses_original_objects(self):
        root,source,env=self.replacement_fixture()
        archive=root/'source.tar'
        BUILDER_MODULE.archive_source(source,archive,env)
        with tarfile.open(archive) as contents:
            self.assertEqual(contents.extractfile('other.c').read(),b'original source\n')

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
