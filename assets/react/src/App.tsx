// React track shell: header + sidebar + theme toggle + routing.
//
// The theme logic is unchanged from the scaffold: `unified-theme`
// in localStorage, the same key as the Handlebars track, with the
// `.dark` class on <html>. The sidebar collapses on desktop and
// becomes a drawer with a backdrop on small screens.

import { useEffect, useState } from "react";
import { Link, NavLink, Route, Routes, useLocation } from "react-router-dom";
import { applyTheme, initialTheme, watchSystemTheme, type Theme } from "./theme";
import { fetchEnabledPostTypes, itemsFromData, type PostTypeDefinition } from "./api";
import { useLoad } from "./hooks";
import DashboardPage from "./pages/DashboardPage";
import LoginPage from "./pages/LoginPage";
import AdminPage from "./pages/AdminPage";
import MediaPage from "./pages/MediaPage";
import ModuleDetailPage from "./pages/ModuleDetailPage";
import ContentTypesPage from "./pages/ContentTypesPage";
import CustomEntriesPage from "./pages/CustomEntriesPage";
import MapsPage from "./pages/MapsPage";

const NAV_ITEMS = [
  { to: "/", label: "Dashboard", end: true },
  { to: "/media", label: "Media", end: true },
  { to: "/maps", label: "Maps", end: true },
  { to: "/admin", label: "Admin", end: false },
  { to: "/content/types", label: "Content types", end: false },
  { to: "/login", label: "Sign in", end: true },
] as const;

function titleForPath(pathname: string): string {
  if (pathname === "/") return "Dashboard";
  if (pathname === "/media") return "Media library";
  if (pathname === "/maps") return "Maps";
  if (pathname === "/login") return "Sign in";
  if (pathname.startsWith("/admin")) return "Admin";
  if (pathname.startsWith("/content/types")) return "Content types";
  if (pathname.startsWith("/content/custom/")) return "Content";
  if (pathname.startsWith("/modules/")) return "Module";
  return "Unified";
}

function NotFoundPage() {
  return (
    <div className="rounded-lg border border-border bg-surface p-6 shadow-md">
      <h2 className="text-xl font-bold">Page not found</h2>
      <p className="mt-2 text-muted">
        This React-track page does not exist. The dashboard and admin pages
        live under <code className="font-mono text-sm">/react/</code>.
      </p>
      <p className="mt-4">
        <Link to="/" className="text-accent hover:underline">
          ← Back to the dashboard
        </Link>
      </p>
    </div>
  );
}

