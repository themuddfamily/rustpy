#!/usr/bin/env python3
"""Headless Orca + AT-SPI acceptance for RustPy's AccessKit tree (Linux).

Run through `qa/accessibility/orca_acceptance.sh`, which supplies a private
D-Bus session (dbus-run-session), so neither the user's display nor the
user's accessibility bus is touched. This script then:

1. starts a private Xvfb display (-displayfd, never $DISPLAY) and, when
   available, openbox so the IDE window really gets X input focus;
2. enables the private AT-SPI bus (org.a11y.Status IsEnabled /
   ScreenReaderEnabled) and starts Orca with a private preference
   directory and `--debug-file`; Orca talks to a private speech-dispatcher
   whose audio goes to the ALSA "null" device (nothing is played);
3. starts RustPy on a fixture project and drives it with real X key events
   (xdotool, the path a keyboard-only user takes);
4. after each step, records what Orca spoke (the `SPEECH OUTPUT:` lines of
   its debug log) and what AT-SPI reports as focused (role, name, states);
5. dumps the IDE's accessible tree and checks names/roles of key widgets.

Writes `report.json` and `orca-debug.out` into the run directory and exits
non-zero when a required announcement or AT-SPI property is missing.
"""
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
PROCS = []


def log(*a):
    print("[orca-acceptance]", *a, flush=True)


def spawn(cmd, **kw):
    p = subprocess.Popen(cmd, start_new_session=True, **kw)
    PROCS.append(p)
    return p


