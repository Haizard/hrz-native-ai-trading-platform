/// The client event log.
///
/// An in-memory ring of the last 200 lines, written by the paths that already
/// know something happened -- a refused request, a market channel that dropped,
/// a fill, a lost save -- and read by the dock's Console tab. Nothing is
/// persisted: a log that survives a reload is a stale log offered as evidence,
/// and the one question a console answers ("what just happened") is about this
/// session.
///
/// `hrz:log` is the only channel out. The dock listens; nothing keeps a
/// callback list it can forget to drain. `clear()` dispatches the same event
/// with a null entry, so a view repaints and a badge does not.
(() => {
  const LIMIT = 200;
  const buffer = [];
  let nextId = 1;

  function push(level, message) {
    const text = String(message == null ? "" : message).trim();
    if (!text) return null;
    const entry = { id: nextId++, at: Date.now(), level, message: text };
    buffer.push(entry);
    // Oldest first out: the newest lines are the ones a user opens the console
    // for, and a ring that dropped the newest would be the wrong way round.
    if (buffer.length > LIMIT) buffer.splice(0, buffer.length - LIMIT);
    document.dispatchEvent(new CustomEvent("hrz:log", { detail: { entry } }));
    return entry;
  }

  window.HRZLog = {
    push,
    info: (message) => push("info", message),
    warn: (message) => push("warn", message),
    error: (message) => push("error", message),
    /// A copy: the dock renders from it, and a holder of the array could
    /// otherwise mutate history the console is showing.
    entries: () => buffer.slice(),
    clear() {
      buffer.length = 0;
      document.dispatchEvent(new CustomEvent("hrz:log", { detail: { entry: null } }));
    },
  };
})();
