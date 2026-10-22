#!/usr/bin/env python3
"""Build a reviewed native source closure. Incomplete locks stop before download.

This is a candidate-build recipe, not distribution approval. The separate
bundle verifier requires observed binary hashes/imports and reviewed notices.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shlex
import shutil
import subprocess
import sys

REPO = Path(__file__).resolve().parents[2]
FFMPEG_COMMIT = '140fd653aed8cad774f991ba083e2d01e86420c7'


class Blocked(Exception):
    pass


def need(condition, code):
    if not condition:
        raise Blocked(code)


def relative(value):
    need(isinstance(value, str) and value and not Path(value).is_absolute() and ':' not in value and '\\' not in value and all(part not in ('', '.', '..') for part in value.split('/')), 'plan_path')
    return value


def validate_plan(manifest, target):
    need(target in ('aarch64-apple-darwin', 'x86_64-pc-windows-msvc'), 'target')
    pin = manifest.get('targets', {}).get(target, {})
    sources, order = pin.get('sources', {}), pin.get('build_order', [])
    need(order and len(order) == len(set(order)) and set(order) == set(sources), 'dependency_order')
    need({'mpv', 'ffmpeg', 'libass', 'libplacebo', 'lua', 'dav1d'}.issubset(sources), 'dependency_order')
    seen = set()
    for name in order:
        source = sources[name]
        need(re.fullmatch(r'[a-z0-9][a-z0-9_-]*', name) and source.get('url', '').startswith('https://') and re.fullmatch(r'[0-9a-f]{40}', source.get('revision', '')), 'source_pin')
        need(source.get('license') not in (None, '', 'unknown', 'NOASSERTION') and source.get('license_files'), 'source_license')
        for value in source['license_files']:
            relative(value)
        need(source.get('build_system') in ('cmake', 'meson', 'configure', 'lua-make') and isinstance(source.get('configure_flags'), list) and source['configure_flags'] and all(isinstance(value, str) for value in source['configure_flags']), 'source_recipe')
        need(source['build_system'] != 'lua-make' or (name == 'lua' and target == 'aarch64-apple-darwin'), 'source_recipe')
        for flag in source['configure_flags']:
            option = flag.split('=', 1)[0]
            cmake_option = flag[2:] if flag.startswith('-D') else flag
            need(option not in ('CC', 'CXX', '--cc', '--cxx', '--native-file', '--cross-file', '--toolchain')
                 and not cmake_option.startswith(('CMAKE_C_COMPILER', 'CMAKE_CXX_COMPILER', 'CMAKE_TOOLCHAIN_FILE')), 'compiler_override')
        need(set(source.get('dependencies', [])).issubset(seen), 'dependency_order')
        for submodule in source.get('submodules', []):
            relative(submodule.get('path'))
            need(submodule.get('url', '').startswith('https://') and
                 re.fullmatch(r'[0-9a-f]{40}', submodule.get('revision', '')) and
                 submodule.get('license') not in (None, '', 'unknown', 'NOASSERTION') and
                 submodule.get('license_files') and
                 all(relative(value) for value in submodule['license_files']) and
                 submodule.get('build_system') in
                 ('python-source-module', 'python-build-generator', 'header-only') and
                 isinstance(submodule.get('configure_flags'), list) and
                 all(isinstance(value, str) for value in submodule['configure_flags']),
                 'submodule_pin')
            for generated_input in submodule.get('generated_inputs', []):
                relative(generated_input.get('source'))
                relative(generated_input.get('build_file'))
                need(re.fullmatch(r'[0-9a-f]{64}', generated_input.get('sha256', '')),
                     'submodule_generated_input')
        if source.get('submodules'):
            submodule_paths = [entry['path'] for entry in source['submodules']]
            submodule_order = source.get('submodule_order', [])
            need(len(submodule_paths) == len(set(submodule_paths)) and
                 len(submodule_order) == len(set(submodule_order)) and
                 set(submodule_order) == set(submodule_paths), 'submodule_order')
            submodules_by_path = {entry['path']: entry for entry in source['submodules']}
            seen_submodules = set()
            for submodule_path in submodule_order:
                need(set(submodules_by_path[submodule_path].get('dependencies', []))
                     .issubset(seen_submodules), 'submodule_order')
                seen_submodules.add(submodule_path)
        seen.add(name)
    need(sources['ffmpeg']['revision'] == FFMPEG_COMMIT and sources['ffmpeg']['url'] == 'https://github.com/FFmpeg/FFmpeg.git', 'ffmpeg_pin')
    need({'--enable-shared', '--disable-static', '--disable-programs', '--enable-gpl', '--enable-libdav1d'}.issubset(sources['ffmpeg']['configure_flags']) and 'dav1d' in sources['ffmpeg']['dependencies'], 'ffmpeg_flags')
    need('--disable-postproc' not in sources['ffmpeg']['configure_flags'], 'ffmpeg_flags')
    mpv = manifest['linux_development']['mpv']
    need(sources['mpv']['revision'] == mpv['revision'] and sources['mpv']['url'] == mpv['source_url'], 'mpv_pin')
    need(pin.get('mpv_patches') == [{key: item[key] for key in ('file', 'sha256')} for item in mpv['patches']], 'mpv_patch')
    for item in pin['mpv_patches']:
        need(hashlib.sha256((REPO / relative(item['file'])).read_bytes()).hexdigest() == item['sha256'], 'mpv_patch')
    need({'-Dlibmpv=true', '-Dcplayer=false', '-Dlua=lua5.2', '-Dgl=enabled', '-Dplain-gl=enabled', '-Dauto_features=disabled'}.issubset(sources['mpv']['configure_flags']), 'mpv_flags')
    toolchain = pin.get('toolchain', {})
    need(pin.get('os_floor') and all(isinstance(toolchain.get(key), str) and toolchain[key]
         for key in ('compiler', 'compiler_version', 'provenance', 'cxx_compiler', 'cxx_compiler_version', 'cxx_provenance')), 'toolchain_provenance')
    need(all(re.fullmatch(r'[0-9a-f]{64}', toolchain.get(key, ''))
         for key in ('compiler_sha256', 'cxx_compiler_sha256')), 'toolchain_provenance')
    runtime = pin.get('runtime_files', {})
    need(runtime and {'mpv', 'ffmpeg'}.issubset(runtime) and set(runtime).issubset(sources), 'runtime_file_plan')
    for files in runtime.values():
        need(files and all(relative(name) for name in files), 'runtime_file_plan')
    # Validate every recorded field before honoring the readiness assertion.
    # This lets reviewers use --check-plan to identify plan defects without
    # prematurely marking an incomplete source lock as ready.
    need(pin.get('source_lock_complete') is True, 'source_lock_incomplete')
    return pin


def source_git_environment(inherited=None):
    # Commit IDs must refer to their original objects, regardless of local
    # refs/replace or an inherited replacement namespace.
    return dict(os.environ if inherited is None else inherited,
                GIT_NO_REPLACE_OBJECTS='1')


def run(command, cwd, env, log):
    # Full local compiler output stays in ignored build logs, not public reports.
    if command[0] == 'git':
        env = source_git_environment(env)
    with log.open('ab') as output:
        subprocess.run(command, cwd=cwd, env=env, stdout=output, stderr=subprocess.STDOUT, check=True)


def verify_pristine_source(source):
    # Do not let index hints or ignored files hide inputs from the source lock.
    # All generated outputs belong in the separate build directory.
    command = ['git', '--no-optional-locks', '-c', 'core.fsmonitor=false',
               '-c', 'core.untrackedCache=false', '-C', str(source)]
    env = source_git_environment()
    entries = subprocess.check_output([*command, 'ls-files', '-v', '-z'], env=env).split(b'\0')
    need(all(not entry or (entry[:1] != b'S' and not entry[:1].islower()) for entry in entries), 'source_dirty')
    need(not subprocess.check_output([*command, 'ls-files', '--others', '-z'], env=env), 'source_dirty')
    for staged in ([], ['--cached']):
        result = subprocess.run([*command, 'diff', '--no-ext-diff', '--no-textconv',
                                 '--quiet', *staged, '--'], env=env, stderr=subprocess.DEVNULL)
        need(result.returncode == 0, 'source_dirty')


def archive_source(source, destination, env=None):
    with destination.open('wb') as archive:
        subprocess.run(['git', '-C', str(source), 'archive', '--format=tar', 'HEAD'],
                       stdout=archive, env=source_git_environment(env), check=True)


def initialize_pinned_submodules(source, submodules, env, log):
    if not submodules:
        return
    config = subprocess.check_output(
        ['git', '-C', str(source), 'config', '-f', '.gitmodules', '--get-regexp',
         r'^submodule\..*\.path$'], env=source_git_environment(), text=True)
    paths = {}
    for line in config.splitlines():
        key, value = line.split(None, 1)
        name = key[len('submodule.'):-len('.path')]
        paths[value] = name
    for item in submodules:
        relative_path = relative(item['path'])
        name = paths.get(relative_path)
        need(name is not None, 'submodule_path')
        url = subprocess.check_output(
            ['git', '-C', str(source), 'config', '-f', '.gitmodules', '--get',
             'submodule.' + name + '.url'], env=source_git_environment(), text=True).strip()
        need(url == item['url'], 'submodule_url')
        tree_entry = subprocess.check_output(
            ['git', '-C', str(source), 'ls-tree', 'HEAD', '--', relative_path],
            env=source_git_environment(), text=True).strip().split()
        need(len(tree_entry) >= 3 and tree_entry[1] == 'commit' and
             tree_entry[2] == item['revision'], 'submodule_pin')
        run(['git', '-C', str(source), 'submodule', 'update', '--init',
             '--depth=1', '--', relative_path], source, env, log)
        checkout = source / relative_path
        need(checkout.resolve().is_relative_to(source.resolve()), 'submodule_path')
        need(subprocess.check_output(['git', '-C', str(checkout), 'rev-parse', 'HEAD'],
             env=source_git_environment(env), text=True).strip() == item['revision'],
             'submodule_pin')
        verify_pristine_source(checkout)
        for license_file in item['license_files']:
            path = checkout / relative(license_file)
            need(path.is_file() and path.resolve().is_relative_to(checkout.resolve()),
                 'source_license')


def select_toolchain(toolchain, inherited):
    env, receipt = dict(inherited), {}
    need(not env.get('CMAKE_TOOLCHAIN_FILE'), 'compiler_override')
    for variable, key, provenance in [('CC', 'compiler', 'provenance'), ('CXX', 'cxx_compiler', 'cxx_provenance')]:
        selected = shutil.which(toolchain[key], path=inherited.get('PATH'))
        need(selected, 'build_tool_missing')
        # Preserve argv0: clang++ may be a symlink to clang, and resolving it
        # for execution would silently select the C driver/linker mode.
        executable = Path(selected).absolute()
        referent = executable.resolve(strict=True)
        if inherited.get(variable):
            previous = shutil.which(inherited[variable], path=inherited.get('PATH'))
            need(previous and Path(previous).absolute() == executable, 'inherited_compiler_mismatch')
        digest = hashlib.sha256(referent.read_bytes()).hexdigest()
        need(digest == toolchain[key + '_sha256'], 'compiler_hash')
        version = subprocess.check_output([str(executable), '--version'], text=True).splitlines()[0]
        need(version == toolchain[key + '_version'], 'compiler_version')
        env[variable] = str(executable)
        receipt[variable.lower()] = {'executable': executable.name, 'sha256': digest,
                                    'version': version, 'provenance': toolchain[provenance]}
    return env, receipt


def prepare_generated_outputs(work):
    generated = ('sdk', 'build', 'logs', 'notices', 'sdk-receipt.json',
                 'bundle-config.json', 'activation.sh', 'activation.ps1', 'libmpv-2.def')
    need(all(not (work / name).exists() and not (work / name).is_symlink()
             for name in generated), 'generated_outputs_exist')
    work.mkdir(parents=True, exist_ok=True)
    prefix, logs, notices = work / 'sdk', work / 'logs', work / 'notices'
    prefix.mkdir()
    (work / 'build').mkdir()
    logs.mkdir()
    notices.mkdir()
    return prefix, logs, notices


def build_lua_make(source, build, prefix, env, flags, log):
    """Build the pinned Lua source with its upstream makefile as PIC static code.

    Lua 5.2's upstream makefile only creates liblua.a. The SDK needs a
    position-independent archive so mpv can link it into libmpv.dylib.
    Stage the pristine source into the separate build directory, disable its
    test-only ltests object, then install headers and a lua52.pc file.
    """
    need(platform.system() == 'Darwin', 'lua_make_target')
    makefile = source / 'makefile'
    need(makefile.is_file(), 'lua_makefile')
    text = makefile.read_text()
    match = re.search(r'(?m)^CORE_O=\s*((?:[^\n]*\\\n)*[^\n]*)', text)
    need(match is not None, 'lua_makefile')
    core_objects = re.sub(r'\\\n\s*', ' ', match.group(1)).split()
    need('ltests.o' in core_objects, 'lua_makefile')
    core_objects.remove('ltests.o')

    # Lua 5.2.4's pinned upstream makefile omits objects needed by mpv:
    # lctype.o defines luai_ctype_, while lbitlib.o and lcorolib.o provide
    # luaopen_bit32 and luaopen_coroutine respectively.
    need((source / 'lctype.c').is_file(), 'lua_ctype_source_missing')
    if 'lctype.o' not in core_objects:
        core_objects.append('lctype.o')
    lib_match = re.search(r'(?m)^LIB_O=\s*((?:[^\n]*\\\n)*[^\n]*)', text)
    need(lib_match is not None, 'lua_makefile')
    lib_objects = re.sub(r'\\\n\s*', ' ', lib_match.group(1)).split()
    for object_name, source_name, code in (
        ('lbitlib.o', 'lbitlib.c', 'lua_bitlib_source_missing'),
        ('lcorolib.o', 'lcorolib.c', 'lua_corolib_source_missing'),
    ):
        need((source / source_name).is_file(), code)
        if object_name not in lib_objects:
            lib_objects.append(object_name)

    staged = build / 'lua-source'
    staged.mkdir()
    for path in source.iterdir():
        if path.is_file() and (path.suffix in ('.c', '.h') or path.name == 'makefile'):
            shutil.copy2(path, staged / path.name)

    make = shutil.which('make', path=env.get('PATH'))
    need(make, 'build_tool_missing')
    mycflags = ' '.join([*flags, *shlex.split(env.get('CFLAGS', ''))])
    command = [make, '-C', str(staged), '-f', 'makefile', '-j3', 'a',
               'CC=' + env['CC'], 'AR=ar rcu', 'RANLIB=ranlib',
               'TESTS=', 'MYCFLAGS=' + mycflags,
               'CORE_O=' + ' '.join(core_objects),
               'LIB_O=' + ' '.join(lib_objects)]
    run(command, build, env, log)

    library = staged / 'liblua.a'
    need(library.is_file(), 'lua_library_missing')
    libdir = prefix / 'lib'
    includedir = prefix / 'include/lua5.2'
    pcdir = libdir / 'pkgconfig'
    libdir.mkdir(parents=True, exist_ok=True)
    includedir.mkdir(parents=True, exist_ok=True)
    pcdir.mkdir(parents=True, exist_ok=True)
    shutil.copy2(library, libdir / 'liblua.a')
    for name in ('lua.h', 'luaconf.h', 'lauxlib.h', 'lualib.h'):
        header = source / name
        need(header.is_file(), 'lua_header_missing')
        shutil.copy2(header, includedir / name)
    (pcdir / 'lua5.2.pc').write_text(
        'prefix=' + str(prefix) + '\n'
        'libdir=${prefix}/lib\n'
        'includedir=${prefix}/include/lua5.2\n\n'
        'Name: Lua 5.2\nDescription: Lua 5.2 static library\n'
        'Version: 5.2.4\nLibs: -L${libdir} -llua -lm\n'
        'Cflags: -I${includedir}\n'
    )


def stage_generator_inputs(source, build, submodules):
    """Stage hash-locked generated-code inputs in the Meson build root."""
    for submodule in submodules:
        for item in submodule.get('generated_inputs', []):
            source_file = source / relative(submodule['path']) / relative(item['source'])
            need(source_file.is_file() and
                 hashlib.sha256(source_file.read_bytes()).hexdigest() == item['sha256'],
                 'submodule_generated_input')
            output = build / relative(item['build_file'])
            need(not output.exists(), 'submodule_generated_input')
            shutil.copy2(source_file, output)


def relocate_macos_libraries(prefix, runtime_files, env, log):
    """Give SDK dylibs relocatable IDs and references for Tauri Frameworks."""
    need(shutil.which('install_name_tool', path=env.get('PATH')) and
         shutil.which('otool', path=env.get('PATH')), 'build_tool_missing')
    libraries = [prefix / relative(name)
                 for files in runtime_files.values() for name in files]
    need(all(path.is_file() for path in libraries), 'runtime_artifact_missing')
    library_names = {path.name for path in libraries}
    for library in libraries:
        run(['install_name_tool', '-id', '@rpath/' + library.name, str(library)],
            REPO, env, log)
    sdk_lib = (prefix / 'lib').resolve()
    for library in libraries:
        loads = subprocess.check_output(['otool', '-L', str(library)],
                                        env=env, text=True).splitlines()[1:]
        for line in loads:
            dependency = line.strip().split(' (compatibility', 1)[0]
            candidate = Path(dependency)
            if (candidate.parent.resolve() == sdk_lib and
                    candidate.name in library_names):
                run(['install_name_tool', '-change', dependency,
                     '@rpath/' + candidate.name, str(library)], REPO, env, log)
    for library in libraries:
        loads = subprocess.check_output(['otool', '-L', str(library)],
                                        env=env, text=True).splitlines()[1:]
        need(all(not line.strip().split(' (compatibility', 1)[0].startswith(str(sdk_lib) + '/')
                 for line in loads), 'absolute_runtime_import')


def build_sdk(pin, target, manifest_path, work):
    mac = target == 'aarch64-apple-darwin'
    need((mac and platform.system() == 'Darwin' and platform.machine() == 'arm64') or (not mac and os.name == 'nt'), 'native_host_required')
    for command in ('git', 'cmake', 'meson', 'ninja', 'make', 'pkg-config'):
        need(shutil.which(command), 'build_tool_missing')
    env, toolchain = select_toolchain(pin['toolchain'], os.environ)
    env = source_git_environment(env)
    # Meson source generators import modules from pinned Git submodules.
    # Keep Python bytecode caches out of those pristine source checkouts.
    env['PYTHONDONTWRITEBYTECODE'] = '1'
    prefix, logs, notices = prepare_generated_outputs(work)
    env.update(PKG_CONFIG_PATH=str(prefix / 'lib/pkgconfig') + os.pathsep + str(prefix / 'share/pkgconfig'), PKG_CONFIG_LIBDIR=str(prefix / 'lib/pkgconfig') + os.pathsep + str(prefix / 'share/pkgconfig'), CMAKE_PREFIX_PATH=str(prefix), CMAKE_IGNORE_PREFIX_PATH='/opt/homebrew;/usr/local')
    env['PATH'] = str(prefix / 'bin') + os.pathsep + env['PATH']
    env['CPPFLAGS'] = '-I' + shlex.quote(str(prefix / 'include'))
    env['LDFLAGS'] = '-L' + shlex.quote(str(prefix / 'lib'))
    env['CFLAGS'] = '-O2 -ffile-prefix-map=' + shlex.quote(str(work)) + '=.'
    env['CXXFLAGS'] = env['CFLAGS']
    if mac:
        env['MACOSX_DEPLOYMENT_TARGET'] = pin['os_floor']
    else:
        need(env.get('MSYSTEM') == 'MINGW64', 'mingw64_environment_required')
        for variable in ('CC', 'CXX'):
            need(subprocess.check_output([env[variable], '-dumpmachine'], text=True).strip() == 'x86_64-w64-mingw32', 'compiler_target')
    receipts = {}
    for name in pin['build_order']:
        item = pin['sources'][name]
        source = work / 'source' / name
        build = work / 'build' / name
        source.parent.mkdir(exist_ok=True)
        build.mkdir()
        log = logs / (name + '.log')
        if not source.exists():
            run(['git', 'init', str(source)], work, env, log)
            run(['git', '-C', str(source), 'fetch', '--depth=1', item['url'], item['revision']], work, env, log)
            run(['git', '-C', str(source), 'checkout', '--detach', 'FETCH_HEAD'], work, env, log)
        need(subprocess.check_output(['git', '-C', str(source), 'rev-parse', 'HEAD'], env=env, text=True).strip() == item['revision'], 'source_revision')
        if name == 'mpv':
            run([sys.executable, str(REPO / 'scripts/native/verify-mpv-source.py'), str(source), str(manifest_path), str(REPO), '--apply'], work, env, log)
        else:
            verify_pristine_source(source)
        initialize_pinned_submodules(source, item.get('submodules', []), env, log)
        for license_file in item['license_files']:
            path = source / relative(license_file)
            need(path.is_file() and path.resolve().is_relative_to(source.resolve()), 'source_license')
        flags = item['configure_flags']
        if item['build_system'] == 'meson':
            need(not (build / 'meson-private/coredata.dat').exists(), 'fresh_build_directory_required')
            run(['meson', 'setup', str(build), str(source), '--prefix=' + str(prefix), '--default-library=shared', '--wrap-mode=nofallback', *flags], work, env, log)
            stage_generator_inputs(source, build, item.get('submodules', []))
            run(['meson', 'compile', '-C', str(build), '-j3'], work, env, log)
            run(['meson', 'install', '-C', str(build)], work, env, log)
        elif item['build_system'] == 'cmake':
            options = ['-DCMAKE_OSX_ARCHITECTURES=arm64', '-DCMAKE_OSX_DEPLOYMENT_TARGET=' + pin['os_floor']] if mac else ['-G', 'Ninja']
            run(['cmake', '-S', str(source), '-B', str(build), '-DCMAKE_INSTALL_PREFIX=' + str(prefix), '-DBUILD_SHARED_LIBS=ON', '-DCMAKE_FIND_USE_PACKAGE_REGISTRY=OFF', '-DCMAKE_FIND_USE_SYSTEM_PACKAGE_REGISTRY=OFF', *options, *flags, '-DCMAKE_C_COMPILER=' + env['CC'], '-DCMAKE_CXX_COMPILER=' + env['CXX']], work, env, log)
            run(['cmake', '--build', str(build), '--parallel', '3'], work, env, log)
            run(['cmake', '--install', str(build)], work, env, log)
        elif item['build_system'] == 'lua-make':
            build_lua_make(source, build, prefix, env, flags, log)
        else:
            drivers = ['--cc=' + env['CC'], '--cxx=' + env['CXX']] if name == 'ffmpeg' else []
            run(['sh', str(source / 'configure'), '--prefix=' + prefix.as_posix(), *flags, *drivers], build, env, log)
            run(['make', '-j3'], build, env, log)
            run(['make', 'install'], build, env, log)
        component_notices = notices / name
        component_notices.mkdir()
        for license_file in item['license_files']:
            shutil.copy2(source / license_file, component_notices / Path(license_file).name)
        archive_source(source, component_notices / (name + '-source.tar'), env)
        submodule_receipts = []
        for submodule in item.get('submodules', []):
            checkout = source / relative(submodule['path'])
            sub_notice = component_notices / 'submodules' / Path(submodule['path']).name
            sub_notice.mkdir(parents=True)
            for license_file in submodule['license_files']:
                shutil.copy2(checkout / relative(license_file),
                             sub_notice / Path(license_file).name)
            archive_source(checkout,
                           sub_notice / (Path(submodule['path']).name + '-source.tar'), env)
            submodule_receipts.append({key: submodule[key] for key in
                                       ('path', 'url', 'revision', 'license', 'license_files')})
        if name == 'mpv':
            for patch in pin['mpv_patches']:
                shutil.copy2(REPO / patch['file'], component_notices / Path(patch['file']).name)
        receipts[name] = {key: item[key] for key in ('url', 'revision', 'license', 'license_files', 'configure_flags', 'dependencies', 'build_system')}
        if submodule_receipts:
            receipts[name]['submodules'] = submodule_receipts
        (component_notices / 'SOURCE-BUILD.json').write_text(json.dumps(receipts[name], indent=2) + '\n')
    if mac:
        relocate_macos_libraries(prefix, pin['runtime_files'], env, logs / 'relocate.log')
    artifacts = []
    for component, names in pin['runtime_files'].items():
        for name in names:
            path = prefix / relative(name)
            need(path.is_file() and path.resolve().is_relative_to(prefix.resolve()), 'runtime_artifact_missing')
            artifacts.append({'file': name, 'component': component, 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()})
    if not mac:
        need(shutil.which('gendef') and shutil.which('lib.exe'), 'msvc_import_library_tools')
        dlls = [prefix / name for name in pin['runtime_files']['mpv'] if name.lower().endswith('libmpv-2.dll')]
        need(len(dlls) == 1, 'mpv_dll_name')
        run(['gendef', str(dlls[0])], work, env, logs / 'import-library.log')
        run(['lib.exe', '/nologo', '/machine:x64', '/def:' + str(work / 'libmpv-2.def'), '/out:' + str(prefix / 'lib/mpv.lib')], work, env, logs / 'import-library.log')
    evidence = {'status': 'sdk_built_distribution_unreviewed', 'target': target, 'os': platform.system() + ' ' + platform.release(), 'toolchain': toolchain, 'os_floor': pin['os_floor'], 'install_name_scheme': '@rpath/<runtime filename>', 'sources': receipts, 'artifacts': artifacts, 'mpv_patches': pin['mpv_patches']}
    (work / 'sdk-receipt.json').write_text(json.dumps(evidence, indent=2) + '\n')
    # No paths are emitted in the shareable receipt. Activation is local only.
    resources = {str(path): 'licenses/' + path.relative_to(notices).as_posix() for path in notices.rglob('*') if path.is_file()}
    libraries = [str(prefix / name) for names in pin['runtime_files'].values() for name in names]
    config = {'bundle': {'active': True, 'resources': resources}}
    if mac:
        config['bundle']['macOS'] = {'minimumSystemVersion': pin['os_floor'], 'frameworks': libraries}
    else:
        config['bundle']['resources'].update({name: Path(name).name for name in libraries})
        # Candidate installer uses the explicitly verified system runtime; no
        # network bootstrapper is downloaded implicitly. Clean-machine QA is
        # still blocked until a runtime/installer is inventoried and tested.
        need(pin.get('webview2', {}).get('mode') == 'system_verified', 'webview2_unverified')
        config['bundle']['windows'] = {'webviewInstallMode': {'type': 'skip'}}
    (work / 'bundle-config.json').write_text(json.dumps(config, indent=2) + '\n')
    native = str(prefix / 'lib')
    (work / 'activation.sh').write_text('export PATH=' + shlex.quote(str(prefix / 'bin')) + ':"$PATH"\nexport LIBRARY_PATH=' + shlex.quote(native) + ':"${LIBRARY_PATH:-}"\nexport DYLD_LIBRARY_PATH=' + shlex.quote(native) + ':"${DYLD_LIBRARY_PATH:-}"\nexport CARGO_ENCODED_RUSTFLAGS=' + shlex.quote('-L\x1fnative=' + native) + '\n')
    path_ps = (str(prefix / 'bin') + ';').replace("'", "''")
    flags_ps = ('native=' + native).replace("'", "''")
    (work / 'activation.ps1').write_text("$env:PATH = '" + path_ps + "' + $env:PATH\n$env:CARGO_ENCODED_RUSTFLAGS = '-L' + [char]31 + '" + flags_ps + "'\n")
    print('native_sdk_built_distribution_unreviewed')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--target', required=True)
    parser.add_argument('--manifest', type=Path, default=REPO / 'docs/development/native-dependencies.json')
    parser.add_argument('--check-plan', action='store_true')
    args = parser.parse_args()
    try:
        pin = validate_plan(json.loads(args.manifest.read_text()), args.target)
        if args.check_plan:
            print('source_plan_verified')
        else:
            build_sdk(pin, args.target, args.manifest.resolve(), REPO / 'target/native' / args.target)
    except Blocked as error:
        print('native_build_blocked:' + str(error), file=sys.stderr)
        return 1
    except (OSError, ValueError, KeyError, TypeError, subprocess.CalledProcessError):
        print('native_build_blocked:recipe_or_tool_failure', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
