#!/usr/bin/env python3
"""Prepare artifact-bound Windows native evidence from the recorded final link.

Run on the native Windows build runner, after linking and before packaging.
Unknown native inputs, stale link records and mismatching SDK payloads fail closed.
No empty static-code declaration is inferred from the absence of bundled DLLs.
"""
import argparse
from collections import Counter
import hashlib
import importlib.util
import json
import ntpath
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tomllib
from urllib.request import urlopen
import zipfile

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location('native_collector', ROOT / 'scripts/collect-native-licenses.py')
COLLECTOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(COLLECTOR)
NOTICE_SPEC = importlib.util.spec_from_file_location('cargo_notice_evidence', ROOT / 'scripts/generate-cargo-notices.py')
CARGO_NOTICES = importlib.util.module_from_spec(NOTICE_SPEC)
NOTICE_SPEC.loader.exec_module(CARGO_NOTICES)
LINK_SPEC = importlib.util.spec_from_file_location('windows_link_recorder', ROOT / 'scripts/record-native-link.py')
LINK_RECORDER = importlib.util.module_from_spec(LINK_SPEC)
LINK_SPEC.loader.exec_module(LINK_RECORDER)
VS_TERMS = 'https://visualstudio.microsoft.com/wp-content/uploads/2024/03/Visual-Studio-2022-Diagnostic-Build-Tools-Agent-License_Update-March-2024_EN.docx'
VS_PRODUCT_TERMS = 'https://visualstudio.microsoft.com/wp-content/uploads/2021/11/Visual-Studio-2022-Enterprise-Professional-License-EN.docx'
VS_RUNTIME_TERMS = 'https://visualstudio.microsoft.com/wp-content/uploads/2021/09/Visual-C-Runtime-2015-2022-License-1.docx'
VS_REDIST = 'https://learn.microsoft.com/en-us/visualstudio/releases/2022/redistribution'
VS_GUIDANCE = 'https://www.microsoft.com/licensing/guidance/Visual-Studio'
CRT_DESCRIPTION = 'https://learn.microsoft.com/en-us/cpp/c-runtime-library/crt-library-features?view=msvc-170'
CRT_DEPLOYMENT = 'https://raw.githubusercontent.com/microsoft/WindowsAppSDK/main/docs/Coding-Guidelines/HybridCRT.md'
VISUAL_STUDIO_PROFILES = {
    17: {'year': '2022', 'tools_terms': VS_TERMS, 'product_terms': VS_PRODUCT_TERMS,
         'runtime_terms': VS_RUNTIME_TERMS, 'redist': VS_REDIST},
    18: {'year': '2026',
         'tools_terms': 'https://visualstudio.microsoft.com/wp-content/uploads/2025/10/Visual_Studio_2026-License-Diagnostic_and_Build_Tools_ENU.docx',
         'product_terms': 'https://visualstudio.microsoft.com/wp-content/uploads/2025/10/Visual_Studio_2026-License-Enterprise_Professional_ENU.docx',
         'runtime_terms': 'https://visualstudio.microsoft.com/wp-content/uploads/2025/10/Visual-C-V14-License-Redistributable_and_Runtime_ENU.docx',
         'redist': 'https://learn.microsoft.com/en-us/visualstudio/releases/2026/redistribution'},
}


def visual_studio_profile(version):
    if not isinstance(version, str) or not re.fullmatch(r'[0-9]+(?:\.[0-9]+){1,3}', version):
        raise ValueError('Toolchain record needs the installed Visual Studio product version')
    major = int(version.split('.', 1)[0])
    if major not in VISUAL_STUDIO_PROFILES:
        raise ValueError('Vendor terms need review for this Visual Studio major version')
    return VISUAL_STUDIO_PROFILES[major]


def digest(path):
    with Path(path).open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def native_package_notice(package, archive, notice_manifest, overrides, repository=ROOT):
    """Retain archive texts and authenticated upstream supplements for that crate.

    The caller validates the complete retained Cargo notice manifest first.
    Wrapper terms do not replace separately inventoried vendor SDK terms.
    """
    key = f"{package['name']}@{package['version']}"
    records = [record for record in notice_manifest['packages'] if record['package'] == key]
    if (len(records) != 1 or records[0]['source_sha256'] != package['checksum']
            or digest(archive) != package['checksum']):
        raise ValueError(f'Native crate notice evidence differs from the locked archive: {key}')
    parts = []
    try:
        parts.append(COLLECTOR.archive_notices(package, archive))
    except ValueError as error:
        if not str(error).endswith(': no license texts'):
            raise
    references = {item['path']: item['text_sha256'] for item in records[0]['texts']}
    for extra in overrides.get(key, {}).get('files', []):
        path = (repository / extra['path']).resolve()
        if not path.is_relative_to(repository.resolve()):
            raise ValueError(f'Pinned native licence supplement escapes the repository: {key}')
        raw = path.read_bytes()
        if hashlib.sha256(raw).hexdigest() != extra['sha256']:
            raise ValueError(f'Pinned native licence supplement SHA256 mismatch: {key}')
        text = raw.decode('utf-8-sig')
        origin = extra.get('url', extra['path'])
        if references.get(origin) != hashlib.sha256(text.encode()).hexdigest():
            raise ValueError(f'Pinned native licence supplement differs from Cargo notice manifest: {key}')
        parts.append(f'\nSource: {origin}\nSHA256: {extra["sha256"]}\n\n' + text)
    if not parts:
        raise ValueError(f'No authenticated native crate licence text or pinned supplement: {key}')
    return (f'Locked Cargo package: {key}\n'
            'These package texts do not replace separately retained vendor/native SDK terms.\n'
            + '\n'.join(parts))


