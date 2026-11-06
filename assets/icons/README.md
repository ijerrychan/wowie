# Icon assets

Custom static glyphs are vendored SVG exports from the official Iconify API and
are embedded locally by the application. Their Iconify IDs, colors, source
URLs, and licenses are recorded in `iconify.json`.

Update the approved SVG assets with:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File tools/update-icons.ps1
```

The updater downloads and validates every listed SVG before changing any target,
limits writes to the bundled icon directories, and rolls back replaced files if
an update fails. Builds do not access the network. New custom glyphs must be
added to the manifest from an Iconify collection; do not hand-draw their paths.

System-provided executable/window icons and the Wowie brand icon are separate
resources and must not be replaced with generic Iconify glyphs.
