"""Update the vendored PowerToys catalog before a build (Python 3.10+).

Default: re-fetch the pinned revision. --ref latest explicitly follows the
latest stable release. No binaries are downloaded or executed. Existing
localized strings are reused only when their English source is unchanged.
"""

import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import re
import sys
import tempfile
from urllib.parse import quote
from urllib.request import Request, urlopen
import xml.etree.ElementTree as ET

sys.dont_write_bytecode = True
from import_powertoys_windows_settings import render_po

ROOT = Path(__file__).resolve().parent.parent
DATA = Path("src/providers/windows_settings_data")
UPSTREAM = "src/modules/cmdpal/ext/Microsoft.CmdPal.Ext.WindowsSettings"
FILES = {
    "WindowsSettings.json": f"{UPSTREAM}/WindowsSettings.json",
    "WindowsSettings.schema.json": f"{UPSTREAM}/WindowsSettings.schema.json",
    "Resources.en.resx": f"{UPSTREAM}/Properties/Resources.resx",
    "PowerToys-LICENSE": "LICENSE",
}


def download(url):
    request = Request(url, headers={"User-Agent": "wowie-settings-maintenance",
                                    "Accept": "application/vnd.github+json"})
    with urlopen(request, timeout=30) as response:
        data = response.read(2 * 1024 * 1024 + 1)
    if len(data) > 2 * 1024 * 1024:
        raise ValueError(f"Upstream file exceeds 2 MiB: {url}")
    return data


def api(path):
    return json.loads(download(f"https://api.github.com/repos/microsoft/PowerToys/{path}"))


def provenance_parts(source):
    if "catalog" in source:
        return source["catalog"], source["localizations"]
    # Migrate the original all-in-one provenance without relabeling old
    # Chinese resources as translations from the new release.
    return source, {key: source[key] for key in
                    ("release", "revision", "binary_sha256", "locale_stream_offsets", "neutral_resource_count")}


def resolve_ref(reference, pinned):
    reference = reference or pinned["revision"]
    if reference == "latest":
        release = api("releases/latest")
        if release.get("draft") or release.get("prerelease"):
            raise ValueError("Expected a stable public PowerToys release")
        reference = release["tag_name"]
    revision = reference if re.fullmatch(r"[0-9a-fA-F]{40}", reference) else api(
        f"commits/{quote(reference, safe='')}"
    )["sha"]
    if not re.fullmatch(r"[0-9a-fA-F]{40}", revision):
        raise ValueError("GitHub did not return an immutable commit SHA")
    label = pinned["release"] if revision == pinned["revision"] else reference
    return label, revision


def validate_catalog(catalog, english):
    settings = catalog.get("Settings")
    if not isinstance(settings, list) or not settings:
        raise ValueError("Upstream Settings must be a nonempty list")
    required = set()
    for entry in settings:
        if not isinstance(entry, dict) or not isinstance(entry.get("Name"), str) or not entry["Name"]:
            raise ValueError("Every setting requires a resource Name")
        kind, command = entry["Type"], entry["Command"]
        if kind not in {"AppSettingsApp", "AppControlPanel", "AppMMC"}:
            raise ValueError(f"Unsupported catalog type: {kind}")
        if not isinstance(command, str) or not command.strip() or "\x00" in command:
            raise ValueError("Invalid catalog command")
        if kind == "AppSettingsApp" and not (
            re.fullmatch(r"ms-settings:[a-zA-Z0-9-]*", command)
            or command == "ms-settings-connectabledevices:devicediscovery"
        ):
            raise ValueError(f"Unsupported Settings URI format: {command}")
        for field in ("IntroducedInBuild", "DeprecatedInBuild"):
            value = entry.get(field)
            if value is not None and (type(value) is not int or not 0 <= value <= 0xFFFFFFFF):
                raise ValueError(f"Invalid {field}: {value}")
        for field in ("Areas", "AltNames"):
            values = entry.get(field, [])
            if not isinstance(values, list) or any(not isinstance(value, str) for value in values):
                raise ValueError(f"Invalid {field} resource list")
        if entry.get("Note") is not None and not isinstance(entry["Note"], str):
            raise ValueError("Invalid Note resource")
        score = entry.get("AppHomepageScore", 0)
        if type(score) is not int or not -(2 ** 31) <= score < 2 ** 31:
            raise ValueError("Invalid AppHomepageScore")
        required.update([entry["Name"], kind, *entry.get("Areas", []), *entry.get("AltNames", [])])
        if entry.get("Note"):
            required.add(entry["Note"])
    if missing := required - english.keys():
        raise ValueError(f"Catalog references missing English resources: {sorted(missing)}")
    return required


