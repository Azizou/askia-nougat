import { useEffect, useState } from "react";
import { Dashboard } from "./pages/Dashboard";
import { Items } from "./pages/Items";
import { Parties } from "./pages/Parties";
import { Purchases } from "./pages/Purchases";
import { Sales } from "./pages/Sales";
import { Payments } from "./pages/Payments";
import { useTheme } from "./theme";

type Page = "dashboard" | "items" | "parties" | "purchases" | "sales" | "payments";

const NAV: { key: Page; label: string; icon: string }[] = [
  { key: "dashboard", label: "Dashboard", icon: "📊" },
  { key: "items", label: "Items", icon: "📦" },
  { key: "parties", label: "Parties", icon: "👥" },
  { key: "purchases", label: "Purchases", icon: "🛒" },
  { key: "sales", label: "Sales", icon: "💰" },
  { key: "payments", label: "Payments", icon: "💳" },
];

const SIDEBAR_KEY = "accounting.sidebar.collapsed";

function App() {
  const [page, setPage] = useState<Page>("dashboard");
  const [collapsed, setCollapsed] = useState<boolean>(() => {
    return window.localStorage.getItem(SIDEBAR_KEY) === "1";
  });
  const { theme, label, icon, cycle } = useTheme();

  useEffect(() => {
    window.localStorage.setItem(SIDEBAR_KEY, collapsed ? "1" : "0");
  }, [collapsed]);

  return (
    <div className="app" data-theme-current={theme}>
      <header className="header">
        <div className="header-left">
          <button
            className="icon-btn"
            onClick={() => setCollapsed((c) => !c)}
            aria-label="Toggle sidebar"
            title="Toggle sidebar"
          >
            ☰
          </button>
          <span className="header-brand">Accounting</span>
        </div>
        <div className="header-right">
          <button className="theme-btn" onClick={cycle} title="Cycle theme">
            <span>{icon}</span>
            <span>{label}</span>
          </button>
        </div>
      </header>

      <nav className={`sidebar${collapsed ? " collapsed" : ""}`}>
        <ul className="sidebar-nav">
          {NAV.map((n) => (
            <li key={n.key}>
              <button
                className={`nav-item${page === n.key ? " active" : ""}`}
                onClick={() => setPage(n.key)}
                data-tooltip={n.label}
              >
                <span className="nav-icon">{n.icon}</span>
                <span className="nav-label">{n.label}</span>
              </button>
            </li>
          ))}
        </ul>
      </nav>

      <main className="main">
        {page === "dashboard" && <Dashboard />}
        {page === "items" && <Items />}
        {page === "parties" && <Parties />}
        {page === "purchases" && <Purchases />}
        {page === "sales" && <Sales />}
        {page === "payments" && <Payments />}
      </main>
    </div>
  );
}

export default App;
