"""Offline checks for immutable fork artifact packaging (no Docker or network)."""
import contextlib
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('package_artifact', ROOT / 'fork/package-artifact.py')
packager = importlib.util.module_from_spec(spec)
spec.loader.exec_module(packager)


class ArtifactTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.artifact = self.root / 'input'
        self.artifact.mkdir()
        self.source = 'a' * 40
        version = json.loads((ROOT / 'fork/release.json').read_text())['extensionVersion']
        files = {'LICENSE': b'test license', 'SOURCE.tar.gz': b'test source',
                 'usr/lib/postgresql/18/lib/pg_search.so': b'test library',
                 'usr/share/postgresql/18/extension/pg_search.control': b'test control',
                 'BUILD-INFO': (f'source={self.source}\nplatform=linux/amd64\npostgres=18\n'
                                'feature=experimental-physical-failover\nstandby_search_reads=disabled\n'
                                f'extension_version={version}\n').encode()}
        for name, content in files.items():
            path = self.artifact / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)
        (self.artifact / 'SHA256SUMS').write_text(''.join(
            f'{hashlib.sha256(content).hexdigest()}  ./{name}\n' for name, content in files.items()))

    def package(self, name='output', source=None):
        with contextlib.redirect_stdout(io.StringIO()):
            packager.package(self.artifact, self.root / name, source or self.source, 'amd64')

    def test_deterministic_and_never_overwrites(self):
        self.package('first')
        self.package('second')
        self.assertEqual(next((self.root / 'first').iterdir()).read_bytes(),
                         next((self.root / 'second').iterdir()).read_bytes())
        with self.assertRaises(FileExistsError):
            self.package('first')

    def test_rejects_wrong_revision(self):
        with self.assertRaisesRegex(ValueError, 'provenance'):
            self.package(source='b' * 40)

    def test_rejects_tampering(self):
        (self.artifact / 'LICENSE').write_text('changed')
        with self.assertRaisesRegex(ValueError, 'Checksum mismatch'):
            self.package()

    def test_rejects_unverified_files(self):
        (self.artifact / 'extra').write_text('unverified')
        with self.assertRaisesRegex(ValueError, 'missing from its checksum'):
            self.package()

    def test_rejects_manifest_path_escape(self):
        with (self.artifact / 'SHA256SUMS').open('a') as manifest:
            manifest.write(f'{"0" * 64}  ../escape\n')
        with self.assertRaisesRegex(ValueError, 'Unsafe manifest path'):
            self.package()


if __name__ == '__main__':
    unittest.main()
