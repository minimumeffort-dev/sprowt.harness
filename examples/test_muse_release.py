"""Release preparation through its filesystem and download boundaries; no model calls."""

import fcntl
import hashlib
import io
import multiprocessing
import os
from pathlib import Path
import tempfile
import time
import unittest
from unittest.mock import patch
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))
import muse_release as release


HOST = b"tested native executable\n"
LINUX = b"tested guest executable\n"


def item(name, data):
    return release.Artifact(name, hashlib.sha256(data).hexdigest(), len(data))


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.cache = self.root / "cache"
        self.items = {"aarch64_macos": item("muse-aarch64-macos", HOST),
                      "aarch64_linux": item("muse-aarch64-linux", LINUX)}
        for target, value in [("ARTIFACTS", self.items), ("platform.system", lambda: "Darwin"),
                              ("platform.machine", lambda: "arm64")]:
            mock = patch("muse_release." + target, value)
            mock.start()
            self.addCleanup(mock.stop)
        self.launcher = self.root / "standalone/muse"
        self.launcher.parent.mkdir()
        self.launcher.write_text("Standalone launcher must never be executed\n")
        self.installed = self.launcher.parent / ("muse-bin-" + release.VERSION)
        self.installed.write_bytes(HOST)
        self.launcher_before = self.launcher.read_bytes()

    def downloads(self, data=LINUX):
        class Opener:
            def open(self, url, timeout):
                return io.BytesIO(data)
        return patch("muse_release.urllib.request.build_opener", return_value=Opener())

    def test_existing_native_is_copied_and_guest_is_verified_without_running_updater(self):
        with patch("muse_release.shutil.which", return_value=str(self.launcher)), self.downloads() as download:
            self.assertTrue(release.prepare(self.cache))
        self.assertEqual(release.binary_path(self.cache, "aarch64_macos").read_bytes(), HOST)
        self.assertEqual(release.artifact(self.cache).read_bytes(), LINUX)
        self.assertEqual(download.call_count, 1)
        self.assertEqual(self.launcher.read_bytes(), self.launcher_before)
        self.assertEqual(self.installed.read_bytes(), HOST)
        self.assertEqual(self.cache.stat().st_mode & 0o777, 0o700)
        self.assertEqual(release.binary_path(self.cache, "aarch64_macos").stat().st_mode & 0o777, 0o700)

    def test_verified_private_release_works_offline_even_without_standalone_cli(self):
        with patch("muse_release.shutil.which", return_value=str(self.launcher)), self.downloads():
            release.prepare(self.cache)
        with patch("muse_release.shutil.which", return_value=None), \
                patch("muse_release.urllib.request.build_opener", side_effect=AssertionError("Must reuse cache")):
            self.assertTrue(release.prepare(self.cache))
            self.assertEqual(release.native(self.cache).read_bytes(), HOST)
            self.assertEqual(release.artifact(self.cache).read_bytes(), LINUX)

    def test_missing_cli_and_private_release_keep_codex_only(self):
        with patch("muse_release.shutil.which", return_value=None), \
                patch("muse_release.urllib.request.build_opener", side_effect=AssertionError("Muse is optional")):
            self.assertFalse(release.prepare(self.cache))
        self.assertFalse(self.cache.exists())

    def test_older_and_future_standalone_versions_do_not_choose_the_artifact(self):
        self.installed.unlink()
        for version in ["1.4.3-R5018.1", "99.0.0-R9999.1"]:
            with self.subTest(version=version):
                (self.launcher.parent / ("muse-bin-" + version)).write_bytes(b"unsupported")
                self.launcher.write_text("reported version " + version)
                cache = self.root / version
                requests = []

                class Opener:
                    def open(inner, url, timeout):
                        requests.append(url)
                        return io.BytesIO(HOST if "macos" in url else LINUX)

                with patch("muse_release.shutil.which", return_value=str(self.launcher)), \
                        patch("muse_release.urllib.request.build_opener", return_value=Opener()):
                    self.assertTrue(release.prepare(cache))
                self.assertEqual(release.native(cache).read_bytes(), HOST)
                self.assertEqual(requests, [self.items[key].url for key in ("aarch64_macos", "aarch64_linux")])
                self.assertEqual(self.launcher.read_text(), "reported version " + version)

    def test_corrupt_download_never_replaces_cache_and_can_be_retried(self):
        self.cache.mkdir()
        destination = release.binary_path(self.cache, "aarch64_linux")
        destination.write_bytes(b"old corrupt cache")
        with self.downloads(b"unverified"), self.assertRaisesRegex(release.ReleaseError, "checksum or size"):
            release.artifact(self.cache)
        self.assertEqual(destination.read_bytes(), b"old corrupt cache")
        self.assertEqual(list(self.cache.glob(".muse-next-*")), [])
        with self.downloads():
            self.assertEqual(release.artifact(self.cache).read_bytes(), LINUX)

    def test_interrupted_download_preserves_host_and_withholds_partial_guest(self):
        class Interrupted(io.BytesIO):
            def read(self, count=-1):
                if self.tell():
                    raise OSError("https://credential.invalid/?token=synthetic-secret")
                return super().read(3)

        class Opener:
            def open(self, url, timeout):
                return Interrupted(LINUX)

        with patch("muse_release.shutil.which", return_value=str(self.launcher)), \
                patch("muse_release.urllib.request.build_opener", return_value=Opener()):
            with self.assertRaisesRegex(release.ReleaseError, "connection or local cache access") as error:
                release.prepare(self.cache)
        self.assertNotIn("synthetic-secret", str(error.exception))
        self.assertEqual(release.binary_path(self.cache, "aarch64_macos").read_bytes(), HOST)
        self.assertFalse(release.binary_path(self.cache, "aarch64_linux").exists())
        self.assertEqual(list(self.cache.glob(".muse-next-*")), [])
        with patch("muse_release.shutil.which", return_value=None), self.downloads():
            self.assertTrue(release.prepare(self.cache))

    def test_oversized_download_is_stopped_before_publication(self):
        with self.downloads(LINUX + b"extra"), self.assertRaisesRegex(release.ReleaseError, "expected size"):
            release.artifact(self.cache)
        self.assertFalse(release.binary_path(self.cache, "aarch64_linux").exists())

    def test_redirects_are_rejected_without_following_another_origin(self):
        request = release.urllib.request.Request(self.items["aarch64_linux"].url)
        with self.assertRaisesRegex(release.ReleaseError, "redirected unexpectedly"):
            release.NoRedirect().redirect_request(request, None, 302, "Moved", {}, "https://attacker.invalid")

    def test_checksum_matched_symlink_is_not_reused_or_chmodded(self):
        self.cache.mkdir()
        source = self.root / "unmanaged"
        source.write_bytes(LINUX)
        source.chmod(0o644)
        destination = release.binary_path(self.cache, "aarch64_linux")
        destination.symlink_to(source)
        with self.downloads():
            self.assertEqual(release.artifact(self.cache).read_bytes(), LINUX)
        self.assertFalse(destination.is_symlink())
        self.assertEqual(source.stat().st_mode & 0o777, 0o644)

    def test_concurrent_preparation_publishes_one_complete_download(self):
        context = multiprocessing.get_context("fork")
        opens = context.Value("i", 0)
        result = context.Queue()

        class Slow(io.BytesIO):
            def read(self, count=-1):
                time.sleep(0.02)
                return super().read(count)

        class Opener:
            def open(self, url, timeout):
                with opens.get_lock():
                    opens.value += 1
                return Slow(LINUX)

        def worker():
            with patch("muse_release.urllib.request.build_opener", return_value=Opener()):
                result.put(release.artifact(self.cache).read_bytes())

        workers = [context.Process(target=worker) for _ in range(2)]
        for worker in workers:
            worker.start()
            self.addCleanup(lambda p=worker: p.kill() if p.is_alive() else None)
        for worker in workers:
            worker.join(3)
            self.assertEqual(worker.exitcode, 0)
        self.assertEqual([result.get(timeout=1) for _ in workers], [LINUX, LINUX])
        self.assertEqual(opens.value, 1)
        self.assertEqual(list(self.cache.glob(".muse-next-*")), [])
        result.close()

    def test_killed_download_releases_lock_and_stale_stage_is_discarded(self):
        context = multiprocessing.get_context("fork")
        waiting = context.Event()

        class Paused(io.BytesIO):
            def read(self, count=-1):
                if self.tell():
                    waiting.set()
                    time.sleep(30)
                return super().read(3)

        class Opener:
            def open(self, url, timeout):
                return Paused(LINUX)

        def worker():
            with patch("muse_release.urllib.request.build_opener", return_value=Opener()):
                release.artifact(self.cache)

        worker = context.Process(target=worker)
        worker.start()
        self.addCleanup(lambda: worker.kill() if worker.is_alive() else None)
        self.assertTrue(waiting.wait(3))
        worker.kill()
        worker.join(3)
        with self.downloads():
            self.assertEqual(release.artifact(self.cache).read_bytes(), LINUX)
        self.assertEqual(list(self.cache.glob(".muse-next-*")), [])

    def test_lock_wait_has_a_deadline(self):
        self.cache.mkdir()
        with (self.cache / ".release.lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            with patch("muse_release.DOWNLOAD_SECONDS", 0.02), \
                    self.assertRaisesRegex(release.ReleaseError, "timed out"):
                release.artifact(self.cache)


if __name__ == "__main__":
    unittest.main()
