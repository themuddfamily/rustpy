#!/usr/bin/env python3
"""Prepare binary-bound evidence for an Ubuntu Linux archive without bundled libraries.

The caller retains the original linked executable, linker map and --trace output,
then records their SHA256 values and the full rustc commit in --link-record JSON:
linked_binary_sha256, link_map_sha256, link_trace_sha256, rustc_commit.
The final --binary may have debug information stripped; allocated ELF sections
must still match the recorded original. This is a producer evidence inventory,
not a licence-compatibility decision or a replacement for native acceptance.
"""
import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import tarfile
import urllib.parse
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
SHA256 = re.compile(r'[0-9a-f]{64}')
INPUT = re.compile(r'''/[^\s():"']+\.(?:rlib|a|o|so(?:\.[0-9]+)*)(?=[\s():"']|$)''')
RELATIVE_INPUT = re.compile(r'''(?<![A-Za-z0-9_./+-])(?:[A-Za-z0-9_.+-]+/)*[A-Za-z0-9_.+-]+\.(?:rlib|a|o|so(?:\.[0-9]+)*)(?=[\s():"']|$)''')
RETAINED_NATIVE = {'libgit2-sys', 'libssh2-sys', 'libz-sys', 'libsqlite3-sys', 'openssl-src'}
AUDITED_NATIVE = RETAINED_NATIVE | {'ring', 'blake3'}
LAUNCHPAD = 'https://api.launchpad.net/1.0/ubuntu/+archive/primary'