def download(url, path, expected=None):
    path = Path(path)
    if not path.exists() or expected is None or digest(path) != expected:
        partial = path.with_suffix(path.suffix + '.part')
        try:
            with urlopen(url, timeout=120) as response, partial.open('wb') as output:
                shutil.copyfileobj(response, output)
            if expected and digest(partial) != expected:
                raise ValueError(f'Official download checksum mismatch: {url}')
            if path.suffix.lower() == '.docx':
                if not zipfile.is_zipfile(partial):
                    raise ValueError(f'Vendor terms download is not a DOCX document: {url}')
                with zipfile.ZipFile(partial) as document:
                    text = document.read('word/document.xml').lower()
                    if b'microsoft' not in text or b'license' not in text:
                        raise ValueError(f'Vendor terms document has no Microsoft licence text: {url}')
            partial.replace(path)
        finally:
            partial.unlink(missing_ok=True)
    return path


def installed_sdk_terms(root, version, explicit=None):
    """Use dedicated installed SDK terms, never a random source/header licence."""
    if explicit:
        paths = [explicit]
    else:
        paths = []
        for directory in (root / 'Licenses' / version, root / 'Licenses'):
            if directory.is_dir():
                for path in directory.rglob('*'):
                    versions = [part for part in path.relative_to(root / 'Licenses').parts
                                if re.fullmatch(r'[0-9]+(?:\.[0-9]+){2,3}', part)]
                    if (path.is_file() and path.suffix.lower() in ('.rtf', '.txt', '.html', '.docx')
                            and all(installed == version for installed in versions)):
                        paths.append(path)
        paths += [root / name for name in ('License.rtf', 'SDKLicense.rtf', 'SDKLicense.txt')
                  if (root / name).is_file()]
    paths = list(dict.fromkeys(paths))
    if not paths or any(not path.is_file() or path.stat().st_size == 0 for path in paths):
        raise ValueError('Installed Windows SDK terms are missing; provide its actual SDK licence with --windows-sdk-terms')
    return paths


def prepare_vendor_terms(output, sdk_root, sdk_version, explicit, profile):
    sdk_term_names = []
    for index, path in enumerate(installed_sdk_terms(sdk_root, sdk_version, explicit)):
        retained = output / ('windows-sdk-terms-' + str(index) + path.suffix)
        shutil.copyfile(path, retained)
        sdk_term_names.append(retained.name)
    resources = [('visual-studio-terms.docx', profile['tools_terms']), ('visual-studio-product-terms.docx', profile['product_terms']),
                 ('visual-studio-runtime-terms.docx', profile['runtime_terms']), ('visual-studio-redist.html', profile['redist']),
                 ('visual-studio-guidance.html', VS_GUIDANCE), ('crt-linkage.html', CRT_DESCRIPTION),
                 ('crt-deployment.md', CRT_DEPLOYMENT)]
    for filename, url in resources:
        download(url, output / filename)
    return (['visual-studio-terms.docx', 'visual-studio-product-terms.docx', 'visual-studio-runtime-terms.docx'],
            ['visual-studio-redist.html', 'visual-studio-guidance.html', 'crt-linkage.html', 'crt-deployment.md'], sdk_term_names)


def archive_members(data):
    """Read COFF/GNU/BSD ar members without executing a platform tool."""
    if data[:8] != b'!<arch>\n':
        raise ValueError('Native linker input is not an ar archive')
    offset, long_names = 8, b''
    while offset < len(data):
        header = data[offset:offset + 60]
        if len(header) != 60 or header[58:] != b'`\n':
            raise ValueError('Malformed native archive header')
        name = header[:16].decode('ascii').strip()
        size = int(header[48:58])
        start = offset + 60
        payload = data[start:start + size]
        if len(payload) != size:
            raise ValueError('Truncated native archive member')
        offset = start + size + size % 2
        if name == '//':
            long_names = payload
            continue
        if name in ('/', '/SYM64/'):
            continue
        if name.startswith('#1/'):
            length = int(name[3:])
            name, payload = payload[:length].rstrip(b'\0').decode(errors='replace'), payload[length:]
        elif name.startswith('/') and name[1:].isdigit():
            name = re.split(b'/\n|\0', long_names[int(name[1:]):], maxsplit=1)[0].decode(errors='replace')
        else:
            name = name.rstrip('/')
        yield name, payload


