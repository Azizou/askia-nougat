import { useEffect, useState } from "react";
import { Dashboard } from "./pages/Dashboard";
import { Items } from "./pages/Items";
import { Parties } from "./pages/Parties";
import { Purchases } from "./pages/Purchases";
import { Sales } from "./pages/Sales";
import { Payments } from "./pages/Payments";
import { Faq } from "./pages/Faq";
import { useTheme } from "./theme";
import { useI18n } from "./i18n";

type Page = "dashboard" | "items" | "parties" | "purchases" | "sales" | "payments" | "faq";

const NAV_ICONS: Record<Page, string> = {
  dashboard: "📊",
  items: "📦",
  parties: "👥",
  purchases: "🛒",
  sales: "💰",
  payments: "💳",
  faq: "❓",
};

const NAV_ORDER: Page[] = ["dashboard", "items", "parties", "purchases", "sales", "payments", "faq"];

const SIDEBAR_KEY = "accounting.sidebar.collapsed";

function App() {
  const [page, setPage] = useState<Page>("dashboard");
  const [collapsed, setCollapsed] = useState<boolean>(() => {
    return window.localStorage.getItem(SIDEBAR_KEY) === "1";
  });
  const { label, icon, cycle } = useTheme();
  const { t, localeLabel, cycleLocale } = useI18n();

  useEffect(() => {
    window.localStorage.setItem(SIDEBAR_KEY, collapsed ? "1" : "0");
  }, [collapsed]);

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
            onClick={cycleLocale}
            title={t.app.switchLocale}
          >
            <span className="nav-icon">🌐</span>
            <span className="nav-label">{localeLabel}</span>
          </button>
          <button
            className="sidebar-footer-btn"
            onClick={cycle}
            title={t.app.cycleTheme}
          >
            <span className="nav-icon">{icon}</span>
            <span className="nav-label">{label}</span>
          </button>
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
