"""Offline regressions for the updater's data-loss and translation boundaries."""

import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import update_windows_settings as updater


class UpdateTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.data = self.root / updater.DATA
        self.data.mkdir(parents=True)
        english = {"Display": "Display", "AppSettingsApp": "Settings"}
        source = {"release": "old", "revision": "a" * 40, "binary_sha256": "old-hash",
                  "locale_stream_offsets": {"zh-CN": 123}, "neutral_resource_count": 2}
        (self.data / "SOURCE.json").write_text(json.dumps(source), encoding="utf-8")
        for locale in ["en", "zh-CN"]:
            (self.data / f"Resources.{locale}.json").write_text(json.dumps(english), encoding="utf-8")
            po = self.root / "lang" / locale / "LC_MESSAGES/wowie.po"
            po.parent.mkdir(parents=True)
            po.write_text('msgctxt "Other"\nmsgid "Keep"\nmsgstr "Keep me"\n\n', encoding="utf-8")
        catalog = {"Settings": [{"Name": "Display", "Type": "AppSettingsApp", "Command": "ms-settings:display"}]}
        self.files = {
            "WindowsSettings.json": json.dumps(catalog).encode(),
            "WindowsSettings.schema.json": b'{"properties":{"Settings":{}}}',
            "Resources.en.resx": b'<root><data name="Display"><value>Display</value></data>'
                                 b'<data name="AppSettingsApp"><value>Settings</value></data></root>',
            "PowerToys-LICENSE": b"Permission is hereby granted",
        }
        for name in self.files:
            (self.data / name).write_bytes(b"previous content")

    def test_update_preserves_unrelated_po_and_translation_provenance_and_is_repeatable(self):
        changes = updater.prepare_update(self.root, self.files, "new", "b" * 40)
        updater.commit_update(self.root, changes)
        source = json.loads((self.data / "SOURCE.json").read_text())
        self.assertEqual(source["catalog"]["revision"], "b" * 40)
        self.assertEqual(source["localizations"]["release"], "old")
        po = self.root / "lang/zh-CN/LC_MESSAGES/wowie.po"
        self.assertIn('msgctxt "Other"\nmsgid "Keep"\nmsgstr "Keep me"', po.read_text())
        self.assertEqual(updater.prepare_update(self.root, self.files, "new", "b" * 40), {})

    def test_new_or_changed_resource_is_rejected_before_any_write(self):
        before = {path: path.read_bytes() for path in self.root.rglob("*") if path.is_file()}
        for replacement in [b"New display meaning", b""]:
            files = dict(self.files)
            if replacement:
                files["Resources.en.resx"] = files["Resources.en.resx"].replace(b">Display<", b">" + replacement + b"<")
            else:
                catalog = json.loads(files["WindowsSettings.json"])
                catalog["Settings"][0]["AltNames"] = ["NewKey"]
                files["WindowsSettings.json"] = json.dumps(catalog).encode()
                files["Resources.en.resx"] = files["Resources.en.resx"].replace(b"</root>", b'<data name="NewKey"><value>New</value></data></root>')
            with self.assertRaisesRegex(ValueError, "needs verified upstream translations"):
                updater.prepare_update(self.root, files, "new", "b" * 40)
        self.assertEqual(before, {path: path.read_bytes() for path in self.root.rglob("*") if path.is_file()})

    def test_partial_replace_failure_rolls_back_written_files(self):
        changes = updater.prepare_update(self.root, self.files, "new", "b" * 40)
        before = {path: path.read_bytes() for path in changes}
        replace = updater.replace_file
        calls = 0

        def fail_second(source, destination):
            nonlocal calls
            calls += 1
            if calls == 2:
                raise OSError("simulated filesystem failure")
            return replace(source, destination)

        with patch.object(updater, "replace_file", side_effect=fail_second):
            with self.assertRaises(OSError):
                updater.commit_update(self.root, changes)
        self.assertEqual(before, {path: path.read_bytes() for path in changes})


if __name__ == "__main__":
    unittest.main()
