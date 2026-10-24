"""Explicitly authorized IDE Python scripting bridge, not an OS sandbox."""
import contextlib
import json
import sys

with open(sys.argv[1], "r", encoding="utf-8") as stream:
    payload = json.load(stream)
request = payload["request"]
result = {"api_version": 1, "actions": [], "tool_windows": [], "diagnostics": [],
          "completions": [], "edits": [], "file_types": [], "themes": [], "keymaps": [], "status": ""}

class Console:
    def __init__(self):
        self.parts, self.size = [], 0
    def write(self, text):
        self.size += len(text)
        if self.size > 65536:
            raise ValueError("IDE scripting transcript exceeds 64 KiB")
        self.parts.append(text)
        return len(text)
    def flush(self):
        pass

class Ide:
    document = request.get("document")
    event = request["event"]
    action = request.get("action")
    def notify(self, text):
        result["status"] = str(text)
    def replace(self, start, end, text):
        """UTF-8 byte offsets in the captured document; one undo transaction."""
        result["edits"].append({"start": start, "end": end, "text": text})
    def register_action(self, identifier, title):
        """Invoked by re-running this script with ide.event/action populated."""
        result["actions"].append({"id": identifier, "title": title})
    def tool_window(self, identifier, title, text, actions=()):
        result["tool_windows"].append({"id": identifier, "title": title,
                                      "text": text, "actions": list(actions)})
    def inspect(self, start, end, message, code="ScriptInspection", severity="warning"):
        result["diagnostics"].append({"start": start, "end": end, "severity": severity,
                                      "message": message, "code": code})
    def complete(self, label, insert, detail="IDE script"):
        result["completions"].append({"label": label, "insert": insert, "detail": detail})

console = Console()
with contextlib.redirect_stdout(console), contextlib.redirect_stderr(console):
    exec(compile(payload["script"], "<ide-script>", "exec"), {"ide": Ide(), "__name__": "__ide_script__"})
if console.parts:
    result["status"] = (result["status"] + "\n" + "".join(console.parts))[:8192]
encoded = json.dumps(result, ensure_ascii=False)
if len(encoded.encode("utf-8")) > 1024 * 1024:
    raise ValueError("IDE script response exceeds 1 MiB")
sys.stdout.write(encoded)