export default function App() {
  const [theme, setTheme] = useState<Theme>(() => initialTheme());
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [drawerOpen, setDrawerOpen] = useState(false);
  const location = useLocation();

  useEffect(() => {
    applyTheme(theme, false);
  }, [theme]);

  useEffect(() => watchSystemTheme(setTheme), []);

  // A new route closes the mobile drawer.
  useEffect(() => setDrawerOpen(false), [location.pathname]);

  const toggleTheme = () => {
    const next: Theme = theme === "dark" ? "light" : "dark";
    setTheme(next);
    applyTheme(next);
  };

  const toggleSidebar = () => {
    if (window.innerWidth < 768) {
      setDrawerOpen((open) => !open);
    } else {
      setSidebarOpen((open) => !open);
    }
  };

  // Auto-appearing content menu: one nav link per enabled custom
  // post type with show_in_menu, ordered by menu_position. A
  // failed load (for example signed out) renders no extra links
  // instead of fabricated entries.
  const contentTypes = useLoad(
    () =>
      fetchEnabledPostTypes().then((page) =>
        itemsFromData<PostTypeDefinition>(page)
          .filter((type) => type.show_in_menu)
          .sort((a, b) => a.menu_position - b.menu_position),
      ),
    [],
  );
  const contentNav =
    contentTypes.status === "ready" ? contentTypes.data : [];

  const nav = (
    <>
      <div className="flex h-15 items-center border-b border-border px-4">
        <Link to="/" className="text-lg font-bold text-accent">
          Unified
        </Link>
      </div>
      <nav className="flex flex-col gap-1 p-3" aria-label="Primary">
        {NAV_ITEMS.map((item) => (
          <NavLink
            key={item.to}
            to={item.to}
            end={item.end}
            className={({ isActive }) =>
              `rounded-md px-3 py-2 text-sm font-medium ${
                isActive
                  ? "bg-accent-soft text-accent"
                  : "text-muted hover:bg-surface-hover hover:text-text"
              }`
            }
          >
            {item.label}
          </NavLink>
        ))}
        {contentNav.length > 0 && (
          <>
            <p className="mt-3 px-3 text-xs font-semibold uppercase tracking-wide text-faint">
              Content
            </p>
            {contentNav.map((type) => (
              <NavLink
                key={type.slug}
                to={`/content/custom/${type.slug}`}
                className={({ isActive }) =>
                  `rounded-md px-3 py-2 text-sm font-medium ${
                    isActive
                      ? "bg-accent-soft text-accent"
                      : "text-muted hover:bg-surface-hover hover:text-text"
                  }`
                }
              >
                {type.plural_label}
              </NavLink>
            ))}
          </>
        )}
      </nav>
      <div className="mt-auto space-y-2 border-t border-border p-3 text-xs text-faint">
        <p>
          React track — the classic dashboard lives at{" "}
          <a href="/dashboard" className="text-accent hover:underline">
            /dashboard
          </a>
          .
        </p>
      </div>
    </>
  );

  return (
    <div className="flex h-full bg-bg text-text">
      {sidebarOpen && (
        <aside className="hidden w-64 shrink-0 flex-col border-r border-border bg-surface md:flex">
          {nav}
        </aside>
      )}
      {drawerOpen && (
        <div className="fixed inset-0 z-40 md:hidden">
          <button
            type="button"
            aria-label="Close navigation"
            className="absolute inset-0 bg-black/50"
            onClick={() => setDrawerOpen(false)}
          />
          <aside className="absolute inset-y-0 left-0 flex w-64 flex-col border-r border-border bg-surface">
            {nav}
          </aside>
        </div>
      )}

      <div className="flex min-w-0 flex-1 flex-col">
        <header className="flex h-15 shrink-0 items-center gap-3 border-b border-border bg-surface px-4">
          <button
            type="button"
            onClick={toggleSidebar}
            className="rounded-md border border-border px-2 py-1 text-sm text-muted hover:bg-surface-hover"
            aria-label={sidebarOpen || drawerOpen ? "Collapse sidebar" : "Expand sidebar"}
            aria-expanded={sidebarOpen || drawerOpen}
          >
            ☰
          </button>
          <h1 className="text-lg font-semibold">{titleForPath(location.pathname)}</h1>
          <button
            type="button"
            onClick={toggleTheme}
            className="ml-auto rounded-md border border-border px-3 py-1 text-sm text-muted hover:bg-surface-hover"
            aria-label={
              theme === "dark" ? "Switch to light theme" : "Switch to dark theme"
            }
          >
            {theme === "dark" ? "☀ Light" : "☾ Dark"}
          </button>
        </header>

        <main className="flex-1 overflow-auto p-4 sm:p-6">
          <div className="mx-auto max-w-6xl">
            <Routes>
              <Route path="/" element={<DashboardPage />} />
              <Route path="/media" element={<MediaPage />} />
              <Route path="/maps" element={<MapsPage />} />
              <Route path="/login" element={<LoginPage />} />
              <Route path="/admin" element={<AdminPage />} />
              <Route path="/modules/:id" element={<ModuleDetailPage />} />
              <Route path="/content/types" element={<ContentTypesPage />} />
              <Route path="/content/custom/:typeSlug" element={<CustomEntriesPage />} />
              <Route path="*" element={<NotFoundPage />} />
            </Routes>
          </div>
        </main>
      </div>
    </div>
  );
}
