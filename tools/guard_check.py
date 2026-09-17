#!/usr/bin/env python3
"""Break one thing at a time and check that the harness notices.

Every check in `tools/shell_check.mjs` claims to name a specific defect. This runs
each claim: patch the shell with the bug the check describes, run the harness, and
require that the check fails. A check that passes against its own bug is not a
guard, it is decoration.

Two things it learned the hard way, both of which are the point of running it:

  * A harness that *crashes* on a broken shell reports nothing, and a crash looks
    like a pass from a distance. So a mutation that produces no FAIL line is
    reported as a failure of this script, with the crash output attached -- and
    that is how the pane section's `waitFor` precondition was found to be a
    precondition the section could not survive losing.
  * A run killed part-way leaves the shell mutated. The originals are held in
    memory *and* on disk, and restored on every exit path including a signal.

Usage: python tools/guard_check.py [substring-of-a-description]
"""
import atexit
import re
import shutil
import signal
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
APP = ROOT / "frontend" / "app" / "app.js"
HARNESS = ROOT / "tools" / "shell_check.mjs"
BACKUP = ROOT / "target" / "guard_backup"
NODE = "C:/Users/haizard/.workbuddy-ai/binaries/node/versions/22.22.2-2/node.exe"
NODE_PATH = "C:/Users/haizard/.workbuddy-ai/binaries/node/workspace/node_modules"

EL_BOUNDARY = (
    "PANE_ELS.has(name) ? root.querySelector(`.${name}`) : document.getElementById(name);"
)
SOCKET = (
    "    socket = new WebSocket(`${scheme}://${location.host}/ws/market/"
    "${symbol}/${timeframe}${query}`);"
)

# (description, file, find, replace, checks that must fail)
MUTATIONS = [
    (
        "a pane reads the first pane's controls instead of its own",
        APP,
        EL_BOUNDARY,
        "PANE_ELS.has(name) ? document.querySelector(`.${name}`) : document.getElementById(name);",
        [
            "and it opens on the next timeframe of the same instrument, so it is not a copy",
            "changing one chart's instrument fetches that instrument",
            "a shape drawn in the second chart is stored against the second chart's instrument",
        ],
    ),
    (
        "Add chart opens a copy of the chart it came from",
        APP,
        "pane.fillSeries(coverage, activePane.symbol(), frames[at + 1]);",
        "pane.fillSeries(coverage, activePane.symbol(), activePane.timeframe());",
        [
            "and it opens on the next timeframe of the same instrument, so it is not a copy",
            "so the two charts have asked the engine for two different series",
        ],
    ),
    (
        "the page only ever holds one chart",
        APP,
        "const MAX_PANES = 4;",
        "const MAX_PANES = 1;",
        ["adding a chart puts a second one beside it"],
    ),
    (
        "a wheel rebuilds every pane, not the one under the pointer",
        APP,
        "  function scheduleRender() {",
        "  function scheduleRender() {\n    for (const p of panes) if (p !== paneApi) p.redraw();",
        ["a wheel in one chart rebuilds that chart and only that one"],
    ),
    (
        "the new pane is the first pane's node, not a clone of it",
        APP,
        '  const node = activePane.root.cloneNode(true);\n  el("charts").appendChild(node);',
        '  const node = activePane.root;\n  el("charts").appendChild(node);',
        ["adding a chart puts a second one beside it"],
    ),
    (
        "every pane opens the first pane's channel",
        APP,
        SOCKET,
        "    socket = new WebSocket(`${scheme}://${location.host}/ws/market/BTCUSDT/5m${query}`);",
        ["and each one holds its own market channel"],
    ),
    (
        "the book keeps following the first pane rather than the active one",
        APP,
        '  const symbol = activePane ? activePane.symbol() : "";\n'
        "  if (!symbol) return;\n  const scheme = location.protocol",
        '  const symbol = panes.length ? panes[0].symbol() : "";\n'
        "  if (!symbol) return;\n  const scheme = location.protocol",
        ["and the panel's book channel moved to that instrument"],
    ),
    (
        "a pointer press no longer claims the chart it landed on",
        APP,
        "    (event) => setActive(ownerOf(event.target)),\n    true",
        "    () => {},\n    true",
        ["and touching a chart is what makes it the one the panel follows"],
    ),
    (
        "a control change no longer claims the chart it belongs to",
        APP,
        '  el("charts").addEventListener("change", (event) => setActive(ownerOf(event.target)));',
        "",
        ["and the chart whose instrument changed is the one the panel follows"],
    ),
    (
        "the add button stays live past the cap",
        APP,
        '  el("split").disabled = panes.length >= MAX_PANES;',
        "",
        ["and the button that would add a fifth says so instead of refusing"],
    ),
    (
        "the add button never comes back",
        APP,
        '  el("split").disabled = panes.length >= MAX_PANES;',
        '  el("split").disabled = true;',
        ["adding a chart puts a second one beside it"],
    ),
    (
        "closing a chart leaves it on the page",
        APP,
        "  pane.root.remove();\n  panes.splice(at, 1);",
        "  panes.splice(at, 1);",
        ["closing a chart takes it off the page"],
    ),
    (
        "closing a pane leaves the panel pointing at it",
        APP,
        "    activePane = null;\n    setActive(panes[Math.max(0, at - 1)]);",
        "",
        ["and the panel moves to the chart beside it, not to nothing"],
    ),
    (
        "a closed pane leaves its market channel running",
        APP,
        "    destroy() {\n      if (socket) socket.close();",
        "    destroy() {\n      if (false && socket) socket.close();",
        ["and its market channel was closed with it, not left running"],
    ),
    (
        "the last pane is closable",
        APP,
        "  if (panes.length < 2) return;\n\n  const at = panes.indexOf(pane);",
        "  const at = panes.indexOf(pane);",
        ["and clicking it anyway does not empty the page"],
    ),
    (
        "the close button is shown even when there is nothing to close",
        APP,
        '    pane.root.querySelector(".close").hidden = panes.length < 2;',
        '    pane.root.querySelector(".close").hidden = false;',
        ["closing down to one chart leaves it open, with nothing left to close"],
    ),
]

