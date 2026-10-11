"""Copy verified project assets; never share a writable worker directory."""

import ctypes
from contextlib import contextmanager
import errno
import fcntl
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import stat
import sys
import tempfile

MAX_BYTES = 512 * 1024 * 1024
MAX_FILES = 100_000
MAX_ENTRIES = 3
ROOTS = [
    ("home", ".local/share/mise/installs"),
    ("home", ".npm/_cacache/content-v2"),
    ("home", ".cache/pip/wheels"),
]
PRIVATE = {".git", ".codex", ".muse", ".config", ".ssh", ".npmrc", ".netrc", ".pypirc",
           "auth.json", "credentials", "credentials.json", "token", "token.json", "history", "history.json"}


def relative(value):
    path = PurePosixPath(value)
    if not value or path.is_absolute() or any(part in ("", ".", "..") for part in value.split("/")):
        raise ValueError("Cache path is not relative")
    return path


def private(name):
    return name in PRIVATE or name.startswith(".env")


def open_path(path, flags):
    """Reject links in every component, including worker-controlled parents."""
    parts = Path(path).absolute().parts
    fd = os.open(parts[0], os.O_RDONLY | os.O_DIRECTORY)
    try:
        for index, part in enumerate(parts[1:]):
            next_fd = os.open(part, (flags if index == len(parts) - 2 else os.O_RDONLY | os.O_DIRECTORY)
                             | os.O_NOFOLLOW, dir_fd=fd)
            os.close(fd)
            fd = next_fd
        return fd
    except BaseException:
        os.close(fd)
        raise


def directory(base, path, create=False):
    fd = os.dup(base)
    try:
        for part in PurePosixPath(path).parts:
            if create:
                try:
                    os.mkdir(part, 0o700, dir_fd=fd)
                except FileExistsError:
                    pass
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
            os.close(fd)
            fd = child
        return fd
    except BaseException:
        os.close(fd)
        raise


def file_at(base, path, flags, mode=0o600):
    path = relative(str(path))
    parent = directory(base, path.parent)
    try:
        return os.open(path.name, flags | os.O_NOFOLLOW, mode, dir_fd=parent)
    finally:
        os.close(parent)


