import { useState } from "react";
import { Dashboard } from "./pages/Dashboard";
import { Items } from "./pages/Items";
import { Parties } from "./pages/Parties";
import { Purchases } from "./pages/Purchases";
import { Sales } from "./pages/Sales";
import { Payments } from "./pages/Payments";

type Page = "dashboard" | "items" | "parties" | "purchases" | "sales" | "payments";

const NAV: { key: Page; label: string }[] = [
  { key: "dashboard", label: "Dashboard" },
  { key: "items", label: "Items" },
  { key: "parties", label: "Parties" },
  { key: "purchases", label: "Purchases" },
  { key: "sales", label: "Sales" },
  { key: "payments", label: "Payments" },
];

function App() {
  const [page, setPage] = useState<Page>("dashboard");

  return (
    <div className="layout">
      <nav className="sidebar">
        <div className="brand">Accounting</div>
        <ul>
          {NAV.map((n) => (
            <li key={n.key}>
              <button
                className={page === n.key ? "active" : ""}
                onClick={() => setPage(n.key)}
              >
                {n.label}
              </button>
            </li>
          ))}
        </ul>
      </nav>
      <main className="content">
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