def archive_payload_hashes(data):
    hashes = Counter()
    for _, payload in archive_members(data):
        if payload.startswith(b'!<arch>\n'):
            hashes.update(archive_payload_hashes(payload))
        else:
            hashes[hashlib.sha256(payload).hexdigest()] += 1
    return hashes


def verify_bundled_loader(loader, wrappers):
    required = archive_payload_hashes(loader)
    if not required:
        raise ValueError('Official SDK loader has no object members')
    for wrapper in wrappers:
        available = archive_payload_hashes(wrapper.read_bytes())
        if required - available:
            raise ValueError('Linked WebView2 wrapper does not contain the matching official SDK loader objects')


def official_rust_source(version, output, include_compiler_rt):
    files = {}
    terms = []
    filenames = [f'rust-src-{version}.tar.xz']
    if include_compiler_rt:
        filenames.append(f'rustc-{version}-src.tar.xz')
    compiler_rt_found = False
    for filename in filenames:
        url = 'https://static.rust-lang.org/dist/' + filename
        checksum = download(url + '.sha256', output / (filename + '.sha256'))
        expected = checksum.read_text().split()[0]
        if not re.fullmatch('[0-9a-f]{64}', expected):
            raise ValueError('Malformed official Rust source checksum')
        archive = download(url, output / filename, expected)
        files[filename] = digest(archive)
        with tarfile.open(archive) as source:
            for member in source:
                if '/compiler-rt/lib/builtins/' in member.name and member.name.endswith('.c'):
                    compiler_rt_found = True
                basename = Path(member.name).name.lower()
                if member.isfile() and basename.startswith(('license', 'copying', 'copyright', 'notice')):
                    path = output / f'rust-terms-{len(terms)}-{Path(member.name).name}'
                    path.write_bytes(source.extractfile(member).read())
                    terms.append(path.name)
    if not terms:
        raise ValueError('Official Rust source archives contained no licence evidence')
    if include_compiler_rt and not compiler_rt_found:
        raise ValueError('Official source archives did not retain the linked compiler-rt C sources')
    return files, terms


def recorded_inputs(record, directory):
    aliases = {}
    for item in record.get('input_files', []):
        original = Path(item['path'])
        source = original
        if not source.is_file() and item.get('retained'):
            source = (directory / item['retained']).resolve()
            if not source.is_relative_to(directory.resolve()):
                raise ValueError('Retained linker input escapes the proof directory')
        if not source.is_file() or digest(source) != item['sha256']:
            raise ValueError('A recorded linker input is missing or changed after linking')
        aliases[str(original).casefold()] = source.resolve()
    return aliases


def preserve_rustc_archive(original, source, target_root, output, index):
    if not LINK_RECORDER.rustc_temporary_archive(original, target_root):
        raise ValueError('Retained Rust archive was not a rustc temporary archive inside the target directory')
    data = source.read_bytes()
    list(archive_members(data))
    retained = output / f'rust-link-archive-{index}-{original.name}'
    retained.write_bytes(data)
    return retained


def generated_import_archive(path):
    """Accept only COFF import descriptors/short imports, never native function code."""
    members = list(archive_members(path.read_bytes()))
    if not members:
        raise ValueError('Generated import library has no object members')
    for _, payload in members:
        if (len(payload) >= 20 and payload[:4] == b'\0\0\xff\xff'
                and int.from_bytes(payload[4:6], 'little') == 0
                and int.from_bytes(payload[6:8], 'little') in (0x8664, 0xaa64)):
            length = int.from_bytes(payload[12:16], 'little')
            names = payload[20:20 + length].split(b'\0')
            if (length == 0 or len(payload) < 20 + length or len(names) < 3 or not names[0]
                    or not names[1].lower().endswith(b'.dll')):
                raise ValueError('Malformed generated short import object')
            continue
        if len(payload) < 20 or int.from_bytes(payload[:2], 'little') not in (0x8664, 0xaa64):
            raise ValueError('Temporary native input is not a generated Windows import library')
        sections = int.from_bytes(payload[2:4], 'little')
        offset = 20 + int.from_bytes(payload[16:18], 'little')
        if not sections or offset + sections * 40 > len(payload):
            raise ValueError('Invalid generated import-library section table')
        for index in range(sections):
            header = payload[offset + index * 40:offset + (index + 1) * 40]
            name = header[:8].rstrip(b'\0')
            if (not name.startswith((b'.idata', b'.debug', b'.drectve', b'.rdata'))
                    or int.from_bytes(header[36:40], 'little') & 0x20):
                raise ValueError('Temporary native archive contains code beyond import descriptors')


