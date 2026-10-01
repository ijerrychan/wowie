# PowerToys Windows Settings data

`WindowsSettings.json`, `WindowsSettings.schema.json`, and `Resources.en.resx` are
from the Microsoft PowerToys Run Windows Settings plugin. The Rust catalog and
matching behavior in `src/providers/windows_settings.rs` adapt that plugin's
data model and behavior to Wow ie's provider API.

- Upstream: <https://github.com/microsoft/PowerToys/tree/main/src/modules/launcher/Plugins/Microsoft.PowerToys.Run.Plugin.WindowsSettings>
- Imported revision: `31bf904826709b81b4a9bd9756788a0596f2ff81`
- Imported: 2026-09-30
- License: MIT; see `PowerToys-LICENSE` and `Launcher-LICENSE`.

`Resources.en.json` is generated from `Resources.en.resx` by
`tools/import_powertoys_windows_settings.ps1`. `Resources.zh-CN.json` contains
Wowie's fallback Simplified Chinese resource translations and is checked
against every setting name, type, and area during an upstream refresh.

`Resources.zh-CN.microsoft-learn.json` contains setting labels and availability
notes checked against Microsoft's Simplified Chinese `ms-settings:` URI reference:
<https://learn.microsoft.com/zh-cn/windows/apps/develop/launch/launch-settings>.
These Microsoft-published documentation labels take precedence over fallback
translations. In this imported catalog, the map covers 165 of 199
`AppSettingsApp` page names. They are the localized names in Microsoft Learn,
not strings loaded from the installed Settings app package. Settings absent
from that public reference retain Wowie's fallback translation; Windows does
not expose a supported API for enumerating or localizing every Settings page.

Availability rules are also checked against Microsoft's URI reference and
Windows feature lifecycle documentation. Hardware checks are conservative:
the index hides a conditional result when Windows confirms its prerequisite is
absent; an indeterminate capability probe does not claim the hardware is
missing.

The implementation uses Microsoft's published [Windows 11 24H2 removed
features](https://learn.microsoft.com/en-us/windows/whats-new/whats-new-windows-11-version-24h2)
and [Cortana end-of-support notice](https://support.microsoft.com/en-us/cortana/end-of-support-for-cortana)
for retired destinations. It does not use community-maintained URI lists as an
availability or localization authority.
