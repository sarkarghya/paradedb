#!/usr/bin/env python3
"""Package a verified CI artifact for an immutable fork prerelease; never publish."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import re
import tarfile


def package(directory, output, source, arch):
    metadata = json.loads((Path(__file__).parent / 'release.json').read_text())
    info = dict(line.split('=', 1) for line in (directory / 'BUILD-INFO').read_text().splitlines())
    expected = {'source': source, 'platform': f'linux/{arch}', 'postgres': '18',
                'feature': 'experimental-physical-failover', 'standby_search_reads': 'disabled',
                'extension_version': metadata['extensionVersion']}
    if info != expected:
        raise ValueError(f'Unexpected build provenance: {info}')
    checked = set()
    for line in (directory / 'SHA256SUMS').read_text().splitlines():
        digest, relative = line.split(maxsplit=1)
        path = directory / relative.removeprefix('*')
        if not path.resolve().is_relative_to(directory.resolve()) or not path.is_file() or path.is_symlink():
            raise ValueError(f'Unsafe manifest path: {relative}')
        if hashlib.sha256(path.read_bytes()).hexdigest() != digest:
            raise ValueError(f'Checksum mismatch: {relative}')
        checked.add(path.resolve())
    paths = sorted(path for path in directory.rglob('*') if path.is_file())
    if any(path.resolve() not in checked and path.name != 'SHA256SUMS' for path in paths):
        raise ValueError('Artifact contains files missing from its checksum manifest')
    for required in ['LICENSE', 'SOURCE.tar.gz', 'BUILD-INFO', 'usr/lib/postgresql/18/lib/pg_search.so',
                     'usr/share/postgresql/18/extension/pg_search.control']:
        if (directory / required).resolve() not in checked:
            raise ValueError(f'Missing verified file: {required}')
    output.mkdir(parents=True, exist_ok=True)
    name = f"pg_search-{metadata['extensionVersion']}-pointsy.{metadata['forkRevision']}-pg18-trixie-{arch}.tar.gz"
    destination = output / name
    # Exclusive creation prevents accidentally overwriting a released artifact.
    with destination.open('xb') as raw, gzip.GzipFile(filename='', fileobj=raw, mode='wb', mtime=0) as zipped:
        with tarfile.open(fileobj=zipped, mode='w') as archive:
            for path in paths:
                member = archive.gettarinfo(str(path), arcname=path.relative_to(directory).as_posix())
                member.mtime = member.uid = member.gid = 0
                member.uname = member.gname = ''
                member.mode = 0o644
                with path.open('rb') as data:
                    archive.addfile(member, data)
    print(f'{hashlib.sha256(destination.read_bytes()).hexdigest()}  {name}')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--source', required=True)
    parser.add_argument('--arch', choices=['amd64', 'arm64'], required=True)
    args = parser.parse_args()
    if not re.fullmatch(r'[a-f0-9]{40}', args.source):
        parser.error('--source must be an exact 40-character commit SHA')
    package(args.directory, args.output, args.source, args.arch)
