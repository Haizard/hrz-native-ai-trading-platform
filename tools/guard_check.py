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
# The market socket, as `connectLive` builds it. Rewritten when `connectLive`
# started keeping a handle on the socket it opened, so a superseded one cannot
# speak for the pane -- a mutation anchored on the old one-liner would silently
# stop matching, and a mutation that does not apply is a guard that never runs.
SOCKET = (
    "    const ws = new WebSocket(\n"
    "      `${scheme}://${location.host}/ws/market/${symbol}/${timeframe}${query}`\n"
    "    );"
)

# (description, file, find, replace, checks that must fail)
MUTATIONS = [
    (
        "a pane reads the first pane's controls instead of its own",
        APP,
        EL_BOUNDARY,
        "PANE_ELS.has(name) ? document.querySelector(`.${name}`) : document.getElementById(name);",
        # Not "and it opens on the same instrument as the chart it came from": the
        # clone carries the first pane's option list, so its symbol select already
        # reads BTCUSDT before `fillSeries` runs. That check is worth having -- it
        # asserts the new pane starts on the instrument it came from -- but it
        # cannot see this mutation, and claiming it does would be a lie.
        [
            "and on the next timeframe up that can fill a chart, so it is not a copy",
            "changing one chart's instrument fetches that instrument",
            "a shape drawn in the second chart is stored against the second chart's instrument",
        ],
    ),
    (
        "Add chart opens a copy of the chart it came from",
        APP,
        "nextFrame(activePane.symbol(), activePane.timeframe(), wanted)",
        "activePane.timeframe()",
        [
            "and on the next timeframe up that can fill a chart, so it is not a copy",
            "so the two charts have asked the engine for two different series",
        ],
    ),
    (
        "a new chart opens on the next timeframe whether or not it holds a chart",
        APP,
        "  const fills = up.find((f) => f.candles >= wanted);",
        "  const fills = up[0];",
        ["and on the next timeframe up that can fill a chart, so it is not a copy"],
    ),
    (
        "the chart opens on the first series the server listed",
        APP,
        '      else el("timeframe").value = deepestFrame(symbol);',
        "",
        ["the chart opens on the series with the most bars, not the first one listed"],
    ),
    (
        "the timeframe options keep the server's order",
        APP,
        "    const frames = [...(entry ? entry.timeframes : [])].sort(\n"
        "      (a, b) => frameMinutes(a.timeframe) - frameMinutes(b.timeframe)\n"
        "    );",
        "    const frames = entry ? entry.timeframes : [];",
        # Only the ladder. The default series is still right without the sort,
        # because `deepestFrame` picks by bar count and does not care about order
        # -- so this mutation is caught by the ordering check alone, which is the
        # one that names it.
        [
            "and the timeframe options read as a ladder rather than in the server's order",
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
        "    const ws = new WebSocket(\n"
        "      `${scheme}://${location.host}/ws/market/BTCUSDT/5m${query}`\n"
        "    );",
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
    # --- the two destructive controls, and the silent channel -----------------
    #
    # The report behind these: "when I click the clear button it removes all the
    # attached tools on the chart instead of the one I have selected", and seven
    # hours of the same candles with nothing anywhere saying why.
    (
        "Clear all deletes everything on the first click",
        APP,
        "    if (now > clearArmedUntil) {",
        "    if (false) {",
        ["the first click on Clear all only asks"],
    ),
    (
        "the delete control is never disabled",
        APP,
        "    if (remove) remove.disabled = !selectedDrawing;",
        "    if (remove) remove.disabled = false;",
        ["with nothing selected the delete control is disabled, not silently inert"],
    ),
    (
        "the delete control clears the chart instead of deleting the selection",
        APP,
        '    el("deleteDrawing").addEventListener("click", deleteSelected);',
        '    el("deleteDrawing").addEventListener("click", clearDrawings);',
        ["the delete control removes the selection and leaves the rest"],
    ),
    (
        "a notice on the market channel is ignored",
        APP,
        '      } else if (frame.type === "notice") {',
        "      } else if (false) {",
        ["a notice on the market channel reaches the strip under the chart"],
    ),
    (
        "the notice is written to the strip but not held",
        APP,
        '        feedNotice = frame.message;\n        live.state = "nofeed";\n'
        '        el("chartNote").textContent = feedNotice;',
        '        live.state = "nofeed";\n        el("chartNote").textContent = frame.message;',
        ["and it survives the render a pan would have triggered"],
    ),
    (
        "the shell goes back to splitting a cell into two numbers",
        APP,
        '        ctx.fillText(pair, cell.x + cell.w / 2, cell.y + cell.h / 2);',
        "        ctx.fillText(cell.bid_text, cell.x + 4, cell.y + cell.h / 2);\n"
        "        ctx.fillText(cell.ask_text, cell.x + 40, cell.y + cell.h / 2);",
        ["a footprint draws its ladder as `bid x ask` pairs"],
    ),
    (
        "the shell goes back to a fixed cell width instead of asking the engine",
        APP,
        "    const cellPx = footCellPx || SEED_CELL_PX;",
        "    const cellPx = SEED_CELL_PX;",
        ["and the shell asks for the most candles that cell width allows"],
    ),
    (
        "the engine's cell width is never learned",
        APP,
        "      footCellPx = Math.ceil(scene.footprint.min_cell_px);",
        "      footCellPx = 0;",
        ["and the shell asks for the most candles that cell width allows"],
    ),
    # --- the live badge, which has to be able to say the feed is dead -----------
    #
    # The report behind these: "I am not sure [the data is real time] and I cannot
    # prove it". A badge that cannot go out is the defect rather than the feature,
    # so every one of these makes it lie in a different way.
    (
        "the badge never learns that a frame arrived",
        APP,
        "        live.at = Date.now();",
        "        live.at = 0;",
        ["a bar on the channel makes the badge say live, with the age as the evidence"],
    ),
    (
        "the badge's age never grows",
        APP,
        "    const age = live.at ? Date.now() - live.at : Infinity;",
        "    const age = 0;",
        ["and a channel that has gone quiet stops claiming to be live"],
    ),
    (
        "the badge is only refreshed when something else redraws",
        APP,
        "  liveTimer = setInterval(refreshLiveBadge, 1000);",
        "  liveTimer = 0;",
        ["and a channel that has gone quiet stops claiming to be live"],
    ),
    (
        "a refused channel is not remembered as refused",
        APP,
        '        live.state = "nofeed";',
        '        live.state = "offline";',
        ["a channel the server refused to feed says so, rather than saying offline"],
    ),
    (
        "the badge claims live before any bar has arrived",
        APP,
        "    if (!live.at) {",
        "    if (false) {",
        ["and an open channel with no bar yet does not claim to be live"],
    ),
    (
        "a closed channel keeps the last reading it had",
        APP,
        '      if (live.state !== "nofeed") live.state = "offline";',
        "      live.state = live.state;",
        ["and a channel that closes says offline"],
    ),
    (
        "a live feed over a frozen ladder still claims the chart is live",
        APP,
        "    if (lag > barMs * 2) {",
        "    if (false) {",
        ["a live feed over a frozen ladder says which of the two is stale"],
    ),
    (
        "a candle published after a notice does not outrank the notice",
        APP,
        '        live.state = "open";\n        render();',
        "        render();",
        ["and a candle published by something else outranks the notice"],
    ),
    # The order book. Three ways its channel ends and three different things the
    # panel has to say, plus the retry that turns "not yet" into a book. Each was
    # written after the live log showed the feed healthy, the books syncing, and a
    # user still seeing "WebSocket is closed before the connection is established"
    # over an empty ladder.
    (
        "a close we asked for is reported as a disconnect",
        APP,
        "    if (ws.bookSuperseded) return;",
        "    if (false) return;",
        ["a close we asked for is reported as no close at all"],
    ),
    (
        "the server's own explanation is overwritten by a generic one",
        APP,
        "    if (ws.bookNotice) {\n"
        '      el("bookMsg").textContent = ws.bookNotice;\n'
        "    } else {\n"
        '      el("bookMsg").textContent = "The book disconnected.";\n'
        "    }",
        '    el("bookMsg").textContent = "The book disconnected.";',
        ["a book the server closed keeps the server's own explanation"],
    ),
    (
        "a book that was merely late is never asked for again",
        APP,
        "    scheduleBookRetry(symbol);",
        "    // no retry",
        ["a book channel that closed is re-opened on its own"],
    ),
    (
        "a refused agent channel reports one generic sentence for every cause",
        APP,
        "      agentChannelReason().then((reason) => {\n"
        '        reject(new Error(reason || "could not reach the agent channel"));\n'
        "      });",
        '      reject(new Error("could not reach the agent channel"));',
        [
            "an agent that is not configured says so, rather than blaming the connection",
            "and it asks the server before answering, instead of guessing from the socket",
            "a refused credential is reported as a credential, not as a missing agent",
        ],
    ),
    (
        "an agent with no Bedrock credentials is reported as a credential problem",
        APP,
        '    if (agent && agent.readiness === "not_configured") {\n'
        '      return "the AI analyst is not configured on this deployment";\n'
        "    }",
        "    // the deployment-level answer is never consulted",
        ["an agent that is not configured says so, rather than blaming the connection"],
    ),
    # The scanner. `GET /scan` shipped with eight route tests and no reader, so
    # these are the first checks that have ever looked at the panel -- and the
    # four ways it could be wrong without a Rust test noticing.
    (
        "a symbol the scan could not measure is drawn as one that ranked last",
        APP,
        "  const ranked = (scan.rows || []).map((r, i) => row(r, i + 1)).join(\"\");",
        "  const ranked = [...(scan.rows || []), ...(scan.failures || [])]"
        ".map((r, i) => row(r, i + 1)).join(\"\");",
        ["a symbol that could not be measured is not shown as one that ranked last"],
    ),
    (
        "the metric's label is sent instead of its wire name",
        APP,
        '    params.set("metric", el("scanMetric").value);',
        '    params.set("metric", el("scanMetric").selectedOptions[0].text);',
        ["the metric and timeframe the user picked are what is asked for"],
    ),
    (
        "the ranking is re-sorted in the shell",
        APP,
        "  const ranked = (scan.rows || []).map((r, i) => row(r, i + 1)).join(\"\");",
        "  const ranked = [...(scan.rows || [])].sort((a, b) => (b.value ?? 0) - (a.value ?? 0))"
        ".map((r, i) => row(r, i + 1)).join(\"\");",
        [
            "and in the order the server ranked them, not re-sorted here",
            "and the value under it is that instrument's own, in the server's order",
        ],
    ),
    (
        "an empty symbols box sends a blank list instead of the whole venue",
        APP,
        '    const typed = el("scanSymbols").value.trim();\n'
        '    if (typed) params.set("symbols", typed);',
        '    params.set("symbols", el("scanSymbols").value.trim());',
        ["and an empty box asks about the venue, sending no symbols at all"],
    ),
    (
        "the server's summary is replaced by a sentence the shell wrote",
        APP,
        '    <p class="muted">${escapeHtml(scan.summary || "")}</p>',
        '    <p class="muted">Scan complete.</p>',
        ["and the server's own summary is shown, not a sentence invented here"],
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


def preflight():
    """Refuse to run against a baseline that is not the baseline.

    A run killed with SIGKILL cannot restore the file -- `atexit` and the signal
    handler never run -- so the next run reads the *mutated* text as its baseline
    and then measures every other mutation against a shell that is already broken.

    That is not hypothetical. It happened on 2026-09-17: a killed sweep left
    `if (panes.length < 2) return;` deleted from `closePane` and `hidden = false`
    in `refreshCloseButtons`, and the next sweep spent five minutes reporting on
    the wrong file -- two of its mutations were "SKIP, 0 matches" and the pane
    checks were red against a defect nobody had introduced on purpose.

    The detector is free, because an applied mutation always removes its own
    anchor: if any `find` is not present exactly once, something is left over.
    """
    stale = []
    for desc, path, find, _repl, _must in MUTATIONS:
        if path.read_text(encoding="utf-8").count(find) != 1:
            stale.append((desc, path))
    if not stale:
        return True
    print("REFUSING TO RUN: the working copy is not the one these mutations were")
    print("written against. A killed run leaves a mutation applied; restore it")
    print("first (the pre-run copies are in target/guard_backup/).")
    for desc, path in stale:
        print(f"  no unique anchor for: {desc}  [{path.name}]")
    return False


def main():
    only = sys.argv[1] if len(sys.argv) > 1 else None
    if not preflight():
        return 2
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
