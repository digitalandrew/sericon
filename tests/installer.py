#!/usr/bin/env python3
"""Check the public installer with temporary prefixes; never change the user's install."""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / 'scripts/install.sh'
VERSION = re.search(r'sericon_version=([0-9.]+)', SCRIPT.read_text())[1]
ARCHIVE = f'sericon-{VERSION}-linux-x86_64.tar.gz'


class InstallerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cache = tempfile.TemporaryDirectory(prefix='sericon-installer-assets-')
        cls.archive = ROOT / 'target/releases' / f'v{VERSION}' / ARCHIVE
        if not cls.archive.exists():
            cls.archive = Path(cls.cache.name) / ARCHIVE
            url = f'https://github.com/digitalandrew/sericon/releases/download/v{VERSION}/{ARCHIVE}'
            with urllib.request.urlopen(url, timeout=60) as response:
                cls.archive.write_bytes(response.read())

    @classmethod
    def tearDownClass(cls):
        cls.cache.cleanup()

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='sericon-installer-test-')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.prefix = self.root / 'prefix with spaces ; literal $()'
        self.bin = self.root / 'commands'
        self.bin.mkdir()
        self.tmp = self.root / 'tmp'
        self.tmp.mkdir()
        self.requests = self.root / 'requests.txt'
        self.env = dict(os.environ, PATH=str(self.bin) + os.pathsep + os.environ['PATH'],
                        TMPDIR=str(self.tmp), SERICON_TEST_OS='Linux', SERICON_TEST_MACHINE='x86_64',
                        SERICON_TEST_BITS='64', SERICON_TEST_ARCHIVE=str(self.archive),
                        SERICON_TEST_REQUESTS=str(self.requests), SERICON_TEST_DOWNLOAD='ok')
        self.command('uname', 'import os,sys\nprint(os.environ["SERICON_TEST_OS" if sys.argv[1]=="-s" else "SERICON_TEST_MACHINE"])\n')
        self.command('getconf', 'import os\nprint(os.environ["SERICON_TEST_BITS"])\n')
        self.command('curl', '''import os,sys,shutil
from pathlib import Path
args=sys.argv[1:]
Path(os.environ['SERICON_TEST_REQUESTS']).write_text(args[-1])
out=Path(args[args.index('--output')+1])
mode=os.environ['SERICON_TEST_DOWNLOAD']
if mode=='fail':
    out.write_bytes(b'partial download')
    sys.exit(22)
if mode=='corrupt':
    out.write_bytes(b'invalid archive')
else:
    shutil.copyfile(os.environ['SERICON_TEST_ARCHIVE'],out)
''')

    def command(self, name, source):
        path = self.bin / name
        path.write_text(f'#!{sys.executable}\n' + source)
        path.chmod(0o755)

    def install(self, *, prefix=None, stdin=False):
        args = ['--prefix', str(self.prefix if prefix is None else prefix)]
        command = ['sh', '-s', '--', *args] if stdin else ['sh', str(SCRIPT), *args]
        result = subprocess.run(command, input=SCRIPT.read_text() if stdin else None,
                                capture_output=True, text=True, env=self.env, timeout=30)
        self.assertEqual(list(self.tmp.iterdir()), [], result.stderr)
        return result

    def old_install(self):
        binary = self.prefix / 'bin/sericon'
        binary.parent.mkdir(parents=True)
        binary.write_bytes(b'keep existing executable')
        return binary

    def test_piped_install_and_atomic_reinstall_with_running_process(self):
        result = self.install(stdin=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        binary = self.prefix / 'bin/sericon'
        self.assertEqual(binary.stat().st_mode & 0o777, 0o755)
        self.assertEqual(subprocess.check_output([str(binary), '--version'], text=True).strip(), f'sericon {VERSION}')
        self.assertEqual(len(json.loads(subprocess.check_output([str(binary), 'files', 'helpers']))), 4)
        retained = self.prefix / 'share/sericon' / ARCHIVE
        self.assertEqual(hashlib.sha256(retained.read_bytes()).digest(), hashlib.sha256(self.archive.read_bytes()).digest())
        old_inode = binary.stat().st_ino
        process = subprocess.Popen([str(binary), 'mcp'], stdin=subprocess.PIPE,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   env=dict(self.env, SERICON_RUNTIME_DIR=str(self.root / 'runtime')))
        try:
            result = self.install()
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIsNone(process.poll())
            self.assertNotEqual(old_inode, binary.stat().st_ino)
            process.communicate(timeout=5)
            self.assertEqual(process.returncode, 0)
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate()
        self.assertEqual(list(binary.parent.iterdir()), [binary])
        self.assertEqual(list(retained.parent.iterdir()), [retained])

    def test_corruption_preserves_existing_install(self):
        binary = self.old_install()
        self.env['SERICON_TEST_DOWNLOAD'] = 'corrupt'
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('checksum mismatch', result.stderr)
        self.assertEqual(binary.read_bytes(), b'keep existing executable')
        self.assertFalse((self.prefix / 'share').exists())

    def test_download_failure_preserves_existing_install(self):
        binary = self.old_install()
        self.env['SERICON_TEST_DOWNLOAD'] = 'fail'
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('download failed', result.stderr)
        self.assertEqual(binary.read_bytes(), b'keep existing executable')

    def test_architecture_selection(self):
        self.env['SERICON_TEST_DOWNLOAD'] = 'fail'
        for machine, bits, archive_arch in [('x86_64', '64', 'x86_64'), ('aarch64', '64', 'aarch64'),
                                           ('aarch64', '32', 'armv6hf'), ('armv6l', '32', 'armv6hf'),
                                           ('armv7l', '32', 'armv6hf'), ('armv8l', '32', 'armv6hf')]:
            with self.subTest(machine=machine, bits=bits):
                self.env.update(SERICON_TEST_MACHINE=machine, SERICON_TEST_BITS=bits)
                self.assertNotEqual(self.install().returncode, 0)
                self.assertEqual(self.requests.read_text(), f'https://github.com/digitalandrew/sericon/releases/download/v{VERSION}/sericon-{VERSION}-linux-{archive_arch}.tar.gz')
                self.assertFalse(self.prefix.exists())

    def test_unsupported_hosts_do_not_download(self):
        for os_name, machine in [('Darwin', 'aarch64'), ('Linux', 'i686'), ('Linux', 'armv5l'), ('Linux', 'aarch64_be')]:
            with self.subTest(os=os_name, machine=machine):
                self.env.update(SERICON_TEST_OS=os_name, SERICON_TEST_MACHINE=machine)
                self.assertNotEqual(self.install().returncode, 0)
                self.assertFalse(self.requests.exists())
                self.assertFalse(self.prefix.exists())

    def test_relative_prefix_rejected(self):
        result = self.install(prefix='relative-prefix')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('absolute directory', result.stderr)
        self.assertFalse(self.requests.exists())

    def test_existing_directory_is_not_replaced(self):
        binary = self.prefix / 'bin/sericon'
        binary.mkdir(parents=True)
        (binary / 'keep').write_text('keep')
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('is a directory', result.stderr)
        self.assertEqual((binary / 'keep').read_text(), 'keep')
        self.assertFalse((self.prefix / 'share').exists())

    def test_partial_script_does_not_install(self):
        result = subprocess.run(['sh', '-s', '--', '--prefix', str(self.prefix)],
                                input=SCRIPT.read_text()[:2000], capture_output=True, text=True, env=self.env)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.requests.exists())
        self.assertFalse(self.prefix.exists())


if __name__ == '__main__':
    unittest.main(verbosity=2)