def prepare_update(root, files, release, revision):
    if files.keys() != FILES.keys():
        raise ValueError("Incomplete upstream source snapshot")
    if b"Permission is hereby granted" not in files["PowerToys-LICENSE"]:
        raise ValueError("Expected upstream MIT license")
    destination = root / DATA
    catalog = json.loads(files["WindowsSettings.json"].decode("utf-8-sig"))
    schema = json.loads(files["WindowsSettings.schema.json"].decode("utf-8-sig"))
    if not isinstance(schema, dict) or "Settings" not in schema.get("properties", {}):
        raise ValueError("Unexpected upstream catalog schema")
    english = {entry.attrib["name"]: entry.findtext("value", "") for entry in
               ET.fromstring(files["Resources.en.resx"]).findall("data")}
    required = validate_catalog(catalog, english)
    old_english = json.loads((destination / "Resources.en.json").read_text(encoding="utf-8"))
    source = json.loads((destination / "SOURCE.json").read_text(encoding="utf-8"))
    _, localizations = provenance_parts(source)
    resources = {"en": english}
    for locale_dir in sorted((root / "lang").iterdir()):
        if not locale_dir.is_dir() or locale_dir.name == "en":
            continue
        locale = locale_dir.name
        resource_path = destination / f"Resources.{locale}.json"
        strings = json.loads(resource_path.read_text(encoding="utf-8")) if resource_path.is_file() else {}
        invalid = sorted(key for key in required
                         if not strings.get(key) or old_english.get(key) != english[key])
        if invalid:
            raise ValueError(
                f"{locale} needs verified upstream translations for {invalid}. "
                "Run import_powertoys_windows_settings.py with matching release resources first; "
                "no vendored files were changed."
            )
        resources[locale] = strings
    result = {destination / name: data for name, data in files.items()}
    for locale, strings in resources.items():
        result[destination / f"Resources.{locale}.json"] = (
            json.dumps(strings, ensure_ascii=False, indent=2) + "\n").encode("utf-8")
        po = root / "lang" / locale / "LC_MESSAGES/wowie.po"
        result[po] = render_po(po.read_text(encoding="utf-8"),
                               {key: strings[key] for key in sorted(required)}).encode("utf-8")
    provenance = {
        "catalog": {"release": release, "revision": revision,
                    "catalog_count": len(catalog["Settings"]), "neutral_resource_count": len(english),
                    "files_sha256": {name: hashlib.sha256(data).hexdigest() for name, data in files.items()}},
        "localizations": localizations,
    }
    result[destination / "SOURCE.json"] = (json.dumps(provenance, indent=2) + "\n").encode("utf-8")
    return {path: data for path, data in result.items()
            if path.read_bytes().replace(b"\r\n", b"\n") != data.replace(b"\r\n", b"\n")}


def replace_file(source, destination):
    if os.name != "nt":
        return os.replace(source, destination)
    # os.replace carries a tempfile directory's restrictive DACL into the
    # checkout. ReplaceFileW preserves the existing destination's DACL.
    import ctypes
    from ctypes import wintypes
    replace = ctypes.WinDLL("kernel32", use_last_error=True).ReplaceFileW
    replace.argtypes = [wintypes.LPCWSTR, wintypes.LPCWSTR, wintypes.LPCWSTR,
                        wintypes.DWORD, wintypes.LPVOID, wintypes.LPVOID]
    replace.restype = wintypes.BOOL
    if not replace(str(destination), str(source), None, 0, None, None):
        raise ctypes.WinError(ctypes.get_last_error())


def commit_update(root, changes):
    # Validate everything before staging; replace only known project files.
    # Keep backups so a filesystem error can roll back a partial replacement.
    (root / "target").mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="settings-update-", dir=root / "target") as temporary:
        stage = Path(temporary)
        written = []
        try:
            for index, (path, data) in enumerate(changes.items()):
                (stage / f"{index}.old").write_bytes(path.read_bytes())
                (stage / f"{index}.new").write_bytes(data)
            for index, path in enumerate(changes):
                replace_file(stage / f"{index}.new", path)
                written.append((index, path))
        except OSError:
            for index, path in reversed(written):
                replace_file(stage / f"{index}.old", path)
            raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ref", help="Release tag/commit; 'latest' explicitly selects latest stable")
    parser.add_argument("--check", action="store_true", help="Read-only comparison: exit 1 if updates exist, 2 on error")
    parser.add_argument("--source", type=Path, help="Offline PowerToys checkout instead of network downloads")
    parser.add_argument("--revision", help="Required immutable source commit with --source")
    args = parser.parse_args()
    try:
        source = json.loads((ROOT / DATA / "SOURCE.json").read_text(encoding="utf-8"))
        pinned, _ = provenance_parts(source)
        if args.source:
            if not args.revision or not re.fullmatch(r"[0-9a-fA-F]{40}", args.revision):
                raise ValueError("--source requires --revision with a 40-character commit SHA")
            if not args.ref or args.ref == "latest":
                raise ValueError("--source requires an explicit release tag/commit via --ref")
            release, revision = args.ref, args.revision
            files = {name: (args.source / path).read_bytes() for name, path in FILES.items()}
        else:
            if args.revision:
                raise ValueError("--revision is only supported with --source")
            release, revision = resolve_ref(args.ref, pinned)
            with ThreadPoolExecutor(max_workers=4) as executor:
                blobs = executor.map(download, [f"https://raw.githubusercontent.com/microsoft/PowerToys/{revision}/{path}"
                                                for path in FILES.values()])
                files = dict(zip(FILES, blobs))
        changes = prepare_update(ROOT, files, release, revision)
        print(f"PowerToys {release} ({revision}): {len(changes)} changed file(s)")
        for path in changes:
            print(f"  {path.relative_to(ROOT).as_posix()}")
        if args.check:
            return int(bool(changes))
        if changes:
            commit_update(ROOT, changes)
        return 0
    except (OSError, ValueError, KeyError, TypeError, ET.ParseError) as error:
        print(f"Settings update failed: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