def checksum(base, path):
    digest = hashlib.sha256()
    with os.fdopen(file_at(base, path, os.O_RDONLY), "rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def confined_target(base, path):
    pending = list(path)
    resolved = []
    links = 0
    while pending:
        part = pending.pop(0)
        if part in ("", "."):
            continue
        if part == "..":
            if not resolved:
                raise ValueError("Cache link leaves its asset root")
            resolved.pop()
            continue
        parent = directory(base, PurePosixPath(*resolved))
        try:
            info = os.stat(part, dir_fd=parent, follow_symlinks=False)
            if stat.S_ISLNK(info.st_mode):
                target = os.readlink(part, dir_fd=parent)
                links += 1
                if Path(target).is_absolute() or links > 40:
                    raise ValueError("Cache link is absolute or cyclic")
                pending = target.split("/") + pending
            else:
                resolved.append(part)
        finally:
            os.close(parent)


def scan(root, wheel_only=False, exclude_private=True, complete=False):
    fd = open_path(root, os.O_RDONLY | os.O_DIRECTORY)
    try:
        return scan_fd(fd, wheel_only, exclude_private, complete)
    finally:
        os.close(fd)


def scan_fd(base, wheel_only=False, exclude_private=True, complete=False):
    items = []
    total = 0

    def walk(fd, prefix):
        nonlocal total
        with os.scandir(fd) as listing:
            entries = sorted(listing, key=lambda entry: entry.name)
        for entry in entries:
            if exclude_private and private(entry.name):
                if complete:
                    raise ValueError("Dependency snapshot contains private configuration or history")
                continue
            path = prefix / entry.name
            info = entry.stat(follow_symlinks=False)
            item = {"path": str(path), "mode": stat.S_IMODE(info.st_mode) & 0o777}
            if stat.S_ISDIR(info.st_mode):
                item["kind"] = "dir"
                child = os.open(entry.name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
                try:
                    items.append(item)
                    walk(child, path)
                finally:
                    os.close(child)
            elif stat.S_ISREG(info.st_mode):
                if wheel_only and not entry.name.endswith(".whl"):
                    continue
                if info.st_nlink != 1 or info.st_size > MAX_BYTES:
                    raise ValueError("Cache file has links or exceeds its size limit")
                total += info.st_size
                if total > MAX_BYTES:
                    raise ValueError("Cache exceeds 512 MiB")
                item.update(kind="file", size=info.st_size, sha256=checksum(base, path))
                items.append(item)
            elif stat.S_ISLNK(info.st_mode):
                target = os.readlink(entry.name, dir_fd=fd)
                if Path(target).is_absolute():
                    raise ValueError("Absolute cache links are unsupported")
                confined_target(base, list(path.parent.parts) + target.split("/"))
                item.update(kind="link", target=target)
                items.append(item)
            else:
                raise ValueError("Cache contains a special file")
            if len(items) > MAX_FILES:
                raise ValueError("Cache exceeds its file limit")

    mode = stat.S_IMODE(os.fstat(base).st_mode) & 0o777
    walk(base, PurePosixPath())
    return {"mode": mode, "items": sorted(items, key=lambda item: item["path"]), "bytes": total}


def copy_tree(source, destination, tree):
    destination.mkdir(mode=0o700)
    fd = open_path(destination, os.O_RDONLY | os.O_DIRECTORY)
    try:
        copy_to_fd(source, fd, tree)
    finally:
        os.close(fd)


def copy_to_fd(source, destination, tree):
    src_root = open_path(source, os.O_RDONLY | os.O_DIRECTORY)
    try:
        for item in tree["items"]:
            path = relative(item["path"])
            parent = directory(destination, path.parent)
            try:
                if item["kind"] == "dir":
                    os.mkdir(path.name, 0o700, dir_fd=parent)
                elif item["kind"] == "link":
                    os.symlink(item["target"], path.name, dir_fd=parent)
                else:
                    with os.fdopen(file_at(src_root, path, os.O_RDONLY), "rb") as src, \
                            os.fdopen(file_at(destination, path, os.O_WRONLY | os.O_CREAT | os.O_EXCL), "wb") as dst:
                        shutil.copyfileobj(src, dst, 1024 * 1024)
                        os.fchmod(dst.fileno(), item["mode"])
            finally:
                os.close(parent)
        for item in reversed(tree["items"]):
            if item["kind"] == "dir":
                fd = directory(destination, item["path"])
                try:
                    os.fchmod(fd, item["mode"])
                finally:
                    os.close(fd)
        os.fchmod(destination, tree["mode"])
        if scan_fd(destination, exclude_private=False) != tree:
            raise ValueError("Asset changed during cache copy")
    finally:
        os.close(src_root)


def readonly_tree(tree):
    result = json.loads(json.dumps(tree))
    result["mode"] &= ~0o222
    for item in result["items"]:
        if item["kind"] != "link":
            item["mode"] &= ~0o222
    return result


def freeze(directory):
    for parent, dirs, files in os.walk(directory, followlinks=False):
        for name in dirs + files:
            path = Path(parent) / name
            if not path.is_symlink():
                path.chmod(stat.S_IMODE(path.stat().st_mode) & ~0o222)
        Path(parent).chmod(stat.S_IMODE(Path(parent).stat().st_mode) & ~0o222)


def remove_at(parent, name):
    info = os.stat(name, dir_fd=parent, follow_symlinks=False)
    if not stat.S_ISDIR(info.st_mode):
        os.unlink(name, dir_fd=parent)
        return
    fd = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent)
    try:
        os.fchmod(fd, 0o700)
        with os.scandir(fd) as entries:
            names = [entry.name for entry in entries]
        for child in names:
            remove_at(fd, child)
    finally:
        os.close(fd)
    os.rmdir(name, dir_fd=parent)


def remove(path):
    parent = open_path(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        remove_at(parent, path.name)
    finally:
        os.close(parent)


def verified(entry, key):
    if entry.stat().st_mode & 0o222 or (entry / "manifest.json").stat().st_mode & 0o222:
        raise ValueError("Cache entry is writable")
    if (entry / "manifest.json").stat().st_size > 32 * 1024 * 1024:
        raise ValueError("Cache manifest exceeds its size limit")
    with os.fdopen(open_path(entry / "manifest.json", os.O_RDONLY), "r") as source:
        manifest = json.load(source)
    roots = manifest["roots"]
    if manifest["version"] != 1 or manifest["key"] != key or not isinstance(roots, list) or len(roots) > 4:
        raise ValueError("Cache manifest does not match")
    total = count = 0
    seen = set()
    if {path.name for path in entry.iterdir()} != {"manifest.json"} | {str(index) for index in range(len(roots))}:
        raise ValueError("Cache entry contains unexpected files")
    for index, root in enumerate(roots):
        kind = (root["scope"], root["path"])
        if kind not in ROOTS + [("workspace", "node_modules")] or kind in seen:
            raise ValueError("Cache root is not allowed")
        seen.add(kind)
        tree = root["tree"]
        total += tree["bytes"]
        count += len(tree["items"])
        if total > MAX_BYTES or count > MAX_FILES:
            raise ValueError("Cache manifest exceeds its limits")
        if scan(entry / str(index), exclude_private=False) != readonly_tree(tree):
            raise ValueError("Cache content or modes changed")
    return roots


def cleanup_restores(cache):
    for stage in cache.glob(".restore-*"):
        if not stage.is_dir() or stage.is_symlink():
            continue
        fd = open_path(stage, os.O_RDONLY | os.O_DIRECTORY)
        try:
            lease = os.open(".lease", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600, dir_fd=fd)
        finally:
            os.close(fd)
        with os.fdopen(lease, "a") as lock:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                continue
            remove(stage)


@contextmanager
def locked_cache(cache):
    cache.mkdir(parents=True, exist_ok=True, mode=0o700)
    directory = open_path(cache, os.O_RDONLY | os.O_DIRECTORY)
    try:
        lock_fd = os.open(".publish.lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600, dir_fd=directory)
    finally:
        os.close(directory)
    with os.fdopen(lock_fd, "a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        cleanup_restores(cache)
        yield


def publish(args):
    cache = Path(args["cache"])
    with locked_cache(cache):
        for pending in cache.glob(".pending-*"):
            if pending.is_dir() and not pending.is_symlink():
                remove(pending)
        entry = cache / args["key"]
        if entry.exists():
            try:
                verified(entry, args["key"])
                return {"state": "published"}
            except (OSError, ValueError, KeyError, TypeError):
                remove(entry)
        pending = Path(tempfile.mkdtemp(prefix=".pending-", dir=cache))
        roots = []
        total = count = 0
        try:
            choices = ROOTS + ([("workspace", "node_modules")] if args["dependencies"] else [])
            for scope, path in choices:
                source = Path(args[scope]) / path
                if not source.exists() and not source.is_symlink():
                    continue
                tree = scan(source, wheel_only=path == ".cache/pip/wheels", complete=scope == "workspace")
                total += tree["bytes"]
                count += len(tree["items"])
                if total > MAX_BYTES or count > MAX_FILES:
                    raise ValueError("Prepared assets exceed cache limits")
                copy_tree(source, pending / str(len(roots)), tree)
                roots.append({"scope": scope, "path": path, "tree": tree})
            if not roots:
                return {"state": "miss"}
            (pending / "manifest.json").write_text(json.dumps({"version": 1, "key": args["key"], "roots": roots}))
            freeze(pending)
            verified(pending, args["key"])
            os.rename(pending, entry)
            entries = sorted((path for path in cache.iterdir() if len(path.name) == 40
                              and all(char in "0123456789abcdef" for char in path.name)
                              and path.is_dir() and not path.is_symlink()), key=lambda path: path.stat().st_mtime)
            for old in entries[:-MAX_ENTRIES]:
                remove(old)
            return {"state": "published"}
        finally:
            if pending.exists():
                remove(pending)


def restore(args):
    cache = Path(args["cache"])
    with locked_cache(cache):
        pass
    entry = cache / args["key"]
    if not entry.exists():
        return {"state": "miss"}
    roots = verified(entry, args["key"])
    copied = 0
    dependencies = False
    for index, root in enumerate(roots):
        if root["scope"] == "workspace" and not args["dependencies"]:
            continue
        target = Path(args[root["scope"]]) / root["path"]
        if target.exists() or target.is_symlink():
            continue
        base = open_path(Path(args[root["scope"]]), os.O_RDONLY | os.O_DIRECTORY)
        try:
            parent = directory(base, PurePosixPath(root["path"]).parent, create=True)
        finally:
            os.close(base)
        with locked_cache(cache):
            stage_path = Path(tempfile.mkdtemp(prefix=".restore-", dir=cache))
            stage = open_path(stage_path, os.O_RDONLY | os.O_DIRECTORY)
            lease = os.open(".lease", os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=stage)
            fcntl.flock(lease, fcntl.LOCK_EX)
        try:
            os.mkdir("assets", 0o700, dir_fd=stage)
            assets = os.open("assets", os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=stage)
            try:
                copy_to_fd(entry / str(index), assets, root["tree"])
            finally:
                os.close(assets)
            # The parent is pinned by descriptor, so replacing a parent with a link cannot redirect writes.
            libc = ctypes.CDLL(None, use_errno=True)
            function, flag = (libc.renameatx_np, 4) if sys.platform == "darwin" else (libc.renameat2, 1)
            result = function(stage, b"assets", parent, os.fsencode(target.name), flag)
            if result != 0 and ctypes.get_errno() != errno.EEXIST:
                raise OSError(ctypes.get_errno(), "Private asset restore failed")
            if result == 0:
                copied += root["tree"]["bytes"]
                dependencies |= root["scope"] == "workspace"
        finally:
            os.close(stage)
            try:
                remove(stage_path)
            finally:
                os.close(lease)
                os.close(parent)
    return {"state": "restored", "bytes": copied, "dependencies": dependencies}


def run(args):
    if len(args["key"]) != 40 or any(char not in "0123456789abcdef" for char in args["key"]):
        raise ValueError("Environment cache key is invalid")
    return publish(args) if args["action"] == "publish" else restore(args)


if __name__ == "__main__":
    arguments = json.loads(sys.argv[1])
    try:
        result = run(arguments)
    except (OSError, ValueError, KeyError, TypeError) as error:
        result = {"state": "rejected", "reason": str(error)}
    Path(arguments["result"]).write_text(json.dumps(result))
