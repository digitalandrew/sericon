#!/usr/bin/env python3
"""Build static DUT payloads before cargo build. Build tools are host-only."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
VERSION = '1.2.5'
DIGEST = 'a9a118bbe84d8764da0ea0d28b3ab3fae8477fc7e4085d90102b8596fc7c75e4'
TARGETS = {
    'x86_64': ('gcc', '', []),
    'mipsel': ('mipsel-linux-gnu-gcc', 'mipsel-linux-musl', ['-march=mips32', '-msoft-float']),
    'mips': ('mips-linux-gnu-gcc', 'mips-linux-musl', ['-march=mips32', '-msoft-float']),
    'aarch64': ('aarch64-linux-gnu-gcc', 'aarch64-linux-musl', []),
    # This toolchain also supplies ARMv6-compatible startup objects and libgcc.
    # Distribution arm-linux-gnueabihf runtimes commonly require ARMv7.
    'arm': ('arm-buildroot-linux-musleabihf-gcc', 'arm-linux-musleabihf', ['-march=armv6', '-marm', '-mfpu=vfp', '-mfloat-abi=hard']),
}
DEFAULT_ARCHES = ['x86_64', 'mipsel', 'aarch64', 'arm']

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--arch', choices=TARGETS, action='append',
                        help='repeat to select payloads; default: ' + ', '.join(DEFAULT_ARCHES))
    args = parser.parse_args()
    cache = ROOT / '.local/toolchains'
    downloads = cache / 'downloads'
    downloads.mkdir(parents=True, exist_ok=True)
    archive = downloads / f'musl-{VERSION}.tar.gz'
    if not archive.exists():
        with urllib.request.urlopen(f'https://musl.libc.org/releases/{archive.name}', timeout=90) as src:
            archive.write_bytes(src.read())
    if hashlib.sha256(archive.read_bytes()).hexdigest() != DIGEST:
        raise SystemExit('musl archive hash mismatch')
    source = cache / f'musl-{VERSION}'
    if not source.exists():
        with tarfile.open(archive) as tar:
            tar.extractall(cache, filter='data')
    out = ROOT / 'target/helpers'
    out.mkdir(parents=True, exist_ok=True)
    for arch in args.arch or DEFAULT_ARCHES:
        cc, target, flags = TARGETS[arch]
        compiler = shutil.which(cc)
        if not compiler:
            raise SystemExit(f'missing build compiler: {cc}')
        cc = compiler
        compiler_version = subprocess.check_output([cc, '--version'], text=True).splitlines()[0]
        # Changing the compiler or flags must not reuse a libc built for another ISA.
        settings = json.dumps([VERSION, DIGEST, cc, compiler_version, target, flags]).encode()
        cache_key = hashlib.sha256(settings).hexdigest()[:12]
        prefix = cache / f'musl-{arch}-{cache_key}'
        wrapper = prefix / 'bin/musl-gcc'
        build = cache / f'build-{arch}-{cache_key}'
        build.mkdir(exist_ok=True)
        compiler_prefix = cc[:-3] if cc.endswith('gcc') else ''
        if not wrapper.exists():
            configure = [str(source/'configure'), '--disable-shared', '--enable-wrapper=gcc', f'--prefix={prefix}',
                         f'CC={cc}', f'AR={compiler_prefix}ar', f'RANLIB={compiler_prefix}ranlib',
                         'CFLAGS=-Os -ffunction-sections -fdata-sections ' + ' '.join(flags)]
            if target:
                configure.append(f'--target={target}')
            with (build/'build.log').open('w') as log:
                for command in [configure, ['make', '-j8'], ['make', 'install']]:
                    subprocess.run(command, cwd=build, stdout=log, stderr=subprocess.STDOUT, check=True)
        binary = out / arch
        # A failed rebuild must never leave an earlier stamp on an invalid payload.
        (out/(arch+'.stamp')).unlink(missing_ok=True)
        subprocess.run([str(wrapper), '-std=c99', '-Os', '-static', '-fno-pie', '-no-pie',
                        '-ffunction-sections', '-fdata-sections', '-Wall', '-Wextra', '-Werror',
                        '-Wl,--gc-sections,-s,-z,max-page-size=4096', *flags, f'-DSC_ARCH="{arch}"',
                        str(ROOT/'helper/sericon-helper.c'), '-o', str(binary)], check=True)
        data = binary.read_bytes()
        if len(data)>65536:
            raise SystemExit(f'{arch}: helper exceeds 64 KiB budget ({len(data)})')
        ph = subprocess.check_output(['readelf', '-l', str(binary)], text=True)
        dynamic = subprocess.check_output(['readelf', '-d', str(binary)], text=True)
        if 'INTERP' in ph or 'NEEDED' in dynamic:
            raise SystemExit(f'{arch}: payload is not self-contained')
        if arch == 'arm':
            attributes = subprocess.check_output(['readelf', '-A', str(binary)], text=True)
            cpu = re.search(r'Tag_CPU_arch:\s*(\S+)', attributes)
            fp = re.search(r'Tag_FP_arch:\s*(\S+)', attributes)
            if not cpu or cpu[1] not in ('v6', 'v6KZ', 'v6K') or not fp or fp[1] not in ('VFPv1', 'VFPv2'):
                raise SystemExit('arm: runtime exceeds ARMv6/VFP; use the documented ARMv6 toolchain')
        metadata = dict(arch=arch, size=len(data), sha256=hashlib.sha256(data).hexdigest(),
                        musl=VERSION, musl_source_sha256=DIGEST,
                        compiler=compiler_version, flags=flags)
        (out/(arch+'.json')).write_text(json.dumps(metadata,indent=2)+'\n')
        # Cargo refuses stale payloads after either source or build recipe changes.
        (out/(arch+'.stamp')).write_bytes((ROOT/'helper/sericon-helper.c').read_bytes() + Path(__file__).read_bytes())
        print(json.dumps(metadata))
    (out/'MUSL-COPYRIGHT').write_bytes((source/'COPYRIGHT').read_bytes())

if __name__=='__main__':
    main()
