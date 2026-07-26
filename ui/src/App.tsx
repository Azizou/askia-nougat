import { useEffect, useState } from "react";
import { Dashboard } from "./pages/Dashboard";
import { Items } from "./pages/Items";
import { Parties } from "./pages/Parties";
import { Purchases } from "./pages/Purchases";
import { Sales } from "./pages/Sales";
import { Payments } from "./pages/Payments";
import { Preferences } from "./pages/Preferences";
import { Faq } from "./pages/Faq";
import { useTheme } from "./theme";
import { useI18n } from "./i18n";
import { useSettings } from "./settings";

type Page = "dashboard" | "items" | "parties" | "purchases" | "sales" | "payments" | "preferences" | "faq";

const NAV_ICONS: Record<Page, string> = {
  dashboard: "📊",
  items: "📦",
  parties: "👥",
  purchases: "🛒",
  sales: "💰",
  payments: "💳",
  preferences: "⚙️",
  faq: "❓",
};

const NAV_ORDER: Page[] = ["dashboard", "items", "parties", "purchases", "sales", "payments", "preferences", "faq"];

const SIDEBAR_KEY = "accounting.sidebar.collapsed";

function App() {
  const [page, setPage] = useState<Page>("dashboard");
  const [collapsed, setCollapsed] = useState<boolean>(() => {
    return window.localStorage.getItem(SIDEBAR_KEY) === "1";
  });
  const { set: setTheme } = useTheme();
  const { t, setLocale } = useI18n();
  const { settings, ready } = useSettings();

  useEffect(() => {
    window.localStorage.setItem(SIDEBAR_KEY, collapsed ? "1" : "0");
  }, [collapsed]);

  useEffect(() => {
    if (!ready) return;
    const scale = settings.font_size === "small" ? 0.9 : settings.font_size === "large" ? 1.15 : 1.0;
    document.documentElement.style.setProperty("--font-scale", String(scale));
    if (settings.theme) setTheme(settings.theme as "light" | "dark" | "midnight");
    if (settings.locale) setLocale(settings.locale as "fr" | "en");
  }, [ready, settings.font_size, settings.theme, settings.locale, setTheme, setLocale]);

  return (
    <div className="app">
      <header className="header">
        <span className="header-brand">{t.app.name}</span>
      </header>

      {!collapsed && <div className="sidebar-backdrop" onClick={() => setCollapsed(true)} />}
      <nav className={`sidebar${collapsed ? " collapsed" : ""}`}>
        <ul className="sidebar-nav">
          {NAV_ORDER.map((key) => (
            <li key={key}>
              <button
                className={`nav-item${page === key ? " active" : ""}`}
                onClick={() => { setPage(key); if (window.innerWidth <= 768) setCollapsed(true); }}
                data-tooltip={t.nav[key]}
              >
                <span className="nav-icon">{NAV_ICONS[key]}</span>
                <span className="nav-label">{t.nav[key]}</span>
              </button>
            </li>
          ))}
        </ul>

        <div className="sidebar-footer">
          <button
            className="sidebar-footer-btn"
            onClick={() => setCollapsed((c) => !c)}
            title={t.app.toggleSidebar}
          >
            <span className="nav-icon">{collapsed ? "▶" : "◀"}</span>
            <span className="nav-label">{t.app.toggleSidebar}</span>
          </button>
        </div>
      </nav>

      <main className="main">
        {page === "dashboard" && <Dashboard />}
        {page === "items" && <Items />}
        {page === "parties" && <Parties />}
        {page === "purchases" && <Purchases />}
        {page === "sales" && <Sales />}
        {page === "payments" && <Payments />}
        {page === "preferences" && <Preferences />}
        {page === "faq" && <Faq />}
      </main>

      {collapsed && (
        <button
          className="fab-menu"
          onClick={() => setCollapsed(false)}
          aria-label={t.app.toggleSidebar}
        >
          ☰
        </button>
      )}
    </div>
  );
}

export default App;
