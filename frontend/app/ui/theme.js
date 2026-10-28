(function () {
  const KEY = "hrz.theme";
  let pref = "system";
  try {
    const saved = localStorage.getItem(KEY);
    if (saved === "light" || saved === "dark" || saved === "system") pref = saved;
  } catch { /* private mode: the system preference still decides */ }
  window.Theme = {
    current() {
      if (pref !== "system") return pref;
      return window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
    },
    preference: () => pref,
    set(next) {
      pref = next === "light" || next === "dark" ? next : "system";
      try { localStorage.setItem(KEY, pref); } catch { /* the theme still applies */ }
      document.documentElement.dataset.theme = this.current();
    },
  };
  document.documentElement.dataset.theme = window.Theme.current();
})();
