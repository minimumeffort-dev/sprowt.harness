"""The tested Muse executables, kept separate from the standalone auto-updater."""

from contextlib import contextmanager
from dataclasses import dataclass
import fcntl
import hashlib
import os
from pathlib import Path
import platform
import secrets
import shutil
import time
import urllib.error
import urllib.parse
import urllib.request

VERSION = "1.4.7-R6101.1"
DOWNLOAD_SECONDS = 600


@dataclass(frozen=True)
class Artifact:
    file: str
    checksum: str
    size: int

    @property
    def url(self):
        query = urllib.parse.urlencode({"channel": "muse", "version": VERSION, "file": self.file})
        return "https://lookaside.facebook.com/lookaside/muse/download/?" + query


# SHA-256 and sizes from this release's official manifest, not the mutable channel.
ARTIFACTS = {
    "aarch64_macos": Artifact("muse-aarch64-macos", "87ff5a31b63e30b6ac04d3e8298285517e753c126fb6f3aee80accf28c4562f1", 358892272),
    "aarch64_linux": Artifact("muse-aarch64-linux", "48db3c7468cd4f401eff9b415fb7e0cdfe0351fb1730649ceae2a0f766e23b85", 325916776),
}
CHECKSUM = ARTIFACTS["aarch64_linux"].checksum


class ReleaseError(RuntimeError):
    """Fixed, credential-free recovery instructions safe to show at startup."""


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ReleaseError("Muse download redirected unexpectedly. Update Sprowt and retry.")


def cache_root():
    value = os.environ.get("SPROWT_MUSE_CACHE")
    return Path(value) if value else Path.home() / "Library/Caches/sprowt-harness/muse" / VERSION


def native_platform():
    if platform.system() != "Darwin" or platform.machine() not in ("arm64", "aarch64"):
        raise ReleaseError("Sprowt's tested Muse release requires an Apple silicon Mac.")
    return "aarch64_macos"


def binary_path(cache, key):
    return cache / (("muse-host-" if key == "aarch64_macos" else "muse-") + VERSION)


def verified(path, item):
    try:
        if path.is_symlink() or not path.is_file() or path.stat().st_size != item.size:
            return False
        digest = hashlib.sha256()
        with path.open("rb") as source:
            while chunk := source.read(1024 * 1024):
                digest.update(chunk)
        return digest.hexdigest() == item.checksum
    except OSError:
        return False


def remaining(deadline):
    seconds = deadline - time.monotonic()
    if seconds <= 0:
        raise ReleaseError("Muse preparation timed out. Restart Sprowt to retry; verified downloads are retained.")
    return seconds


@contextmanager
def release_lock(cache, deadline):
    cache.mkdir(parents=True, exist_ok=True, mode=0o700)
    cache.chmod(0o700)
    with (cache / ".release.lock").open("a") as lock:
        os.chmod(lock.name, 0o600)
        while True:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                time.sleep(min(0.1, remaining(deadline)))
        try:
            yield
        finally:
            fcntl.flock(lock, fcntl.LOCK_UN)


def ensure(cache, key, candidate=None, deadline=None):
    deadline = deadline if deadline is not None else time.monotonic() + DOWNLOAD_SECONDS
    item = ARTIFACTS[key]
    path = binary_path(cache, key)
    with release_lock(cache, deadline):
        if verified(path, item):
            path.chmod(0o700)
            return path
        # A terminated preparation can leave a staging file; no writer survives this lock.
        for stale in cache.glob(".muse-next-*"):
            stale.unlink(missing_ok=True)
        next_path = cache / (".muse-next-" + secrets.token_hex(8))
        try:
            if candidate is not None and verified(candidate, item):
                with candidate.open("rb") as source, next_path.open("xb") as output:
                    next_path.chmod(0o600)
                    while chunk := source.read(1024 * 1024):
                        remaining(deadline)
                        output.write(chunk)
            else:
                opener = urllib.request.build_opener(NoRedirect())
                with opener.open(item.url, timeout=min(60, remaining(deadline))) as source, next_path.open("xb") as output:
                    next_path.chmod(0o600)
                    size = 0
                    while chunk := source.read(1024 * 1024):
                        remaining(deadline)
                        size += len(chunk)
                        if size > item.size:
                            raise ReleaseError("Muse download exceeded its expected size. Restart Sprowt to retry.")
                        output.write(chunk)
            if not verified(next_path, item):
                raise ReleaseError("Muse download failed its checksum or size check. Restart Sprowt to retry.")
            next_path.chmod(0o700)
            next_path.replace(path)
        except (OSError, urllib.error.URLError) as error:
            raise ReleaseError("Could not prepare the tested Muse release. Check your connection or local cache access and restart Sprowt; verified downloads are retained.") from error
        finally:
            next_path.unlink(missing_ok=True)
        return path


def native(cache=None, deadline=None):
    cache = cache if cache is not None else cache_root()
    launcher = shutil.which("muse")
    candidate = Path(launcher).resolve().parent / ("muse-bin-" + VERSION) if launcher else None
    return ensure(cache, native_platform(), candidate, deadline)


def artifact(cache):
    return ensure(cache, "aarch64_linux")


def prepare(cache):
    if not shutil.which("muse") and not binary_path(cache, "aarch64_macos").exists():
        return False
    deadline = time.monotonic() + DOWNLOAD_SECONDS
    native(cache, deadline)
    ensure(cache, "aarch64_linux", deadline=deadline)
    return True
