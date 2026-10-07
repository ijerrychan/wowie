"""Import Command Palette's catalog and original localized resource strings.

Reads NativeAOT .resources streams as data; never loads the release executable.
Locale offsets are explicit because NativeAOT's manifest format is not a public
API. Verify the locale of each selected stream before assigning its language.
"""

import argparse
import hashlib
import json
from pathlib import Path
import struct
import xml.etree.ElementTree as ET


def read_resources(binary, start):
    def uint(position):
        return struct.unpack_from("<I", binary, position)[0]

    def length(position):
        value = 0
        for shift in range(0, 35, 7):
            byte = binary[position]
            position += 1
            value |= (byte & 127) << shift
            if byte < 128:
                return value, position
        raise ValueError("Invalid resource length")

    def string(position, encoding):
        size, position = length(position)
        if size > 1_000_000 or position + size > len(binary):
            raise ValueError("Resource string exceeds stream bounds")
        return binary[position:position + size].decode(encoding), position + size

    if uint(start) != 0xBEEFCACE:
        raise ValueError("Not a managed resource stream")
    position = start + 12 + uint(start + 8)
    if uint(position) != 2:
        raise ValueError("Unsupported .resources version")
    count, types = uint(position + 4), uint(position + 8)
    if count > 10_000 or types > 100:
        raise ValueError("Unexpected resource table size")
    position += 12
    for _ in range(types):
        _, position = string(position, "utf-8")
    position = start + ((position - start + 7) // 8) * 8 + 4 * count
    name_positions = [uint(position + 4 * index) for index in range(count)]
    position += 4 * count
    data_section, name_section = start + uint(position), position + 4
    result = {}
    for offset in name_positions:
        name, position = string(name_section + offset, "utf-16-le")
        kind, position = length(data_section + uint(position))
        if kind == 1:  # ResourceTypeCode.String
            result[name], _ = string(position, "utf-8")
    return result


def po_string(value):
    return json.dumps(value, ensure_ascii=False)


def write_po(path, resources):
    # Keep every unrelated project translation verbatim.
    text = path.read_text(encoding="utf-8")
    generated_note = "# Generated from Microsoft Command Palette resources; see windows_settings_data/NOTICE.md.\n"
    text = text.replace(generated_note, "")
    lines = text.splitlines(keepends=True)
    output = []
    skip = False
    for line in lines:
        if line.startswith("msgctxt "):
            skip = line.strip() == 'msgctxt "Windows Settings"'
        if not skip:
            output.append(line)
    entries = [generated_note]
    for key, value in sorted(resources.items()):
        entries.append(f'msgctxt "Windows Settings"\nmsgid {po_string(key)}\nmsgstr {po_string(value)}\n\n')
    path.write_text("".join(output).rstrip() + "\n\n" + "".join(entries).rstrip() + "\n", encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True, help="Command Palette WindowsSettings source directory")
    parser.add_argument("--binary", type=Path, required=True, help="Matching Microsoft.CmdPal.UI.exe from the official release")
    parser.add_argument("--locale-offset", action="append", default=[], help="Verified language=resource stream offset; repeat for each supported language")
    parser.add_argument("--revision", required=True)
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    destination = root / "src/providers/windows_settings_data"
    english = {entry.attrib["name"]: entry.findtext("value", "")
               for entry in ET.parse(args.source / "Properties/Resources.resx").getroot().findall("data")}
    binary = args.binary.read_bytes()
    resources = {"en": english}
    offsets = {}
    project_locales = {directory.name for directory in (root / "lang").iterdir() if directory.is_dir()}
    for specification in args.locale_offset:
        locale, offset = specification.split("=", 1)
        if locale not in project_locales or locale in resources:
            raise ValueError(f"Unsupported or duplicate neutral locale: {locale}")
        offsets[locale] = int(offset)
        resources[locale] = read_resources(binary, int(offset))
        if resources[locale].keys() != english.keys():
            raise ValueError(f"{locale} resource keys do not match the pinned neutral RESX")
    if missing := project_locales - resources.keys():
        raise ValueError(f"Missing original resources for project languages: {sorted(missing)}")
    # Confirm the release binary contains exactly this revision's neutral
    # resource set before accepting any localized stream from that binary.
    cursor = 0
    neutral_found = False
    while (offset := binary.find(bytes.fromhex("cecaefbe"), cursor)) >= 0:
        cursor = offset + 4
        try:
            if read_resources(binary, offset) == english:
                neutral_found = True
                break
        except (ValueError, IndexError, struct.error, UnicodeError):
            continue
    if not neutral_found:
        raise ValueError("Release binary neutral resources differ from source RESX")
    catalog = json.loads((args.source / "WindowsSettings.json").read_text(encoding="utf-8-sig"))
    required = set()
    for setting in catalog["Settings"]:
        required.update([setting["Name"], setting["Type"], *setting.get("Areas", []), *setting.get("AltNames", [])])
        if setting.get("Note"):
            required.add(setting["Note"])
    if missing := required - english.keys():
        raise ValueError(f"Catalog references missing upstream resources: {sorted(missing)}")
    for name in ["WindowsSettings.json", "WindowsSettings.schema.json"]:
        (destination / name).write_bytes((args.source / name).read_bytes())
    (destination / "Resources.en.resx").write_bytes((args.source / "Properties/Resources.resx").read_bytes())
    for locale, strings in resources.items():
        (destination / f"Resources.{locale}.json").write_text(json.dumps(strings, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        write_po(root / "lang" / locale / "LC_MESSAGES/wowie.po", {key: strings[key] for key in required})
    provenance = {"release": args.version, "revision": args.revision,
                  "binary_sha256": hashlib.sha256(binary).hexdigest(), "locale_stream_offsets": offsets,
                  "neutral_resource_count": len(english), "catalog_count": len(catalog["Settings"])}
    (destination / "SOURCE.json").write_text(json.dumps(provenance, indent=2) + "\n", encoding="utf-8")
    print(f"Imported {len(catalog['Settings'])} Command Palette entries and {len(english)} original resources per locale.")


if __name__ == "__main__":
    main()
