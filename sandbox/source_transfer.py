import fnmatch
import hashlib
import io
import json
import os
import stat
import sys
import tarfile
from pathlib import Path

MAX_FILE = 64 * 1024 * 1024
MAX_BYTES = 512 * 1024 * 1024
MAX_FILES = 100000
EXCLUDE = {'.git', '.codex', 'node_modules', 'target', '.venv', 'venv', '__pycache__', '.DS_Store', '.npmrc', '.netrc', '.pypirc'}


def identity(info):
    return (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns, info.st_mode, info.st_nlink)


def scan(root, basis, retain):
    current = {}
    payload = []
    total = 0

    def visit(directory, prefix):
        nonlocal total
        for name in sorted(os.listdir(directory)):
            if name in EXCLUDE or fnmatch.fnmatch(name, '*.pyc') or (name.startswith('.env') and name not in {'.env.example', '.env.sample'}):
                continue
            relative = prefix + name
            before = os.stat(name, dir_fd=directory, follow_symlinks=False)
            if stat.S_ISDIR(before.st_mode):
                child = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=directory)
                try:
                    if identity(os.fstat(child)) != identity(before):
                        raise RuntimeError('SOURCE_CHANGED: directory changed during export')
                    visit(child, relative + '/')
                finally:
                    os.close(child)
                continue
            if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1:
                raise ValueError('Source export requires regular files without links')
            if before.st_size > MAX_FILE:
                raise ValueError(
                    f'Source file exceeds 64 MiB: {relative!r} '
                    f'({before.st_size} bytes, {before.st_size / (1024 * 1024):.2f} MiB). '
                    'Keep browser and model downloads in XDG_CACHE_HOME outside the task source.'
                )
            total += before.st_size
            if total > MAX_BYTES or len(current) >= MAX_FILES:
                raise ValueError('Source export exceeds its bounds')
            descriptor = os.open(name, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=directory)
            with os.fdopen(descriptor, 'rb') as source:
                if identity(os.fstat(source.fileno())) != identity(before):
                    raise RuntimeError('SOURCE_CHANGED: source replaced during export')
                data = source.read(MAX_FILE + 1)
                after = os.fstat(source.fileno())
            linked = os.stat(name, dir_fd=directory, follow_symlinks=False)
            if identity(before) != identity(after) or identity(after) != identity(linked) or len(data) != before.st_size:
                raise RuntimeError('SOURCE_CHANGED: source changed during export')
            entry = {'hash': hashlib.sha256(data).hexdigest(), 'size': len(data), 'mode': before.st_mode & 0o777}
            current[relative] = entry
            if retain and basis.get(relative) != entry:
                payload.append((relative, data))
    descriptor = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        visit(descriptor, '')
    except FileNotFoundError as error:
        raise RuntimeError('SOURCE_CHANGED: source disappeared during export') from error
    finally:
        os.close(descriptor)
    return current, payload


def snapshot(workspace, basis_path, destination):
    root = Path(workspace)
    basis = json.loads(Path(basis_path).read_text())
    current, payload = scan(root, basis, True)
    manifest = {'version': 1, 'basis': basis, 'files': current, 'deleted': sorted(set(basis) - set(current))}
    with tarfile.open(destination, 'w', format=tarfile.GNU_FORMAT) as archive:
        data = json.dumps(manifest, separators=(',', ':')).encode()
        header = tarfile.TarInfo('manifest.json')
        header.size = len(data)
        header.mode = 0o600
        archive.addfile(header, io.BytesIO(data))
        for relative, data in payload:
            header = tarfile.TarInfo('payload/' + relative)
            header.size = len(data)
            header.mode = current[relative]['mode']
            archive.addfile(header, io.BytesIO(data))
    verified, _ = scan(root, {}, False)
    if current != verified:
        os.unlink(destination)
        raise RuntimeError('SOURCE_CHANGED: whole source changed during export')
    os.chmod(destination, 0o600)


def verify(workspace, archive_path):
    with tarfile.open(archive_path, 'r') as archive:
        header = archive.getmember('manifest.json')
        if not header.isfile() or header.size > MAX_FILE:
            raise ValueError('Invalid source manifest')
        manifest = json.load(archive.extractfile(header))
    current, _ = scan(Path(workspace), {}, False)
    if manifest['files'] != current:
        raise RuntimeError('SOURCE_CHANGED: source changed during transfer')


if __name__ == '__main__':
    if sys.argv[1] == '--verify':
        verify(*sys.argv[2:])
    else:
        snapshot(*sys.argv[1:])
