#!/usr/bin/env python3
"""Package validated static builds from a clean checkout; never publish them."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]
TARGETS = {
    'x86_64-unknown-linux-musl': ('linux-x86_64', []),
    'aarch64-unknown-linux-musl': ('linux-aarch64', ['qemu-aarch64']),
    'arm-unknown-linux-musleabihf': ('linux-armv6hf', ['qemu-arm', '-cpu', 'arm1176']),
}
HELPERS = ['x86_64', 'mipsel', 'aarch64', 'arm']


def run(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True, timeout=120).strip()


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def copy(source, destination):
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')


def dependency_notices(destination, target, lock):
    metadata = json.loads(run('cargo', 'metadata', '--locked', '--format-version', '1',
                              '--filter-platform', target))
    active = {node['id'] for node in metadata['resolve']['nodes']}
    packages = sorted((p for p in metadata['packages']
                       if p['id'] in active and p['name'] != 'sericon'),
                      key=lambda p: (p['name'], p['version']))
    inventory = []
    for package in packages:
        name, version = package['name'], package['version']
        label = f'{name}-{version}'
        source = Path(package['manifest_path']).parent
        licenses = [p for p in source.rglob('*') if p.is_file() and
                    p.name.lower().startswith(('license', 'licence', 'copying', 'copyright', 'notice'))]
        if not licenses:
            source = ROOT / 'scripts/release-licenses' / label
            licenses = sorted(source.glob('LICENSE*'))
        if not licenses:
            raise SystemExit(f'Missing license notices for {label}')
        for path in licenses:
            copy(path, destination / 'licenses/crates' / label / path.relative_to(source))
        entry = {key: package[key] for key in ('name', 'version', 'license', 'repository')}
        # Include the unchanged, checksum-verified source archives for MPL crates.
        if 'MPL' in (package['license'] or ''):
            source = Path(package['manifest_path']).parent
            archive = source.parents[2] / 'cache' / source.parent.name / f'{label}.crate'
            expected = next(p['checksum'] for p in lock['package']
                            if p['name'] == name and p['version'] == version)
            if digest(archive) != expected:
                raise SystemExit(f'Source checksum mismatch for {label}')
            copy(archive, destination / 'sources' / archive.name)
            entry['included_source'] = f'sources/{archive.name}'
        inventory.append(entry)
    write_json(destination / 'DEPENDENCIES.json', inventory)
    rust_docs = Path(run('rustc', '--print', 'sysroot')) / 'share/doc/rust'
    for name in ['COPYRIGHT.html', 'COPYRIGHT-library.html']:
        copy(rust_docs / name, destination / 'licenses/rust' / name)
    shutil.copytree(rust_docs / 'licenses', destination / 'licenses/rust/licenses')
    copy(ROOT / 'helper/MUSL-COPYRIGHT', destination / 'licenses/MUSL-COPYRIGHT')
    copy(ROOT / 'scripts/release-licenses/README.md', destination / 'licenses/SUPPLEMENTAL.md')
    (destination / 'THIRD-PARTY-NOTICES.txt').write_text(
        'Sericon is MIT licensed; see LICENSE. Dependencies retain their own licenses.\n'
        'DEPENDENCIES.json lists resolved runtime and build dependencies for this target.\n'
        'Their license and copyright notices are in licenses/crates/.\n'
        'Rust standard-library/runtime notices are in licenses/rust/.\n'
        'The four embedded DUT helpers use musl 1.2.5; see licenses/MUSL-COPYRIGHT.\n'
        'GCC runtime objects use the GCC Runtime Library Exception; its text and GPLv3\n'
        'are included in licenses/rust/licenses/.\n'
        'Unmodified MPL-licensed serialport and smartstring source is included in sources/\n'
        'as checksum-verified .crate (tar.gz) archives, under the MPL terms in each archive.\n'
        'Sericon source is available at https://github.com/digitalandrew/sericon.\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, help='new directory; default: target/releases/vVERSION')
    args = parser.parse_args()
    if run('git', 'status', '--porcelain'):
        raise SystemExit('Commit source changes before packaging a release.')
    version = tomllib.loads((ROOT / 'Cargo.toml').read_text())['package']['version']
    commit = run('git', 'rev-parse', 'HEAD')
    timestamp = int(run('git', 'show', '-s', '--format=%ct', 'HEAD'))
    lock = tomllib.loads((ROOT / 'Cargo.lock').read_text())
    output = (args.output or ROOT / 'target/releases' / f'v{version}').resolve()
    output.mkdir(parents=True, exist_ok=False)
    checksums = []
    stamp = (ROOT / 'helper/sericon-helper.c').read_bytes() + (ROOT / 'helper/build.py').read_bytes()
    for arch in HELPERS:
        if (ROOT / 'target/helpers' / f'{arch}.stamp').read_bytes() != stamp:
            raise SystemExit(f'Stale {arch} helper; rebuild before packaging.')
    for target, (label, emulator) in TARGETS.items():
        binary = ROOT / 'target' / target / 'release/sericon'
        if 'INTERP' in run('readelf', '-l', str(binary)) or 'NEEDED' in run('readelf', '-d', str(binary)):
            raise SystemExit(f'{target} is not statically linked')
        if run(*emulator, str(binary), '--version') != f'sericon {version}':
            raise SystemExit(f'{target} version mismatch')
        inventory = json.loads(run(*emulator, str(binary), 'files', 'helpers'))
        if [p['arch'] for p in inventory] != HELPERS:
            raise SystemExit(f'{target} is missing a required helper')
        for helper in inventory:
            if helper['sha256'] != digest(ROOT / 'target/helpers' / helper['arch']):
                raise SystemExit(f'{target} embeds a stale helper')
        name = f'sericon-{version}-{label}'
        with tempfile.TemporaryDirectory(prefix='sericon-package-') as temporary:
            directory = Path(temporary) / name
            directory.mkdir()
            copy(binary, directory / 'sericon')
            (directory / 'sericon').chmod(0o755)
            for source in ['LICENSE', 'Cargo.lock', 'examples/config.toml',
                           'formulas/examples/tplink-uboot-interrupt.rhai']:
                copy(ROOT / source, directory / source)
            for arch in HELPERS:
                copy(ROOT / 'target/helpers' / f'{arch}.json', directory / 'helper-builds' / f'{arch}.json')
            dependency_notices(directory, target, lock)
            write_json(directory / 'BUILD-INFO.json', {
                'version': version, 'source_commit': commit, 'target': target,
                'rustc': run('rustc', '--version'), 'cargo': run('cargo', '--version'),
                'cargo_lock_sha256': digest(ROOT / 'Cargo.lock'),
                'binary_sha256': digest(binary), 'helpers': inventory,
                'build': f'cargo build --release --locked --target {target}',
                'linker': 'rust-lld',
            })
            (directory / 'INSTALL.txt').write_text(
                f'Sericon {version} beta for {label}\n\n'
                'Install: install -Dm755 sericon "$HOME/.local/bin/sericon"\n'
                'Put ~/.local/bin on PATH, then run: sericon\n'
                'Your user needs read/write access to the UART device.\n'
                'This static executable includes all four optional DUT helpers.\n'
                'No Rust, Python, shared libraries or AI provider is required to run it.\n'
                'Ctrl-] then m opens the menu; Ctrl-] then q stops the session.\n'
                'Keep LICENSE, THIRD-PARTY-NOTICES.txt, licenses/ and sources/ when redistributing.\n'
                'Documentation: https://sericon.xyz/\n'
                'Beta limits and hardware coverage: https://sericon.xyz/beta/\n')
            archive = output / f'{name}.tar.gz'
            with archive.open('wb') as raw, gzip.GzipFile(filename='', fileobj=raw, mode='wb', mtime=0) as gz:
                with tarfile.open(fileobj=gz, mode='w') as tar:
                    for path in [directory, *sorted(directory.rglob('*'))]:
                        info = tar.gettarinfo(str(path), arcname=str(path.relative_to(directory.parent)))
                        info.uid = info.gid = 0
                        info.uname = info.gname = ''
                        info.mtime = timestamp
                        info.mode = 0o755 if path.is_dir() or path == directory / 'sericon' else 0o644
                        if path.is_file():
                            with path.open('rb') as data:
                                tar.addfile(info, data)
                        else:
                            tar.addfile(info)
            checksums.append(f'{digest(archive)}  {archive.name}\n')
            print(f'{archive.name}: {archive.stat().st_size:,} bytes')
    (output / 'SHA256SUMS').write_text(''.join(checksums))
    print(f'Packages and SHA256SUMS written to {output}')


if __name__ == '__main__':
    main()
