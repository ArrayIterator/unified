export type Theme = "light" | "dark";

// Same storage key as the Handlebars track (assets/js/dashboard.ts), so a
// theme choice made in one track carries over to the other.
const THEME_KEY = "unified-theme";

function systemTheme(): Theme {
  return window.matchMedia?.("(prefers-color-scheme: dark)").matches
    ? "dark"
    : "light";
}

function storedTheme(): Theme | null {
  try {
    const value = window.localStorage.getItem(THEME_KEY);
    return value === "dark" || value === "light" ? value : null;
  } catch {
    // Private browsing or blocked storage: fall back to the OS theme.
    return null;
  }
}

// Applies the theme to <html>. Tailwind reads the `.dark` class
// (@custom-variant dark in index.css); the `data-theme` attribute and
// `theme-*` classes keep the Handlebars track's CSS in sync, since both
// tracks share the same <html> element when served side by side.
export function applyTheme(theme: Theme, persist = true): void {
  const root = document.documentElement;
  root.setAttribute("data-theme", theme);
  root.classList.toggle("dark", theme === "dark");
  root.classList.remove("theme-light", "theme-dark");
  root.classList.add(`theme-${theme}`);
  if (persist) {
    try {
      window.localStorage.setItem(THEME_KEY, theme);
    } catch {
      // Storage unavailable: the theme still applies for this page load.
    }
  }
}

export function initialTheme(): Theme {
  return storedTheme() ?? systemTheme();
}

// Follows the OS theme while the user has not chosen one, mirroring the
// Handlebars track's behaviour.
export function watchSystemTheme(onChange: (theme: Theme) => void): () => void {
  const query = window.matchMedia?.("(prefers-color-scheme: dark)");
  if (!query) return () => {};
  const handler = (event: MediaQueryListEvent) => {
    if (!storedTheme()) onChange(event.matches ? "dark" : "light");
  };
  query.addEventListener("change", handler);
  return () => query.removeEventListener("change", handler);
}
