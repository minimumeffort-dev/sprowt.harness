import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("check_skills", Path(__file__).with_name("check_skills.py"))
skills = importlib.util.module_from_spec(spec)
spec.loader.exec_module(skills)


class SkillPins(unittest.TestCase):
    def test_checked_in_bundle_matches_pins(self):
        self.assertGreater(skills.verify(), 0)

    def test_changed_missing_and_extra_files_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            package = root / "upstream" / "fixture"
            package.mkdir(parents=True)
            (package / "LICENSE").write_text("original")
            manifest = {"packages": [{"directory": "fixture", "license": "LICENSE", "files": {
                "LICENSE": {"sha256": "0682c5f2076f099c34cfdd15a9e063849ed437a49677e6fcc5b4198c76575be5"}
            }}]}
            (root / "manifest.json").write_text(json.dumps(manifest))
            self.assertEqual(skills.verify(root), 1)
            (package / "LICENSE").write_text("modified")
            with self.assertRaisesRegex(ValueError, "differs"):
                skills.verify(root)
            (package / "LICENSE").unlink()
            with self.assertRaisesRegex(ValueError, "missing"):
                skills.verify(root)
            (package / "extra").write_text("unexpected")
            with self.assertRaisesRegex(ValueError, "extra"):
                skills.verify(root)
