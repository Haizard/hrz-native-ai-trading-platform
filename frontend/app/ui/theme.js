(function () {
  const KEY = "hrz.theme";
  const media = window.matchMedia("(prefers-color-scheme: light)");
  const read = () => {
    try {
      const saved = localStorage.getItem(KEY);
      if (saved === "light" || saved === "dark" || saved === "system") return saved;
    } catch { /* private mode: the system preference still decides */ }
    return "system";
  };
  let pref = read();
  const resolve = () => (pref === "system" ? (media.matches ? "light" : "dark") : pref);
  const announce = () => {
    window.dispatchEvent(
      new CustomEvent("hrz:theme", { detail: { theme: resolve(), pref } })
    );
  };
  const apply = () => {
    document.documentElement.dataset.theme = resolve();
    announce();
  };
  // jsdom (the harness) exposes only the legacy `addListener`; a browser
  // exposes both. Calling the missing one would throw before `Theme` exists.
  const watchScheme = (callback) => {
    if (typeof media.addEventListener === "function") media.addEventListener("change", callback);
    else if (typeof media.addListener === "function") media.addListener(callback);
  };
  watchScheme(() => {
    if (pref === "system") apply();
  });
  window.Theme = {
    current: resolve,
    preference: () => pref,
    set(next) {
      pref = next === "light" || next === "dark" ? next : "system";
      try { localStorage.setItem(KEY, pref); } catch { /* the theme still applies */ }
      apply();
    },
    onChange(callback) {
      window.addEventListener("hrz:theme", (event) => callback(event.detail.theme));
      callback(resolve());
    },
  };
  apply();
})();