def linked_inputs(record, trace, directory=None):
    """Resolve explicit archives and /VERBOSE:LIB Loaded records to actual files."""
    arguments = record.get('expanded_arguments')
    if not isinstance(arguments, list) or not arguments:
        raise ValueError('Final link record must retain expanded_arguments')
    search = [Path(arg.split(':', 1)[1].strip('"')) for arg in arguments
              if isinstance(arg, str) and arg.lower().startswith('/libpath:')]
    search += [Path(path) for path in os.environ.get('LIB', '').split(';') if path]
    selected = {}
    located = {}
    aliases = recorded_inputs(record, directory) if directory is not None else {}

    def add(path):
        path = aliases.get(str(path).casefold(), path)
        path = path.resolve()
        if not path.is_file():
            raise ValueError(f'Linked input is missing: {path}')
        selected[str(path).casefold()] = path

    for argument in arguments:
        token = str(argument).strip('"')
        if token.lower().endswith(('.lib', '.rlib')) and not token.startswith('/'):
            path = Path(token)
            if path.is_file() or str(path).casefold() in aliases:
                add(path)
            else:
                matches = [directory / token for directory in search if (directory / token).is_file()]
                if matches:
                    add(matches[0])
        elif Path(token).is_absolute() and Path(token).suffix.lower() in ('.lib', '.rlib'):
            add(Path(token))
    for line in trace.splitlines():
        paths = re.findall(r'(?:[A-Za-z]:[\\/]|/)[^"\r\n]+?\.(?:rlib|lib)\b', line, re.I)
        for token in paths:
            path = Path(token.strip())
            if path.is_file() or str(path).casefold() in aliases:
                located[ntpath.basename(token).casefold()] = aliases.get(str(path).casefold(), path)
        match = re.search(r'\bLoaded\s+(.+?\.(?:rlib|lib))(?=\(|\s|$)', line, re.I)
        if match:
            token = match[1].strip('" ')
            path = Path(token)
            path = aliases.get(str(path).casefold(), path)
            if not path.is_file():
                path = located.get(ntpath.basename(token).casefold())
            if path is None:
                raise ValueError(f'Cannot resolve a Loaded library from the linker trace: {token}')
            add(path)
    if not selected:
        raise ValueError('The final linker evidence identifies no native or Rust archives')
    return list(selected.values())


def relative(path, root):
    try:
        return path.resolve().relative_to(root.resolve()).as_posix()
    except ValueError:
        return None


def sanitise(text, roots):
    for label, path in sorted(roots.items(), key=lambda item: len(str(item[1])), reverse=True):
        for spelling in (str(path), str(path).replace('\\', '/'), str(path).replace('/', '\\')):
            text = re.sub(re.escape(spelling), lambda _: '$' + label, text, flags=re.I)
    # Vendor object members can retain the vendor's private build-machine paths.
    return re.sub(r'[A-Za-z]:[\\/][^\s"()<>]+', lambda match: '$VENDOR_BUILD/' + ntpath.basename(match[0]), text)


def sanitise_record(value, roots):
    if isinstance(value, str):
        return sanitise(value, roots)
    if isinstance(value, list):
        return [sanitise_record(item, roots) for item in value]
    if isinstance(value, dict):
        return {key: sanitise_record(item, roots) for key, item in value.items()}
    return value


