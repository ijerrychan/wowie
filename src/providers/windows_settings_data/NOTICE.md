# PowerToys Command Palette Windows Settings data

The catalog, schema, neutral RESX and localized resource strings come from
Microsoft PowerToys **Command Palette**, latest stable release at import time:

- Release: [v0.101.2362.0](https://github.com/microsoft/PowerToys/releases/tag/v0.101.2362.0), published 2026-08-25.
- Revision: `389b1a3827b2cf577723288df15437490ef0663a`.
- Source: [Microsoft.CmdPal.Ext.WindowsSettings](https://github.com/microsoft/PowerToys/tree/389b1a3827b2cf577723288df15437490ef0663a/src/modules/cmdpal/ext/Microsoft.CmdPal.Ext.WindowsSettings).
- Imported: 2026-10-07. License: MIT, see `PowerToys-LICENSE`.
- Machine-readable provenance: `SOURCE.json`.

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

To refresh, obtain the source directory and matching official release binary,
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

The candidate catalog is not proof of local availability. Wowie additionally
checks installed Settings XML policy IDs (or complete URI literals in the
installed SystemSettings.dll when XML provides no usable IDs), OS build,
edition, hardware prerequisites and SettingsPageVisibility policies. Unknown
prerequisites exclude conditional pages. MMC entries require actual System32
files. Discovery reads bytes only; it does not load the Settings DLL. Local
metadata does not guarantee that every dynamically gated feature is enabled.

Availability corrections use Microsoft's [Settings URI reference](https://learn.microsoft.com/en-us/windows/apps/develop/launch/launch-settings),
[Windows 11 24H2 removed features](https://learn.microsoft.com/en-us/windows/whats-new/whats-new-windows-11-version-24h2)
and [Cortana end-of-support notice](https://support.microsoft.com/en-us/cortana/end-of-support-for-cortana).