def stop_all():
    for p in reversed(PROCS):
        if p.poll() is None:
            try:
                os.killpg(p.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
    deadline = time.time() + 5
    for p in reversed(PROCS):
        try:
            p.wait(max(0.1, deadline - time.time()))
        except subprocess.TimeoutExpired:
            try:
                os.killpg(p.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass


def wait_for(pred, timeout, step=0.25):
    end = time.time() + timeout
    while time.time() < end:
        v = pred()
        if v:
            return v
        time.sleep(step)
    return None


# ---------------------------------------------------------------- AT-SPI ---

def role_name(acc):
    try:
        return acc.get_role_name()
    except Exception:
        return "?"


def name_of(acc):
    try:
        return acc.get_name() or ""
    except Exception:
        return ""


def states_of(acc):
    try:
        ss = acc.get_state_set()
        return sorted(
            Atspi.StateType(s).value_nick for s in ss.get_states()
        )
    except Exception:
        return []


def find_app(name_re):
    desktop = Atspi.get_desktop(0)
    for i in range(desktop.get_child_count()):
        app = desktop.get_child_at_index(i)
        if app is not None and re.search(name_re, name_of(app), re.I):
            return app
    return None


def walk(acc, depth=0, limit=4000, out=None):
    if out is None:
        out = []
    if acc is None or len(out) >= limit:
        return out
    out.append((depth, acc))
    try:
        n = acc.get_child_count()
    except Exception:
        n = 0
    for i in range(n):
        try:
            walk(acc.get_child_at_index(i), depth + 1, limit, out)
        except Exception:
            pass
    return out


def focused(app):
    for _, acc in walk(app):
        if "focused" in states_of(acc):
            return acc
    return None


def describe(acc):
    if acc is None:
        return None
    d = {"role": role_name(acc), "name": name_of(acc), "states": states_of(acc)}
    try:
        text = acc.get_text_iface()
        if text is not None:
            n = text.get_character_count()
            d["text_len"] = n
            d["caret"] = text.get_caret_offset()
            d["text_head"] = Atspi.Text.get_text(acc, 0, min(n, 80))
    except Exception as e:
        d["text_error"] = str(e)
    return d


# ------------------------------------------------------------------ Orca ---

class OrcaLog:
    def __init__(self, path):
        self.path = path
        self.pos = 0

    def read_new(self):
        try:
            with open(self.path, "r", errors="replace") as f:
                f.seek(self.pos)
                data = f.read()
                self.pos = f.tell()
        except FileNotFoundError:
            return [], []
        speech = re.findall(r"SPEECH OUTPUT: '(.*?)'", data)
        events = re.findall(r"OBJECT EVENT: (\S+)", data)
        return speech, events


# ------------------------------------------------------------------ main ---

def main():
    run = Path(tempfile.mkdtemp(prefix="orca-", dir=os.environ.get("TMPDIR")))
    log("run dir", run)
    home = run / "home"
    for d in ("home/.config/speech-dispatcher", "xdg", "orca-prefs", "config", "speechd-log"):
        (run / d).mkdir(parents=True, exist_ok=True)
    os.chmod(run / "xdg", 0o700)
    env = os.environ.copy()
    for k in ("WAYLAND_DISPLAY", "PULSE_SERVER", "PULSE_RUNTIME_PATH", "DISPLAY"):
        env.pop(k, None)
    env.update(
        HOME=str(home),
        XDG_CONFIG_HOME=str(home / ".config"),
        XDG_DATA_HOME=str(home / ".local/share"),
        XDG_CACHE_HOME=str(home / ".cache"),
        XDG_RUNTIME_DIR=str(run / "xdg"),
        WINIT_UNIX_BACKEND="x11",
        LIBGL_ALWAYS_SOFTWARE="1",
        NO_AT_BRIDGE="0",
        GTK_USE_PORTAL="0",
        RUSTPY_CONFIG_DIR=str(run / "config"),
        RUSTPY_TRUST_ALL="1",
    )
    # Speech goes to speech-dispatcher, whose audio goes nowhere.
    (home / ".config/speech-dispatcher/speechd.conf").write_text(
        f'LogLevel 4\nLogDir "{run}/speechd-log"\n'
        'AudioOutputMethod "alsa"\nAudioALSADevice "null"\n'
        'AddModule "espeak-ng" "sd_espeak-ng" "espeak-ng.conf"\n'
        'DefaultModule espeak-ng\nDefaultLanguage "en"\n'
    )

    # Private X display (never the user's). /tmp/.X11-unix may be read-only
    # (WSLg), where -displayfd never settles; Xvfb still listens on the
    # abstract socket, so pick a free display number as xvfb-run -a does.
    num = next(n for n in range(97, 200)
               if not Path(f"/tmp/.X{n}-lock").exists())
    spawn(["Xvfb", f":{num}", "-screen", "0", "1400x900x24", "-nolisten", "tcp"],
          env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    env["DISPLAY"] = f":{num}"
    if not wait_for(lambda: subprocess.run(
            ["xdpyinfo"], env=env, capture_output=True).returncode == 0, 20):
        log("FAIL: Xvfb did not start")
        return 2
    os.environ.update(env)
    log("display", env["DISPLAY"])
    if shutil.which("openbox"):
        spawn(["openbox", "--sm-disable"], env=env,
              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    # Turn the private accessibility bus on, as a desktop does for Orca.
    for prop in ("IsEnabled", "ScreenReaderEnabled"):
        subprocess.run(
            ["gdbus", "call", "--session", "--dest", "org.a11y.Bus",
             "--object-path", "/org/a11y/bus", "--method",
             "org.freedesktop.DBus.Properties.Set", "org.a11y.Status", prop,
             "<true>"], env=env, check=True, stdout=subprocess.DEVNULL)

    debug = run / "orca-debug.out"
    # Orca's debug file is block-buffered, so a step would only see what Orca
    # spoke once 8 KiB more had been logged: flush after every line.
    (run / "orca-prefs" / "orca-customizations.py").write_text(
        "import orca.debug as _debug\n"
        "_println = _debug.println\n"
        "def _flushing_println(*args, **kwargs):\n"
        "    _println(*args, **kwargs)\n"
        "    if _debug.debugFile:\n"
        "        _debug.debugFile.flush()\n"
        "_debug.println = _flushing_println\n")
    spawn(["orca", "--replace", "-u", str(run / "orca-prefs"),
           "--debug-file", str(debug)], env=env,
          stdout=open(run / "orca.stdout", "w"), stderr=subprocess.STDOUT)
    if not wait_for(lambda: find_app(r"^orca$"), 30):
        log("FAIL: Orca never registered on the AT-SPI bus")
        return 2
    log("orca registered")
    orca = OrcaLog(debug)
    time.sleep(2)
    orca.read_new()

    # Fixture project: two modules and a package for tree navigation.
    proj = run / "project"
    (proj / "pkg").mkdir(parents=True)
    (proj / "main.py").write_text(
        "def greet(name):\n    return 'Hello ' + name\n\n\nprint(greet('Orca'))\n")
    (proj / "util.py").write_text("VALUE = 42\n")
    (proj / "pkg" / "__init__.py").write_text("")
    profile = os.environ.get("RUSTPY_A11Y_PROFILE", "debug")
    binary = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")) / profile / "rustpy"
    ide = spawn([str(binary), str(proj), "--open-file", str(proj / "main.py")],
                env=env, cwd=str(proj), stdout=open(run / "rustpy.log", "w"),
                stderr=subprocess.STDOUT)
    app = wait_for(lambda: find_app(r"rustpy"), 120, 0.5)
    if not app:
        log("FAIL: RustPy never registered on the AT-SPI bus")
        return 2
    log("rustpy registered:", name_of(app))

    def x(*args):
        return subprocess.run(["xdotool", *args], env=env, capture_output=True,
                              text=True).stdout.strip()

    win = wait_for(lambda: x("search", "--onlyvisible", "--pid", str(ide.pid)), 60)
    win = (win or "").split("\n")[0]
    if win:
        x("windowactivate", "--sync", win)
        x("windowfocus", "--sync", win)
    # Let the tree populate (indexing, first frames).
    wait_for(lambda: len(walk(app)) > 40, 60, 1)
    time.sleep(3)

    steps = []

    class IdeExited(Exception):
        pass

    def step(label, keys=(), text=None, settle=1.5, expect=(), expect_focus=None,
             quiet=False, timeout=15.0):
        """Sends keys, then polls Orca's log and AT-SPI focus until the
        expectations hold (at least `settle`, at most `timeout` seconds): a
        loaded machine can take seconds per frame."""
        if ide.poll() is not None:
            raise IdeExited(label)
        for k in keys:
            if k.startswith("sleep:"):
                time.sleep(float(k[6:]))
            else:
                x("key", "--clearmodifiers", k)
                time.sleep(0.25)
        if text:
            x("type", "--delay", "60", text)
        start = time.time()
        speech, events = [], []
        while True:
            time.sleep(0.4)
            more_speech, more_events = orca.read_new()
            speech += more_speech
            events += more_events
            f = describe(focused(app))
            ok_speech = all(any(re.search(e, s, re.I) for s in speech) for e in expect)
            ok_focus = True
            if expect_focus:
                ok_focus = f is not None and all(
                    re.search(v, str(f.get(k, "")), re.I) for k, v in expect_focus.items())
            elapsed = time.time() - start
            if (ok_speech and ok_focus and elapsed >= settle) or elapsed >= timeout:
                break
        if os.environ.get("RUSTPY_A11Y_SHOTS"):
            shots = run / "shots"
            shots.mkdir(exist_ok=True)
            png = shots / f"{len(steps):03d}-{re.sub(r'[^A-Za-z0-9]+', '-', label)[:40]}.png"
            subprocess.run(f"xwd -root -silent | magick xwd:- {png}", shell=True,
                           env=env, stderr=subprocess.DEVNULL)
        rec = {"step": label, "keys": list(keys), "text": text, "speech": speech,
               "events": sorted(set(events)), "focus": f,
               "expect": list(expect), "expect_focus": expect_focus,
               "pass": ok_speech and ok_focus}
        steps.append(rec)
        if not quiet:
            log(("PASS" if rec["pass"] else "FAIL"), label, "| focus:",
                f and (f["role"], f["name"][:60]), "| speech:", speech[:8])
        return rec

    try:
        drive(step, app, steps)
    except IdeExited as e:
        tail = (run / "rustpy.log").read_text(errors="replace")[-2000:]
        steps.append({"step": f"IDE exited before '{e}'", "pass": False, "log_tail": tail})
        log("FAIL: IDE exited:", tail)
        app = None
    return finish(run, app, steps)


def tab_until(step, pattern, label, limit=80, keys=("Tab",), field="name"):
    """Presses Tab until the AT-SPI focus `field` (name or role) matches `pattern`
    (keyboard reachability)."""
    for i in range(limit):
        rec = step(f"{label}: Tab #{i + 1}", list(keys), settle=0.6, expect=[], quiet=True,
                   expect_focus={field: pattern}, timeout=2.0)
        rec["pass"] = True  # a stop on the way; tab_until's caller checks the target
        f = rec["focus"] or {}
        if re.search(pattern, f.get(field, ""), re.I):
            return rec
    return None


def drive(step, app, steps):
    step("startup: window and initial focus", settle=3, timeout=30,
         expect=[r"rustpy|main\.py|source editor"])
    # Editor: Escape returns focus to the editor from anywhere.
    step("focus editor", ["Escape"], expect=[],
         expect_focus={"role": r"panel", "name": r"source editor: main\.py"})
    step("caret: next line read", ["ctrl+Home", "Down"], expect=[r"line 2.*return"])
    step("caret: previous line read", ["Up"], expect=[r"line 1.*def greet"])
    step("caret: character moves", ["End", "Left"], expect=[r"column 16"])
    step("type into editor", ["ctrl+End", "Return"], text="x = 1",
         expect=[r"^x$", r"^1$"])
    step("line read after typing", ["Up", "Down"], expect=[r"line context: x = 1"])
    # Project tree, keyboard only.
    # The tree keeps focus; its selected row is the active descendant, which
    # AT-SPI reports (and Orca speaks) as the focused tree item.
    step("Alt+1 focuses Project tree (selected row spoken)", ["alt+1"],
         expect=[r"main\.py"], expect_focus={"role": r"tree item", "name": r"main\.py"})
    step("tree: Down announces next row", ["Down"], expect=[r"util\.py"],
         expect_focus={"role": r"tree item", "name": r"util\.py"})
    step("tree: Up announces previous row", ["Up"], expect=[r"main\.py"],
         expect_focus={"role": r"tree item", "name": r"main\.py"})
    # Open util.py from the tree to get a second editor tab.
    for _ in range(8):
        f = steps[-1]["focus"] or {}
        if re.search(r"util\.py", f.get("name", "")):
            break
        step("tree: seek util.py", ["Down"], expect=[r"\.py|pkg"])
    step("tree: Enter opens util.py in the editor", ["Return"],
         expect=[r"source editor: util\.py"],
         expect_focus={"name": r"source editor: util\.py"})
    step("tabs: Alt+Left previous tab", ["alt+Left"], expect=[r"source editor: main\.py"],
         expect_focus={"name": r"source editor: main\.py"})
    step("tabs: Alt+Right next tab", ["alt+Right"], expect=[r"source editor: util\.py"],
         expect_focus={"name": r"source editor: util\.py"})
    # Settings dialog -> Accessibility page, keyboard only.
    step("open Settings (Ctrl+Alt+S)", ["ctrl+alt+s"], expect=[r"^settings$", r"search settings"],
         expect_focus={"role": r"entry"})
    step("filter pages: Accessibility", [], text="Accessibility", settle=2.5, expect=[])
    page = tab_until(step, r"^accessibility", "seek Accessibility page", 20)
    steps.append({"step": "Tab reaches the Accessibility page entry",
                  "pass": page is not None, "speech": page and page["speech"]})
    if page:
        step("select Accessibility page (Space)", ["space"], expect=[r"pressed"])
    reached = tab_until(step, r"test notification announcement",
                        "seek test-notification button", 80)
    steps.append({"step": "Tab reaches 'Test notification announcement'",
                  "pass": reached is not None,
                  "speech": reached and reached["speech"]})
    log("PASS" if reached else "FAIL", "Tab reaches 'Test notification announcement'")
    if reached:
        step("activate test notification (Space)", ["space"],
             expect=[r"information: accessibility notification test"])
    # Settings is modal: Shift+Tab wraps back to its search field, never into
    # the main window. Then Appearance: its combo boxes are read with names.
    start = len(steps)
    search = tab_until(step, r"^entry$", "Shift+Tab back to search", 120,
                       keys=("shift+Tab",), field="role")
    left = [s["focus"]["name"] for s in steps[start:]
            if re.search(r"source editor", (s["focus"] or {}).get("name", ""), re.I)]
    steps.append({"step": "Shift+Tab stays in Settings and reaches its search field",
                  "pass": search is not None and not left,
                  "speech": search and search["speech"], "left_dialog_for": left})
    if search:
        step("filter pages: Appearance", ["ctrl+a"], text="Appearance", settle=2.5, expect=[])
        page = tab_until(step, r"^appearance", "seek Appearance page", 20)
        if page:
            # The page content changes; Orca does not always repeat the state.
            step("select Appearance page (Space)", ["space"], expect=[],
                 expect_focus={"name": r"^appearance"})
        combo = tab_until(step, r"^language", "seek Language combo box", 40)
        named = combo is not None and (combo["focus"] or {}).get("role") == "combo box" \
            and any(re.search(r"language.*combo box", s, re.I) for s in combo["speech"])
        steps.append({"step": "Appearance combo box announced with its name ('Language')",
                      "pass": named, "speech": combo and combo["speech"],
                      "focus": combo and combo["focus"]})
        log("PASS" if named else "FAIL", "Appearance combo box announced with its name")
        # Its current value is spoken too ("Language, combo box, English"):
        # AT-SPI exposes the selected text as the combo box's text.
        spoken = " ".join(combo["speech"]) if combo else ""
        value = ((combo or {}).get("focus") or {}).get("text_head", "")
        valued = combo is not None and re.search(r"english", value, re.I) is not None \
            and re.search(r"language.*combo box.*english", spoken, re.I) is not None
        steps.append({"step": "Appearance combo box announced with its current value",
                      "pass": valued, "speech": combo and combo["speech"],
                      "atspi_text": value})
        log("PASS" if valued else "FAIL", "Appearance combo box announced with its value")
    step("close Settings (Escape)", ["Escape"], expect=[r"source editor"],
         expect_focus={"name": r"source editor"})
    # Other modal dialogs trap focus too: Tab and Shift+Tab in Go to Line
    # never reach the editor behind it.
    step("open Go to Line (Ctrl+G)", ["ctrl+g"], expect=[],
         expect_focus={"role": r"entry"})
    start = len(steps)
    for i in range(6):
        step(f"Go to Line: Tab #{i + 1}", ["Tab"], settle=0.6, expect=[], quiet=True,
             timeout=2.0)
    for i in range(6):
        step(f"Go to Line: Shift+Tab #{i + 1}", ["shift+Tab"], settle=0.6, expect=[],
             quiet=True, timeout=2.0)
    left = [s["step"] for s in steps[start:]
            if re.search(r"source editor", (s["focus"] or {}).get("name", ""), re.I)]
    steps.append({"step": "Tab / Shift+Tab stay inside the modal Go to Line dialog",
                  "pass": not left, "left_dialog_at": left})
    log("FAIL" if left else "PASS", "Tab / Shift+Tab stay inside Go to Line")
    step("close Go to Line (Escape)", ["Escape"], expect=[r"source editor"],
         expect_focus={"name": r"source editor"})


def finish(run, app, steps):

    # Whole-tree audit.
    nodes = walk(app) if app is not None else []
    roles = {}
    unnamed = []
    for depth, acc in nodes:
        r = role_name(acc)
        roles[r] = roles.get(r, 0) + 1
        if r in ("push button", "toggle button", "check box", "radio button",
                 "page tab", "tree item", "menu item", "combo box", "slider") \
                and not name_of(acc).strip():
            unnamed.append({"role": r, "states": states_of(acc),
                            "parent": name_of(acc.get_parent()) if acc.get_parent() else ""})
    spoken = re.findall(r"SPEECH OUTPUT: '(.*?)'",
                        (run / "orca-debug.out").read_text(errors="replace"))
    startup_note = [t for t in spoken if re.search(r"information: test framework detected", t, re.I)]
    # Informational: a notification posted before the screen reader attached
    # is part of the initial tree, which AT-SPI does not announce.
    steps.append({"step": "info: startup notification spoken", "pass": True,
                  "info": bool(startup_note), "speech": startup_note[:2]})
    tree_text = "\n".join(
        "  " * d + f"[{role_name(a)}] {name_of(a)!r}" for d, a in nodes)
    (run / "atspi-tree.txt").write_text(tree_text)
    report = {
        "run_dir": str(run),
        "orca_version": subprocess.run(["orca", "--version"], capture_output=True,
                                       text=True).stdout.strip(),
        "nodes": len(nodes),
        "roles": roles,
        "unnamed_interactive": unnamed,
        "steps": steps,
    }
    (run / "report.json").write_text(json.dumps(report, indent=2))
    fails = [s["step"] for s in steps if not s.get("pass")]
    log(f"nodes={len(nodes)} roles={roles}")
    log(f"unnamed interactive nodes: {len(unnamed)}")
    log("report:", run / "report.json")
    if fails:
        log("FAILED steps:", fails)
    return 1 if fails or unnamed else 0


if __name__ == "__main__":
    try:
        code = main()
    finally:
        stop_all()
    sys.exit(code)