def webview_sdk(package, archive_path, architecture, output, wrappers):
    """Discover upstream's pinned SDK, then compare official loader bytes."""
    with tarfile.open(archive_path) as archive:
        vcs = next(member for member in archive if member.name.endswith('/.cargo_vcs_info.json'))
        revision = json.loads(archive.extractfile(vcs).read())['git']['sha1']
    if not re.fullmatch('[0-9a-f]{40}', revision):
        raise ValueError('WebView2 wrapper has no exact upstream revision')
    generator_url = f'https://raw.githubusercontent.com/wravery/webview2-rs/{revision}/crates/update-bindings/src/main.rs'
    generator = download(generator_url, output / 'webview2-upstream-generator.rs')
    match = re.search(r'WEBVIEW2_VERSION:\s*&str\s*=\s*"([0-9.]+)"', generator.read_text())
    if not match:
        raise ValueError('Cannot establish the locked WebView2 wrapper SDK version')
    version = match[1]
    if version != '1.0.3800.47':
        raise ValueError('The WebView2 SDK terms need review for this newly selected SDK version')
    url = f'https://api.nuget.org/v3-flatcontainer/microsoft.web.webview2/{version}/microsoft.web.webview2.{version}.nupkg'
    sdk = download(url, output / f'microsoft.web.webview2.{version}.nupkg')
    with zipfile.ZipFile(sdk) as archive:
        lib_name = f'build/native/{architecture}/WebView2LoaderStatic.lib'
        payload = archive.read(lib_name)
        with tarfile.open(archive_path) as crate:
            member = next(member for member in crate if member.name.endswith('/' + architecture + '/WebView2LoaderStatic.lib'))
            wrapped = crate.extractfile(member).read()
        if payload != wrapped:
            raise ValueError('Official WebView2 SDK loader differs from locked wrapper payload')
        verify_bundled_loader(payload, wrappers)
        terms = []
        redistribution = []
        for name in archive.namelist():
            base = Path(name).name.lower()
            if (base.startswith(('license', 'licence', 'redist', 'notice', 'thirdpartynotice'))
                    or name.lower().endswith('.nuspec')) and not name.endswith('/'):
                path = output / ('webview2-' + str(len(terms) + len(redistribution)) + '-' + Path(name).name)
                path.write_bytes(archive.read(name))
                if base.startswith(('redist', 'notice')) or name.lower().endswith('.nuspec'):
                    redistribution.append(path.name)
                else:
                    terms.append(path.name)
        if not terms or not redistribution:
            raise ValueError('Matching WebView2 SDK lacks licence terms or redistribution evidence')
    # Keep only permitted loader payload hashes and terms, not an indiscriminate
    # redistribution of every SDK binary in the downloaded NuGet package.
    proof = output / 'webview2-sdk-provenance.json'
    proof.write_text(json.dumps({'url': url, 'version': version, 'nupkg_sha256': digest(sdk),
                                 'loader_sha256': hashlib.sha256(payload).hexdigest(),
                                 'locked_crate': f"{package['name']}@{package['version']}",
                                 'crate_sha256': package['checksum'], 'upstream_revision': revision}, indent=2) + '\n')
    sdk.unlink()
    return version, terms, redistribution + [proof.name], url, hashlib.sha256(payload).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('binary', 'link-map', 'link-trace', 'link-record'):
        parser.add_argument('--' + name, type=Path)
    for name in ('toolchain-record', 'output'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--preflight', action='store_true', help='Validate/download provenance before the expensive native build')
    parser.add_argument('--target', choices=('x86_64-pc-windows-msvc', 'aarch64-pc-windows-msvc'))
    parser.add_argument('--windows-sdk-terms', type=Path, help='Actual installed SDK licence if outside the standard Licenses directory')
    args = parser.parse_args()
    if os.name != 'nt':
        parser.error('Run this producer on the native Windows runner')
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    if (output / 'runtime-manifest.json').exists():
        parser.error('Refusing to overwrite an existing completed runtime manifest')
    toolchain = json.loads(args.toolchain_record.read_text(encoding='utf-8-sig'))
    args.target = args.target or toolchain.get('target')
    if args.target not in ('x86_64-pc-windows-msvc', 'aarch64-pc-windows-msvc'):
        raise ValueError('Unsupported Windows native target in toolchain record')
    verbose = toolchain.get('rustc_verbose', '')
    release_match = re.search(r'^release:\s*([0-9]+\.[0-9]+\.[0-9]+)$', verbose, re.M)
    commit_match = re.search(r'^commit-hash:\s*([0-9a-f]{40})$', verbose, re.M)
    if not release_match or not commit_match:
        raise ValueError('Toolchain record must identify the exact stable Rust compiler')
    if toolchain.get('target') != args.target:
        raise ValueError('The native target differs from the recorded toolchain')
    profile = visual_studio_profile(toolchain.get('vsproductversion'))
    if toolchain.get('vsisprerelease') is True:
        raise ValueError('Preview toolchain redistribution terms need explicit review before a public release')
    vc = Path(toolchain['VCToolsInstallDir'])
    sdk_root = Path(toolchain['WindowsSdkDir'])
    if not vc.is_absolute() or not sdk_root.is_absolute() or not vc.is_dir() or not sdk_root.is_dir():
        raise ValueError('Recorded native toolchain and SDK directories must actually exist')
    rust_root = Path(subprocess.check_output(['rustc', '--print', 'sysroot'], text=True).strip())
    sdk_version = str(toolchain['WindowsSDKVersion']).strip('/\\ ')
    term_names, redistribution_names, sdk_term_names = prepare_vendor_terms(output, sdk_root, sdk_version, args.windows_sdk_terms, profile)
    if args.preflight:
        archives = list((rust_root / 'lib/rustlib' / args.target / 'lib').glob('libcompiler_builtins-*.rlib'))
        if len(archives) != 1:
            raise ValueError('Preflight needs the actual installed target compiler-builtins archive')
        include_compiler_rt = any(not name.endswith(('.rcgu.o', '.rmeta', '.rmeta-link'))
                                  for name, _ in archive_members(archives[0].read_bytes()))
        source_archives, source_terms = official_rust_source(release_match[1], output, include_compiler_rt)
        preflight = {'rustc_commit': commit_match[1], 'target': args.target, 'visual_studio_year': profile['year'],
                     'source_archives': source_archives,
                     'source_terms': {name: digest(output / name) for name in source_terms},
                     'compiler_rt_sources_included': include_compiler_rt}
        (output / 'preflight.json').write_text(json.dumps(preflight, indent=2) + '\n')
        print('Windows native evidence preflight complete; final binary/link binding remains required')
        return
    if any(value is None for value in (args.binary, args.link_map, args.link_trace, args.link_record)):
        parser.error('Full evidence production requires --binary, --link-map, --link-trace and --link-record')
    if not COLLECTOR.pe_binary(args.binary):
        raise ValueError('The packaged executable must be a Windows PE binary')
    with args.binary.open('rb') as binary:
        binary.seek(60)
        offset = int.from_bytes(binary.read(4), 'little')
        binary.seek(offset + 4)
        machine = int.from_bytes(binary.read(2), 'little')
    if machine != (0xaa64 if args.target.startswith('aarch64') else 0x8664):
        raise ValueError('Packaged PE architecture differs from the native target record')
    record = json.loads(args.link_record.read_text(encoding='utf-8-sig'))
    sha = digest(args.binary)
    for key, actual in (('binary_sha256', sha), ('link_map_sha256', digest(args.link_map)),
                        ('link_trace_sha256', digest(args.link_trace))):
        if record.get(key) != actual:
            raise ValueError(f'Final link record does not match {key}')
    if record.get('linked_binary_sha256', sha) != sha or record.get('rustc_commit') != commit_match[1]:
        raise ValueError('Final link record compiler/executable differs from the packaged build')
    cargo_home = Path(os.environ.get('CARGO_HOME', Path.home() / '.cargo'))
    target_root = Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target')).resolve()
    roots = {'WORKSPACE': ROOT, 'CARGO_HOME': cargo_home, 'RUST_SYSROOT': rust_root,
             'VC_TOOLS': vc, 'VS_ROOT': Path(toolchain['vsinstallation']),
             'WINDOWS_SDK': sdk_root, 'RUNNER_HOME': Path.home(), 'TARGET_DIR': target_root,
             'TASK_TMP': Path(os.environ.get('TMPDIR') or os.environ.get('TEMP') or args.output.parent)}
    trace = args.link_trace.read_text(errors='replace')
    inputs = linked_inputs(record, trace, args.link_record.parent)
    groups = {'msvc-crt': [], 'windows-sdk': [], 'webview2-loader': []}
    webview_inputs = []
    webview_wrappers = []
    crate_inputs = []
    include_compiler_rt = False
    generated_inputs = []
    retained_originals = {}
    for entry in record.get('input_files', []):
        if entry.get('retained'):
            retained_originals[str((args.link_record.parent / entry['retained']).resolve()).casefold()] = entry
            retained_originals[str(Path(entry['path']).resolve()).casefold()] = entry
    for path in inputs:
        snapshot = retained_originals.get(str(path.resolve()).casefold())
        if snapshot is not None:
            original = Path(snapshot['path'])
            if snapshot.get('kind') == 'rustc-temporary-rlib':
                retained = preserve_rustc_archive(original, path, target_root, output, len(generated_inputs))
                generated_inputs.append({'name': retained.name, 'sha256': digest(retained), 'kind': snapshot['kind']})
                if original.name.startswith('libwebview2_com_sys-'):
                    webview_wrappers.append(retained)
                if original.name.startswith('libcompiler_builtins-'):
                    include_compiler_rt = any(not name.endswith(('.rcgu.o', '.rmeta', '.rmeta-link'))
                                              for name, _ in archive_members(retained.read_bytes()))
                continue
            if snapshot.get('kind') not in (None, 'rustc-raw-dylib', 'temporary-native-library'):
                raise ValueError('Unknown retained native linker input kind')
            if 'raw-dylibs' not in str(original).replace('\\', '/').casefold().split('/'):
                raise ValueError('Unreviewed temporary native input; only recorded rustc raw-dylib import stubs are supported')
            generated_import_archive(path)
            retained = output / ('rust-generated-import-' + str(len(generated_inputs)) + '.lib')
            shutil.copyfile(path, retained)
            generated_inputs.append({'name': retained.name, 'sha256': digest(retained)})
            continue
        if path.suffix.lower() == '.rlib':
            if relative(path, rust_root) is None and relative(path, target_root) is None:
                raise ValueError(f'Unclassified Rust archive: {path}')
            if path.name.startswith('libwebview2_com_sys-'):
                webview_wrappers.append(path)
            if path.name.startswith('libcompiler_builtins-'):
                include_compiler_rt = any(not name.endswith(('.rcgu.o', '.rmeta', '.rmeta-link'))
                                          for name, _ in archive_members(path.read_bytes()))
            continue
        if path.name.casefold() == 'webview2loaderstatic.lib':
            family, suffix = 'webview2-loader', path.name
            webview_inputs.append(path)
        elif (suffix := relative(path, vc)) is not None:
            family = 'msvc-crt'
        elif (suffix := relative(path, sdk_root)) is not None:
            family = 'windows-sdk'
        elif (suffix := relative(path, cargo_home / 'registry/src')) is not None:
            crate_inputs.append((path, Path(suffix).parts[1], Path(*Path(suffix).parts[2:]).as_posix()))
            continue
        elif relative(path, target_root) is not None:
            # Cargo build outputs are bound to authenticated native crate archives below.
            continue
        else:
            raise ValueError(f'Unclassified native linker input: {path}')
        groups[family].append({'name': family + '/' + suffix, 'sha256': digest(path)})
    if not groups['msvc-crt']:
        raise ValueError('The linker evidence does not identify the Microsoft CRT startup inputs')
    for name, source in (('link-map.txt', args.link_map), ('link-trace.txt', args.link_trace)):
        (output / name).write_text(sanitise(source.read_text(errors='replace'), roots))
    (output / 'build-record.json').write_text(json.dumps(sanitise_record({'link': record, 'toolchain': toolchain}, roots), indent=2) + '\n')
    imports = subprocess.check_output(['dumpbin', '/dependents', str(args.binary)], text=True, errors='replace')
    if re.search(r'\b(?:VCRUNTIME\d+[^\s.]*|MSVCP\d+[^\s.]*|CONCRT\d+[^\s.]*|ucrtbased)\.dll\b', imports, re.I):
        raise ValueError('Windows release depends on an unbundled VC/debug runtime; build the target with +crt-static')
    (output / 'pe-imports.txt').write_text(sanitise(imports, roots))
    build = output / 'build.txt'
    build.write_text(f'Compiler: Rust {release_match[1]} ({commit_match[1]}).\nTarget: {args.target}.\n'
                     'Use the exact release revision and Cargo.lock with Visual Studio ' + profile['year'] + ' and the retained toolchain versions.\n'
                     'Set RUSTFLAGS=-C target-feature=+crt-static, then run cargo build --locked --release -p rustpy --features native-web-preview --target ' + args.target + '.\n'
                     'Rust standard library/runtime sources and native crate archives are retained alongside this file.\n')
    replace = output / 'replace.txt'
    replace.write_text('Rebuild RustPy from its release source with the recorded compiler/SDK versions.\n'
                       'Patch native crates using Cargo [patch.crates-io] paths, then rebuild the executable and rerun packaging.\n'
                       'Microsoft redistributable object code is governed by its retained vendor terms; do not redistribute its standalone .lib archives.\n'
                       'To replace compiler runtime code, build the matching Rust source distribution using its bootstrap instructions.\n')
    components = []
    lock = tomllib.loads((ROOT / 'Cargo.lock').read_text())['package']
    registry = list((cargo_home / 'registry/cache').glob('*'))
    native_packages = []
    for package in lock:
        if not package.get('source', '').startswith('registry+'):
            continue
        name = f"{package['name']}-{package['version']}"
        paths = [directory / (name + '.crate') for directory in registry if (directory / (name + '.crate')).is_file()]
        if len(paths) != 1 or digest(paths[0]) != package['checksum']:
            raise ValueError(f'Missing authenticated Cargo archive: {name}; run cargo fetch --locked')
        with tarfile.open(paths[0]) as archive:
            member = next(member for member in archive if member.name == name + '/Cargo.toml')
            manifest = tomllib.loads(archive.extractfile(member).read().decode())
            prebuilt = any(member.isfile() and member.name.lower().endswith(('.lib', '.a', '.o', '.dll')) for member in archive.getmembers())
            for path, package_name, archive_rel in crate_inputs:
                if package_name == name:
                    member = archive.getmember(name + '/' + archive_rel)
                    if hashlib.sha256(archive.extractfile(member).read()).hexdigest() != digest(path):
                        raise ValueError('Linked Cargo native payload differs from its authenticated crate archive')
        build_deps = set(manifest.get('build-dependencies', {}))
        if (package['name'] in COLLECTOR.NATIVE or manifest['package'].get('links') or prebuilt
                or build_deps & {'cc', 'cmake', 'autotools', 'bindgen', 'vcpkg', 'pkg-config'}):
            native_packages.append((package, paths[0]))
        if package['name'] == 'webview2-com-sys':
            webview_package, webview_archive = package, paths[0]
    if webview_inputs or webview_wrappers:
        arch = 'arm64' if args.target.startswith('aarch64') else 'x64'
        version, terms, redist, url, loader_sha = webview_sdk(webview_package, webview_archive, arch, output, webview_wrappers)
        if any(digest(path) != loader_sha for path in webview_inputs):
            raise ValueError('Actual linked WebView2 static loader differs from the official matching SDK')
        groups['webview2-loader'] = [{'name': 'webview2-loader/WebView2LoaderStatic.lib', 'sha256': loader_sha,
                                      'bundled_into': [{'name': path.name, 'sha256': digest(path)} for path in webview_wrappers]}]
    for family, entries in groups.items():
        if not entries:
            continue
        inventory = output / (family + '-inputs.json')
        inventory.write_text(json.dumps({'binary_sha256': sha, 'runtime_family': family, 'inputs': entries}, indent=2) + '\n')
        if family == 'webview2-loader':
            component_version, terms_for_family, redist_for_family, urls = version, terms, redist, [url]
        else:
            field = 'VCToolsVersion' if family == 'msvc-crt' else 'WindowsSDKVersion'
            component_version = str(toolchain[field]).strip('/\\ ')
            terms_for_family = sdk_term_names + term_names if family == 'windows-sdk' else term_names
            redist_for_family = redistribution_names
            urls = [profile['tools_terms'], profile['product_terms'], profile['runtime_terms'], profile['redist'], VS_GUIDANCE, CRT_DESCRIPTION]
        linkage = ['link-map.txt', 'link-trace.txt', 'build-record.json', 'pe-imports.txt', inventory.name]
        components.append({'name': COLLECTOR.MICROSOFT_RUNTIME_NAMES[family], 'version': component_version,
                           'evidence_kind': 'microsoft-redistributable', 'vendor': 'Microsoft', 'runtime_family': family,
                           'review_basis': 'Retained vendor terms, redistribution/deployment guidance and actual linked object inventory; producer review, not legal certification',
                           'provenance_urls': urls, 'license_files': terms_for_family,
                           'vendor_terms': {name: digest(output / name) for name in terms_for_family},
                           'redistribution_evidence': {name: digest(output / name) for name in redist_for_family},
                           'linkage_evidence': {name: digest(output / name) for name in linkage}, 'input_inventory': inventory.name,
                           'linkage_records': {'map': 'link-map.txt', 'trace': 'link-trace.txt', 'build_record': 'build-record.json'},
                           'build_instructions': build.name, 'replacement_instructions': replace.name})
    preflight_path = output / 'preflight.json'
    preflight = json.loads(preflight_path.read_text()) if preflight_path.is_file() else None
    if (preflight and preflight.get('rustc_commit') == commit_match[1] and preflight.get('target') == args.target
            and (not include_compiler_rt or preflight.get('compiler_rt_sources_included'))
            and all((output / name).is_file() and digest(output / name) == sha256
                    for name, sha256 in {**preflight['source_archives'], **preflight['source_terms']}.items())):
        rust_sources, rust_terms = preflight['source_archives'], list(preflight['source_terms'])
    else:
        rust_sources, rust_terms = official_rust_source(release_match[1], output, include_compiler_rt)
    provenance = {entry['name']: entry['sha256'] for entry in generated_inputs}
    if generated_inputs:
        (output / 'rust-generated-inputs.json').write_text(json.dumps({'binary_sha256': sha, 'inputs': generated_inputs}, indent=2) + '\n')
        provenance['rust-generated-inputs.json'] = digest(output / 'rust-generated-inputs.json')
    for name in rust_sources:
        checksum_name = name + '.sha256'
        provenance[checksum_name] = digest(output / checksum_name)
    components.append({'name': 'Rust standard library and compiler runtimes', 'version': release_match[1],
                       'license_files': rust_terms, 'source_archives': rust_sources,
                       'provenance_files': provenance,
                       'build_instructions': build.name, 'replacement_instructions': replace.name})
    notice_manifest = CARGO_NOTICES.check()
    notice_overrides = json.loads((ROOT / 'packaging/cargo-license-overrides.json').read_text())['packages']
    for package, archive in native_packages:
        retained = output / archive.name
        shutil.copyfile(archive, retained)
        notice = output / (package['name'] + '-' + package['version'] + '-native-notices.txt')
        notice.write_text(native_package_notice(package, archive, notice_manifest, notice_overrides))
        components.append({'name': package['name'], 'version': package['version'], 'license_files': [notice.name],
                           'source_archives': {retained.name: digest(retained)},
                           'build_instructions': build.name, 'replacement_instructions': replace.name})
    manifest = output / 'runtime-manifest.json'
    manifest.write_text(json.dumps({'binary_sha256': sha, 'system_static_components': components, 'components': []}, indent=2) + '\n')
    print(manifest)


if __name__ == '__main__':
    main()
