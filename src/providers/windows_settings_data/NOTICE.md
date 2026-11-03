# PowerToys Command Palette Windows Settings data

The catalog, schema, neutral RESX and localized resource strings come from
Microsoft PowerToys **Command Palette**. Current catalog snapshot:

- Release: [v0.102.3001](https://github.com/microsoft/PowerToys/releases/tag/v0.102.3001).
- Revision: `a00b2bce7403f0a48efe5cbb6671074e01f27824`.
- Checked/updated: 2026-11-01; 306 catalog entries.
- Catalog, schema, English RESX and license match the previous snapshot;
  only provenance needed updating. Their downloaded byte hashes are recorded
  in `SOURCE.json` under `catalog`.

The verified original localization import is retained separately:

- Release: [v0.101.2362.0](https://github.com/microsoft/PowerToys/releases/tag/v0.101.2362.0), published 2026-08-25.
- Revision: `389b1a3827b2cf577723288df15437490ef0663a`.
- Source: [Microsoft.CmdPal.Ext.WindowsSettings](https://github.com/microsoft/PowerToys/tree/389b1a3827b2cf577723288df15437490ef0663a/src/modules/cmdpal/ext/Microsoft.CmdPal.Ext.WindowsSettings).
- Imported: 2026-10-07. License: MIT, see `PowerToys-LICENSE`.
- Machine-readable localization provenance: `SOURCE.json` / `localizations`.

`WindowsSettings.json` contains 306 candidate entries. English resources are
copied from upstream `Properties/Resources.resx`. Published translations are
supplied by Microsoft's localization pipeline and are not committed alongside
that RESX. Simplified Chinese strings were extracted, without loading or
executing code, from managed `.resources` streams embedded in the official
release's NativeAOT `Microsoft.CmdPal.UI.exe`, packaged inside the Command
Palette MSIX. All 543 resource keys match the neutral RESX; the binary's neutral
resources also match all 543 English values exactly. The binary SHA-256 and
verified Chinese stream offset are recorded in `SOURCE.json`.

`Resources.en.json` and `Resources.zh-CN.json` preserve original resource
values. Catalog-used keys are imported into the project's normal `lang/*/`
PO files with context `Windows Settings` and upstream resource identifiers
as message IDs. Runtime translation uses the existing project i18n service.
There are no Wowie-authored Chinese labels or Microsoft Learn overrides.
Search aliases retain both supported languages. For example, upstream's
Simplified Chinese Wi-Fi title is `WLAN`.

For an automatic source-only refresh, run:

```powershell
python -B tools/update_windows_settings.py --ref latest --check
pwsh -NoProfile -File tools/build.ps1 -UpdateWindowsSettings
```

The optional build step resolves the latest stable tag to a commit, validates
the catalog and reuses localized keys only if their English source is unchanged.
New/changed used resource keys stop the update without substituting translations.
Plain Cargo builds use the checked-in snapshot and do not access GitHub.
`--check` exits 0 when current, 1 when changes are available, 2 on failure.
Without `--ref`, the updater re-fetches the pinned commit rather than latest.

When translations need refreshing, obtain the source directory and matching official release binary,
verify each locale stream (offsets change between releases), then run:

```powershell
python tools/import_powertoys_windows_settings.py --source <source-directory> --binary <Microsoft.CmdPal.UI.exe> --locale-offset zh-CN=21295979 --revision 389b1a3827b2cf577723288df15437490ef0663a --version v0.101.2362.0
```

Like Command Palette's `ControlPanelTasksHelper`, the Windows adapter also
enumerates the Shell **All Tasks** folder
(`shell:::{ED7BA470-8E54-465E-825C-99712043E01C}`). Task titles, application areas and
keywords are localized by Windows. Tasks retain their original PIDLs for
launching and icons because their parsing names cannot reliably be re-parsed.
Classic installed Control Panel applets remain discovered through Shell.
Dynamic task strings follow the Windows display language, as upstream does;
changing Wowie's UI language does not translate those OS-provided strings.

The catalog supplies candidate pages, filtered by OS build, edition, explicit
hardware exclusions and SettingsPageVisibility policies. The Settings host
must be installed. Undocumented Settings XML and SystemSettings.dll strings
are not complete page lists and are deliberately not used as whitelists.
Unknown build/device probes do not establish incompatibility and therefore
do not remove candidates; dynamically gated pages can still redirect to a
parent page. MMC entries require actual System32 files, and their icon DLL
and index come from each console's VisualAttributes metadata. Applets and
tasks keep captured absolute PIDLs as icon sources. Missing native icons
fall back to bundled category SVGs. No Settings DLL code is loaded for page
discovery. Duplicate destinations merge aliases while retaining the first
record's identity, with Shell records taking precedence over catalog records.

See [Windows settings maintenance](../../../docs/Windows设置索引.md) for
the official discovery paths, build-time update command, offline source mode,
translation boundary and focused verification commands.

Availability corrections use Microsoft's [Settings URI reference](https://learn.microsoft.com/en-us/windows/apps/develop/launch/launch-settings),
[Windows 11 24H2 removed features](https://learn.microsoft.com/en-us/windows/whats-new/whats-new-windows-11-version-24h2)
and [Cortana end-of-support notice](https://support.microsoft.com/en-us/cortana/end-of-support-for-cortana).