def digest(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def command(arguments, cwd=None):
    return subprocess.check_output(arguments, cwd=cwd, text=True).strip()


def allocated_sections(path):
    """Compare runtime code/data across stripping without trusting debug sections."""
    data = Path(path).read_bytes()
    if data[:4] != b'\x7fELF' or data[4:6] not in (b'\x02\x01', b'\x02\x02', b'\x01\x01', b'\x01\x02'):
        raise ValueError('Expected a supported ELF executable')
    wide = data[4] == 2
    endian = '<' if data[5] == 1 else '>'
    header = struct.unpack_from(endian + ('HHIQQQIHHHHHH' if wide else 'HHIIIIIHHHHHH'), data, 16)
    offset, entry_size, count, names_index = header[5], header[10], header[11], header[12]
    fmt = endian + ('IIQQQQIIQQ' if wide else 'IIIIIIIIII')
    if not count or entry_size != struct.calcsize(fmt) or names_index >= count:
        raise ValueError('Unsupported ELF section table')
    if offset + count * entry_size > len(data):
        raise ValueError('Truncated ELF section table')
    sections = [struct.unpack_from(fmt, data, offset + index * entry_size) for index in range(count)]
    strings = sections[names_index]
    names = data[strings[4]:strings[4] + strings[5]]
    retained = []
    for section in sections:
        name_offset, kind, flags, address, start, size, _, _, alignment, entry = section
        if not flags & 2:  # SHF_ALLOC: bytes that survive into the loaded program.
            continue
        if name_offset >= len(names):
            raise ValueError('Invalid ELF section name')
        end = names.find(b'\0', name_offset)
        if end < 0:
            raise ValueError('Unterminated ELF section name')
        name = names[name_offset:end].decode('utf-8')
        if kind == 8:  # SHT_NOBITS has size but no stored bytes.
            checksum = None
        else:
            if start + size > len(data):
                raise ValueError('Truncated allocated ELF section')
            checksum = hashlib.sha256(data[start:start + size]).hexdigest()
        retained.append((name, kind, flags, address, size, alignment, entry, checksum))
    if not retained:
        raise ValueError('ELF has no allocated sections')
    return (data[4], data[5], header[0], header[1]), sorted(retained)


def validate_link_record(record, linked_binary, binary, link_map, link_trace, rustc_commit):
    for key, path in (('linked_binary_sha256', linked_binary),
                      ('link_map_sha256', link_map), ('link_trace_sha256', link_trace)):
        expected = record.get(key)
        if not isinstance(expected, str) or not SHA256.fullmatch(expected) or digest(path) != expected:
            raise ValueError(f'Link provenance mismatch: {key}')
    if record.get('rustc_commit') != rustc_commit:
        raise ValueError('Link record does not match the selected Rust toolchain')
    if allocated_sections(linked_binary) != allocated_sections(binary):
        raise ValueError('Packaged ELF code/data differ from the recorded linked executable')


def link_inputs(text, base=None):
    base = Path.cwd() if base is None else Path(base)
    paths = {Path(match[0]) for match in INPUT.finditer(text)}
    for line in text.splitlines():
        for match in RELATIVE_INPUT.finditer(line):
            prefix = line[:match.start()]
            if prefix.count('(') > prefix.count(')'):
                continue  # Archive members are attributed through their containing archive.
            paths.add((base / match[0]).resolve())
    paths = sorted(paths)
    if not paths:
        raise ValueError('Link evidence contains no object/archive/library paths')
    return paths


def dsc_checksums(text):
    checksums = {}
    collecting = False
    for line in text.splitlines():
        if line == 'Checksums-Sha256:':
            collecting = True
            continue
        if collecting and line.startswith((' ', '\t')):
            fields = line.split()
            if len(fields) != 3 or not SHA256.fullmatch(fields[0]) or not fields[1].isdigit():
                raise ValueError('Invalid source .dsc SHA256 entry')
            sha, size, name = fields
            if Path(name).name != name or name in ('.', '..') or name in checksums:
                raise ValueError('Unsafe or duplicate source archive name')
            checksums[name] = (sha, int(size))
        elif collecting:
            break
    if not checksums:
        raise ValueError('Source .dsc has no SHA256 inventory')
    return checksums


class Downloads:
    def __init__(self, opener=None):
        self.opener = opener or urllib.request.urlopen

    def json(self, url):
        with self.opener(url, timeout=60) as response:
            if hasattr(response, 'geturl') and urllib.parse.urlparse(response.geturl()).scheme != 'https':
                raise ValueError('Source metadata redirected away from HTTPS')
            data = response.read(10 * 1024 * 1024 + 1)
            if len(data) > 10 * 1024 * 1024:
                raise ValueError('Source metadata exceeds 10 MiB')
            return json.loads(data)

    def file(self, url, destination, expected=None, expected_size=None):
        if urllib.parse.urlparse(url).scheme != 'https':
            raise ValueError('Source downloads require HTTPS')
        destination = Path(destination)
        destination.parent.mkdir(parents=True, exist_ok=True)
        if destination.exists():
            if expected and digest(destination) == expected and (expected_size is None or destination.stat().st_size == expected_size):
                return destination
            raise ValueError(f'Refusing unverified existing source file: {destination.name}')
        partial = destination.with_name(destination.name + '.part')
        try:
            with self.opener(url, timeout=120) as response, partial.open('xb') as output:
                if hasattr(response, 'geturl') and urllib.parse.urlparse(response.geturl()).scheme != 'https':
                    raise ValueError('Source archive redirected away from HTTPS')
                size = 0
                checksum = hashlib.sha256()
                while block := response.read(1024 * 1024):
                    size += len(block)
                    if size > 1024 ** 3:
                        raise ValueError('Source download exceeds 1 GiB')
                    checksum.update(block)
                    output.write(block)
            if expected and checksum.hexdigest() != expected:
                raise ValueError(f'Source SHA256 mismatch: {destination.name}')
            if expected_size is not None and size != expected_size:
                raise ValueError(f'Source size mismatch: {destination.name}')
            partial.replace(destination)
        finally:
            partial.unlink(missing_ok=True)
        return destination


def cargo_packages(repo):
    data = json.loads(command(['cargo', 'metadata', '--locked', '--offline', '--all-features', '--format-version', '1'], cwd=repo))
    return data['packages']


def crate_owner(path, packages):
    name = path.name
    match = re.fullmatch(r'lib([A-Za-z0-9_]+?)(?:-[0-9a-f]+)?\.rlib', name)
    if not match:
        raise ValueError(f'Unrecognised Rust archive: {name}')
    crate_name = match[1]
    candidates = [package for package in packages if any(target['name'] == crate_name for target in package['targets'])]
    if len(candidates) > 1:
        dependencies = path.with_name(name[3:]).with_suffix('.d')
        if not dependencies.is_file():
            raise ValueError(f'Ambiguous Cargo archive without dependency provenance: {name}')
        text = dependencies.read_text()
        candidates = [package for package in candidates if str(Path(package['manifest_path']).parent) + '/' in text]
    if len(candidates) != 1:
        raise ValueError(f'Cannot attribute Cargo archive to one package: {name}')
    return candidates[0]


def native_archive(path):
    members = command(['ar', 't', str(path)]).splitlines()
    return any(name.endswith(('.o', '.obj')) and not name.endswith('.rcgu.o') for name in members)


def native_build_owner(path, target, packages):
    relative = path.relative_to(target)
    parts = relative.parts
    for index, part in enumerate(parts[:-1]):
        if part != 'build':
            continue
        directory = parts[index + 1]
        matches = [package for package in packages if re.fullmatch(re.escape(package['name']) + '-[0-9a-f]+', directory)]
        if len(matches) == 1:
            return matches[0]
    raise ValueError(f'Cannot attribute Cargo native object/archive: {relative}')


def system_owner(path):
    line = command(['dpkg-query', '-S', str(path)]).splitlines()[0]
    binary_package = line.split(': ', 1)[0]
    source, version = command(['dpkg-query', '-W', '-f=${source:Package}\t${source:Version}', binary_package]).split('\t')
    if source != 'glibc' and not re.fullmatch(r'gcc-[0-9]+', source):
        raise ValueError(f'Unreviewed system static component: {binary_package} ({source})')
    copyright_file = Path('/usr/share/doc') / binary_package.split(':')[0] / 'copyright'
    if not copyright_file.is_file():
        raise ValueError(f'No installed package copyright file: {binary_package}')
    return source, version, copyright_file


def inventory(paths, repo, target, sysroot, packages):
    system = {}
    native = {}
    rust = False
    project = False
    dynamic = []
    for supplied in paths:
        path = supplied.resolve()
        if supplied.name.endswith('.so') or re.search(r'\.so\.[0-9.]+$', supplied.name):
            dynamic.append(str(supplied))
            continue
        if path.is_relative_to(sysroot):
            if path.suffix != '.rlib' and path.name not in ('libunwind.a', 'libcompiler_builtins.a', 'rsbegin.o', 'rsend.o'):
                raise ValueError(f'Unreviewed toolchain native input: {path.name}')
            rust = True
            continue
        if path.is_relative_to(target):
            if supplied.suffix == '.rlib':
                package = crate_owner(path, packages)
                project |= package['name'].startswith('rustpy') and package.get('source') is None
                if native_archive(path):
                    if package['name'] not in AUDITED_NATIVE:
                        raise ValueError(f'Unreviewed Cargo native component: {package["name"]}')
                    native[(package['name'], package['version'])] = package
            elif supplied.name.endswith('.rcgu.o'):
                # Rust codegen outputs are paired with the authenticated link record.
                if not supplied.name.startswith(('rustpy.', 'rustpy-')):
                    raise ValueError(f'Unattributed standalone Rust object: {supplied.name}')
                project = True
            else:
                package = native_build_owner(path, target, packages)
                if package['name'] not in AUDITED_NATIVE:
                    raise ValueError(f'Unreviewed Cargo native component: {package["name"]}')
                native[(package['name'], package['version'])] = package
            continue
        if supplied.name == 'symbols.o' and supplied.parent.name.startswith('rustc'):
            rust = True
            continue
        source, version, copyright_file = system_owner(path)
        entry = system.setdefault((source, version), {'copyright': set(), 'inputs': []})
        entry['copyright'].add(copyright_file)
        entry['inputs'].append(str(supplied))
    if not rust or not project:
        raise ValueError('Link evidence must identify both the Rust toolchain and RustPy objects')
    return system, native, dynamic


def retained(path, output):
    return path.relative_to(output).as_posix()


def provenance_archive(output, link_map, link_trace, record, repo, target, sysroot):
    """Retain link evidence without publishing producer-specific directory names."""
    replacements = [(str(target), '$CARGO_TARGET_DIR'), (str(sysroot), '$RUST_SYSROOT'),
                    (str(repo), '$WORKSPACE'), (str(Path(os.environ.get('TMPDIR', '/tmp')).resolve()), '$TASK_TMP'),
                    (str(Path.home()), '$USER_HOME')]
    replacements.sort(key=lambda pair: len(pair[0]), reverse=True)
    def clean(text):
        for old, new in replacements:
            text = text.replace(old, new)
        return text
    files = {'link.map': clean(link_map.read_text()).encode(),
             'link.trace': clean(link_trace.read_text()).encode(),
             'link-record.json': clean(json.dumps(record, indent=2)).encode()}
    path = output / 'sources/link-provenance.tar.gz'
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open('wb') as destination, gzip.GzipFile(fileobj=destination, mode='wb', mtime=0) as compressed:
        with tarfile.open(fileobj=compressed, mode='w') as archive:
            for name, data in sorted(files.items()):
                info = tarfile.TarInfo(name)
                info.size = len(data)
                info.mode = 0o644
                archive.addfile(info, io.BytesIO(data))
    return path


def component(output, name, version, archives, licenses, build, replacement):
    ident = re.sub('[^A-Za-z0-9_.-]', '_', name + '-' + version)
    instructions = output / 'instructions'
    instructions.mkdir(exist_ok=True)
    build_path = instructions / (ident + '-build.txt')
    replace_path = instructions / (ident + '-replace.txt')
    build_path.write_text(build)
    replace_path.write_text(replacement)
    return {'name': name, 'version': version,
            'license_files': [retained(path, output) for path in licenses],
            'source_archives': {retained(path, output): digest(path) for path in archives},
            'build_instructions': retained(build_path, output),
            'replacement_instructions': retained(replace_path, output)}


def copy_licenses(paths, destination):
    destination.mkdir(parents=True, exist_ok=True)
    result = []
    for index, path in enumerate(sorted(paths)):
        target = destination / (str(index) + '-' + path.name)
        shutil.copyfile(path, target)
        result.append(target)
    if not result:
        raise ValueError('Component has no retained licence texts')
    return result


def ubuntu_source(source, version, output, downloads):
    parameters = urllib.parse.urlencode({'ws.op': 'getPublishedSources', 'source_name': source,
                                        'version': version, 'exact_match': 'true'})
    publications = downloads.json(LAUNCHPAD + '?' + parameters)['entries']
    publications = [entry for entry in publications if entry['source_package_name'] == source
                    and entry['source_package_version'] == version]
    if not publications:
        raise ValueError(f'Exact Ubuntu source publication is unavailable: {source} {version}')
    publication = sorted(publications, key=lambda entry: entry['status'] != 'Published')[0]
    if not publication['self_link'].startswith(LAUNCHPAD + '/+sourcepub/'):
        raise ValueError('Unexpected Ubuntu source publication URL')
    urls = downloads.json(publication['self_link'] + '?ws.op=sourceFileUrls')
    mapping = {urllib.parse.unquote(urllib.parse.urlparse(url).path.rsplit('/', 1)[-1]): url for url in urls}
    dsc_names = [name for name in mapping if name.endswith('.dsc')]
    if len(dsc_names) != 1:
        raise ValueError('Source publication must contain exactly one .dsc')
    destination = output / 'sources' / (source + '-' + version)
    dsc = downloads.file(mapping[dsc_names[0]], destination / dsc_names[0])
    result = [dsc]
    for name, (sha, size) in dsc_checksums(dsc.read_text()).items():
        if name not in mapping:
            raise ValueError(f'Exact source publication is missing {name}')
        result.append(downloads.file(mapping[name], destination / name, sha, size))
    return result


def rust_component(output, sysroot, rust_version, rust_commit, downloads):
    import tomllib
    path = sysroot / 'lib/rustlib/multirust-channel-manifest.toml'
    data = tomllib.loads(path.read_text())
    package = data['pkg']['rust-src']
    if not package['version'].startswith(rust_version + ' (' + rust_commit[:9]):
        raise ValueError('Installed channel manifest does not match the linked Rust release/commit')
    target = package['target']['*']
    url = target['xz_url']
    if urllib.parse.urlparse(url).hostname != 'static.rust-lang.org':
        raise ValueError('Rust source URL is not the official distribution host')
    archive = downloads.file(url, output / 'sources' / Path(urllib.parse.urlparse(url).path).name, target['xz_hash'])
    docs = sysroot / 'share/doc/rust'
    licenses = copy_licenses([docs / 'COPYRIGHT-library.html', *sorted((docs / 'licenses').glob('*.txt'))], output / 'licenses/rust')
    return component(output, 'Rust standard library and compiler builtins', rust_version + '-' + rust_commit,
                     [archive], licenses,
                     'Sources are the checksum-verified rust-src component for the exact linked rustc release.\n'
                     'The archive contains library/compiler-builtins and vendored library dependencies.\n'
                     'For a replacement, install this exact Rust release and its rust-src component, use its\n'
                     'library sources and Cargo.lock, and opt into rebuilding std with this compiler:\n'
                     'RUSTC_BOOTSTRAP=1 cargo -Z build-std=std,panic_unwind build --locked --release -p rustpy\n'
                     'Add the target and optional feature arguments recorded in the retained link provenance.\n'
                     'Compiler bootstrap instructions and source: https://github.com/rust-lang/rust/tree/' + rust_commit + '\n',
                     'Rebuild the Rust standard library/compiler builtins from the retained sources for the same\n'
                     'target and ABI, then rebuild and relink RustPy against those replacement libraries.\n'
                     'Use the retained link map/trace to identify the replaced runtime objects.\n')


def cargo_component(package, output, cargo_home, repo=ROOT):
    import tomllib
    locked = [entry for entry in tomllib.loads((repo / 'Cargo.lock').read_text())['package']
              if entry['name'] == package['name'] and entry['version'] == package['version']]
    if len(locked) != 1 or not locked[0].get('checksum'):
        raise ValueError(f'Native Cargo package lacks a single authenticated registry source: {package["name"]}')
    name = package['name'] + '-' + package['version']
    matches = list((cargo_home / 'registry/cache').glob('*/' + name + '.crate'))
    if len(matches) != 1 or digest(matches[0]) != locked[0]['checksum']:
        raise ValueError(f'Missing or unauthenticated Cargo native source archive: {name}')
    destination = output / 'sources' / (name + '.crate')
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(matches[0], destination)
    licenses = []
    with tarfile.open(destination) as archive:
        for member in archive.getmembers():
            if not member.isfile() or not Path(member.name).name.lower().startswith(('license', 'copying', 'copyright', 'notice')):
                continue
            path = output / 'licenses' / name / (str(len(licenses)) + '-' + Path(member.name).name)
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(archive.extractfile(member).read())
            licenses.append(path)
    if not licenses:
        raise ValueError(f'Native crate contains no licence texts: {name}')
    return component(output, 'Cargo native code: ' + package['name'], package['version'], [destination], licenses,
                     'The complete checksum-verified .crate archive retains the package build.rs, native sources\n'
                     'and any nested upstream build descriptions. Extract it, preserve those sources, and rebuild\n'
                     'RustPy with its committed Cargo.lock, the recorded Rust/compiler toolchain and release flags.\n',
                     'Modify the retained crate sources, configure a local Cargo patch for this exact package,\n'
                     'and rebuild/relink RustPy. Preserve the package native build features and target ABI.\n')


def validate_runtime(runtime, binary):
    for path in runtime.rglob('*'):
        if not path.is_file() or path.resolve() == binary:
            continue
        with path.open('rb') as stream:
            header = stream.read(4)
        mach = {b'\xfe\xed\xfa\xce', b'\xce\xfa\xed\xfe', b'\xfe\xed\xfa\xcf', b'\xcf\xfa\xed\xfe',
                b'\xca\xfe\xba\xbe', b'\xca\xfe\xba\xbf', b'\xbe\xba\xfe\xca', b'\xbf\xba\xfe\xca'}
        if header == b'\x7fELF' or header[:2] == b'MZ' or header in mach or re.search(r'(\.so(?:\.|$)|\.dll$|\.dylib$)', path.name, re.I):
            raise ValueError(f'Bundled runtime library needs its own evidence; this producer handles unbundled archives: {path.name}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--linked-binary', required=True, type=Path)
    parser.add_argument('--link-map', required=True, type=Path)
    parser.add_argument('--link-trace', required=True, type=Path)
    parser.add_argument('--link-record', required=True, type=Path)
    parser.add_argument('--runtime-root', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--repo', type=Path, default=ROOT)
    parser.add_argument('--target-dir', type=Path, default=Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target')))
    parser.add_argument('--sysroot', type=Path)
    args = parser.parse_args()
    try:
        output = args.output.resolve()
        runtime = args.runtime_root.resolve()
        binary = args.binary.resolve()
        if output.is_relative_to(runtime):
            raise ValueError('Evidence output must be outside the runtime staging root')
        if not binary.is_relative_to(runtime):
            raise ValueError('Final binary must be inside the runtime staging root')
        if output.exists() and any(output.iterdir()):
            raise ValueError('Evidence output directory must be empty')
        output.mkdir(parents=True, exist_ok=True)
        sysroot = (args.sysroot or Path(command(['rustc', '--print', 'sysroot']))).resolve()
        rust = dict(line.split(': ', 1) for line in command(['rustc', '-vV']).splitlines() if ': ' in line)
        record = json.loads(args.link_record.read_text())
        validate_link_record(record, args.linked_binary, binary, args.link_map, args.link_trace, rust['commit-hash'])
        validate_runtime(runtime, binary)
        packages = cargo_packages(args.repo)
        paths = sorted(set(link_inputs(args.link_map.read_text(), args.repo)) |
                       set(link_inputs(args.link_trace.read_text(), args.repo)))
        system, native, dynamic = inventory(paths, args.repo.resolve(), args.target_dir.resolve(), sysroot, packages)
        sources = {name for name, _ in system}
        if 'glibc' not in sources or not any(name.startswith('gcc-') for name in sources):
            raise ValueError('This Ubuntu GNU producer requires attributed glibc and GCC startup objects')
        downloads = Downloads()
        components = [rust_component(output, sysroot, rust['release'], rust['commit-hash'], downloads)]
        provenance = provenance_archive(output, args.link_map, args.link_trace, record,
                                        args.repo.resolve(), args.target_dir.resolve(), sysroot)
        components[0]['source_archives'][retained(provenance, output)] = digest(provenance)
        for (source, version), evidence in sorted(system.items()):
            archives = ubuntu_source(source, version, output, downloads)
            licenses = copy_licenses(evidence['copyright'], output / 'licenses' / source)
            components.append(component(output, source, version, archives, licenses,
                                        'These exact Ubuntu source files include the upstream source and Debian/Ubuntu patches.\n'
                                        'Verify the .dsc SHA256 inventory, extract with dpkg-source -x FILE.dsc, install the\n'
                                        'Build-Depends declared in debian/control, then use dpkg-buildpackage -b -uc -us.\n'
                                        'The retained package copyright and source include the component build instructions.\n',
                                        'Rebuild the exact source package for the same target/ABI with your modifications,\n'
                                        'replace its startup/runtime objects in the build sysroot, then rebuild and relink RustPy.\n'))
        cargo_home = Path(os.environ.get('CARGO_HOME', Path.home() / '.cargo'))
        for (name, _), package in sorted(native.items()):
            if name not in RETAINED_NATIVE:
                components.append(cargo_component(package, output, cargo_home, args.repo))
        manifest = {'binary_sha256': digest(binary), 'system_static_components': components, 'components': [],
                    'producer': 'prepare-linux-native-evidence.py', 'link_record': record,
                    'host_dynamic_dependencies_not_bundled': dynamic,
                    'authenticated_native_crates_retained_by_collector': sorted(name for name, _ in native if name in RETAINED_NATIVE)}
        path = output / 'runtime-manifest.json'
        path.write_text(json.dumps(manifest, indent=2) + '\n')
        print(path)
    except (OSError, ValueError, KeyError, IndexError, struct.error, subprocess.CalledProcessError) as error:
        parser.exit(1, f'Linux native evidence: {error}\n')


if __name__ == '__main__':
    main()
