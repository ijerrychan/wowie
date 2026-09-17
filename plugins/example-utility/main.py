import json
import locale
import os
import subprocess
import sys
import uuid


def load_catalog(requested, fallback="en"):
    if requested == "system":
        detected = locale.getlocale()[0] or os.environ.get("LANG", "en")
        requested = "zh-CN" if detected.lower().startswith(("zh", "chinese")) else "en"
    candidates = [requested, requested.split("-")[0], fallback, fallback.split("-")[0], "en"]
    for candidate in candidates:
        path = os.path.join(os.path.dirname(__file__), "locales", candidate + ".json")
        try:
            with open(path, encoding="utf-8") as file:
                return json.load(file)
        except (FileNotFoundError, json.JSONDecodeError):
            pass
    return {}


def copy_to_clipboard(value):
    commands = [["clip"]]
    if sys.platform == "darwin":
        commands = [["pbcopy"]]
    elif sys.platform != "win32":
        commands = [["xclip", "-selection", "clipboard"], ["xsel", "--clipboard", "--input"]]
    for command in commands:
        try:
            subprocess.run(command, input=value.encode("utf-8"), check=True,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            return True
        except (OSError, subprocess.CalledProcessError):
            pass
    return False


for line in sys.stdin:
    request = json.loads(line)
    catalog = load_catalog(request.get("locale", "system"), request.get("fallback_locale", "en"))
    query = request.get("query", "").strip().lower()
    if request.get("method") == "execute":
        value = str(uuid.uuid4())
        copied = copy_to_clipboard(value)
        key = "copied" if copied else "copy_failed"
        print(json.dumps({"title": value, "subtitle": catalog.get(key, key)}), flush=True)
        continue
    items = []
    if query == "uuid":
        items.append({
            "id": "uuid",
            "title": catalog.get("generate_uuid", "Generate UUID"),
            "subtitle": catalog.get("hint", "Press Enter to generate and copy"),
            "aliases": ["uuid", "guid"],
            "icon": {"type": "system", "target": r"C:\Windows\System32\cmd.exe"},
            "actions": [{"id": "generate"}]
        })
    # Keep stdout ASCII-safe for Windows shells with a legacy code page;
    # JSON consumers decode the escaped Unicode transparently.
    print(json.dumps({"items": items}), flush=True)
