#!/usr/bin/env python3
"""Exercise real static C payloads (native, MIPS24Kc and ARM CPU emulation)."""
import argparse
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
TAG = 'SC0123456789abcdef0123456789abcdef'

def cksum(data):
    return tuple(map(int,subprocess.check_output(['cksum'],input=data).split()))

def run(executable, *args, fails=False):
    p = subprocess.run([*executable,TAG,*map(str,args)],stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=10)
    if fails:
        assert p.returncode != 0
        return
    assert p.returncode == 0, p.stderr
    body, tail = p.stdout.split(('\n'+TAG+':C\n').encode())
    data = bytes.fromhex(body.decode())
    assert tuple(map(int,tail.split())) == cksum(data)
    return data

def main():
    emulators = {'x86_64': [], 'mipsel': ['qemu-mipsel', '-cpu', '24Kc'],
                 'aarch64': ['qemu-aarch64', '-cpu', 'cortex-a53'],
                 'arm': ['qemu-arm', '-cpu', 'arm1176']}
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--arch', choices=emulators, action='append',
                        help='repeat to select payloads; default: all four')
    args = parser.parse_args()
    for arch in args.arch or emulators:
        binary = ROOT / 'target/helpers' / arch
        assert binary.stat().st_size <= 65536, (arch, 'helper exceeds 64 KiB')
        assert 'INTERP' not in subprocess.check_output(['readelf', '-l', str(binary)], text=True)
        assert 'NEEDED' not in subprocess.check_output(['readelf', '-d', str(binary)], text=True)
        exe = [*emulators[arch], str(binary)]
        assert run(exe,'info') == f'sericon-helper/2 {arch} check read list upload inspect'.encode()
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            path = root / "all bytes ' $()"
            data = bytes(range(256))*17 + b'\r\n\x00\xffend'
            path.write_bytes(data)
            assert tuple(map(int,run(exe,'check',path).split())) == cksum(data)
            actual = b''.join(run(exe,'read',path,offset) for offset in range(0,len(data),1024))
            assert actual == data
            empty = root/'empty'; empty.touch()
            assert run(exe,'read',empty,0) == b''
            (root/'link').symlink_to(path)
            (root/'proc').symlink_to('/proc')
            os.mkfifo(root/'fifo')
            for bad in [root/'link',root/'fifo',root/'proc/version','/dev/zero',root]:
                run(exe,'check',bad,fails=True)
                run(exe,'read',bad,0,fails=True)
            for offset in ['-1','1','67108865','999999999999999999999999','1024x']:
                run(exe,'read',path,offset,fails=True)
            listing = run(exe,'list',root).split(b'\0')
            assert b'file' in listing and os.fsencode(path) in listing
            assert b'link' in listing and b'other' in listing
            for section in ['identity','cpu','memory','uptime','mounts','storage','flash']:
                status,body=run(exe,'inspect',section).split(b'\0',1)
                assert status in (b'ok',b'truncated',b'unavailable')
                assert len(body)<=16384
                if section=='identity': assert b'helper_arch\0'+arch.encode()+b'\0' in body
            run(exe,'inspect','../../etc/passwd',fails=True)
            dest=root/'uploaded'; token='1'*32
            stage=root/f'.sericon-upload-{token}.partial'
            assert run(exe,'upload-begin',dest,token,len(data))==b'ready'
            assert run(exe,'upload-begin',dest,token,len(data))==b'ready'
            run(exe,'upload-write',stage,0,data[:1024].hex(),0,fails=True)
            assert stage.read_bytes()==b''
            for offset in range(0,len(data),1024):
                part=data[offset:offset+1024]
                for _ in range(2):
                    assert run(exe,'upload-write',stage,offset,part.hex(),cksum(part)[0])==part
            run(exe,'upload-commit',stage,dest,len(data),0,'700',fails=True)
            assert not dest.exists() and stage.read_bytes()==data
            for _ in range(2):
                assert run(exe,'upload-commit',stage,dest,len(data),cksum(data)[0],'700')==b'committed'
            assert dest.read_bytes()==data and dest.stat().st_mode&0o777==0o700
            assert not stage.exists()
            run(exe,'upload-begin',dest,token,len(data),fails=True)
            assert dest.read_bytes()==data
            # A file appearing at publication time is never replaced.
            collision=root/'collision'; token='2'*32
            stage=root/f'.sericon-upload-{token}.partial'
            run(exe,'upload-begin',collision,token,0)
            collision.write_bytes(b'keep')
            run(exe,'upload-commit',stage,collision,0,cksum(b'')[0],'600',fails=True)
            assert collision.read_bytes()==b'keep' and stage.exists()
            collision.unlink()
            run(exe,'upload-commit',stage,collision,0,cksum(b'')[0],'600')
            assert collision.read_bytes()==b'' and collision.stat().st_mode&0o777==0o600
            for bad in (root/'link', root/'proc'/'new-file','/dev/new-file'):
                run(exe,'upload-begin',bad,'3'*32,10,fails=True)
            run(exe,'upload-write',path,0,'00',cksum(b'\0')[0],fails=True)
            assert path.read_bytes()==data
        print(f'{arch}: reads, inspector, verified/retried uploads, corruption, collision, modes and unsafe paths passed')

if __name__=='__main__':
    main()