originals = {}
BACKUP.mkdir(parents=True, exist_ok=True)


def restore(*_):
    for path, text in originals.items():
        path.write_text(text, encoding="utf-8", newline="\n")


def on_signal(*_):
    restore()
    raise SystemExit(130)


for path in (APP, HARNESS):
    originals[path] = path.read_text(encoding="utf-8")
    shutil.copyfile(path, BACKUP / path.name)
atexit.register(restore)
signal.signal(signal.SIGTERM, on_signal)
signal.signal(signal.SIGINT, on_signal)


def run_harness():
    proc = subprocess.run(
        [NODE, "tools/shell_check.mjs"],
        cwd=str(ROOT),
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        env={"NODE_PATH": NODE_PATH, "SYSTEMROOT": "C:\\Windows", "PATH": "C:\\Windows"},
    )
    out = (proc.stdout or "") + (proc.stderr or "")
    failed = set()
    for line in out.splitlines():
        if line.startswith(" FAIL "):
            failed.add(line[6:].split(" -- ")[0].strip())
    return proc.returncode, failed, out


def main():
    only = sys.argv[1] if len(sys.argv) > 1 else None
    bad = 0
    try:
        for desc, path, find, repl, must_fail in MUTATIONS:
            if only and only not in desc:
                continue
            text = originals[path]
            if text.count(find) != 1:
                print(f"SKIP  {desc}\n      {text.count(find)} matches for the text to break")
                bad += 1
                continue
            path.write_text(text.replace(find, repl), encoding="utf-8", newline="\n")
            code, failed, out = run_harness()
            path.write_text(text, encoding="utf-8", newline="\n")

            if not failed:
                tail = "\n".join(out.strip().splitlines()[-6:])
                print(f"BAD   {desc}\n      the harness reported no failure at all (exit {code})")
                for line in tail.splitlines():
                    print(f"      | {line}")
                bad += 1
                continue
            missed = [name for name in must_fail if name not in failed]
            if missed:
                print(f"BAD   {desc}")
                for name in missed:
                    print(f"      did not fail: {name}")
                print(f"      what failed was: {sorted(failed)}")
                bad += 1
                continue
            others = sorted(failed - set(must_fail))
            print(f"ok    {desc}")
            for name in must_fail:
                print(f"      failed as named: {name}")
            if others:
                print(f"      and {len(others)} more, which is expected here: {others[0]}")
    finally:
        for path, text in originals.items():
            path.write_text(text, encoding="utf-8", newline="\n")

    for path, text in originals.items():
        if path.read_text(encoding="utf-8") != text:
            print(f"\nRESTORE FAILED for {path} -- a copy is in {BACKUP}")
            bad += 1

    print(
        "\nall guards failed against the bug they name"
        if not bad
        else f"\n{bad} mutation(s) not caught"
    )
    return 1 if bad else 0


if __name__ == "__main__":
    raise SystemExit(main())
