import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { formatMoney, today } from "../lib";

interface DashboardData {
  inventory_value: number;
  total_receivable: number;
  total_payable: number;
  checks_passing: boolean;
}

interface StockRow {
  item_id: string;
  qty: number;
}

interface ProfitData {
  revenue_minor: number;
  cogs_minor: number;
  gross_profit_minor: number;
  net_profit_minor: number;
}

interface Item {
  id: string;
  name: string;
  sku: string;
  unit: string;
}

type SortKey = "item" | "qty";

export function Dashboard() {
  const [dashboard, setDashboard] = useState<DashboardData | null>(null);
  const [stock, setStock] = useState<StockRow[]>([]);
  const [profit, setProfit] = useState<ProfitData | null>(null);
  const [items, setItems] = useState<Item[]>([]);
  const [error, setError] = useState("");
  const [sortKey, setSortKey] = useState<SortKey>("item");
  const [sortAsc, setSortAsc] = useState(true);

  const refresh = async () => {
    try {
      const [d, s, p, i] = await Promise.all([
        invoke<DashboardData>("get_dashboard"),
        invoke<StockRow[]>("get_stock"),
        invoke<ProfitData>("get_profit", { anchor: today() }),
        invoke<Item[]>("list_items"),
      ]);
      setDashboard(d);
      setStock(s);
      setProfit(p);
      setItems(i);
      setError("");
    } catch (e: unknown) {
      setError(String(e));
    }
  };

  useEffect(() => {
    refresh();
  }, []);

  const itemName = (id: string) => items.find((it) => it.id === id)?.name ?? id;

  const sortedStock = useMemo(() => {
    const copy = [...stock];
    copy.sort((a, b) => {
      let cmp = 0;
      if (sortKey === "item") cmp = itemName(a.item_id).localeCompare(itemName(b.item_id));
      else cmp = a.qty - b.qty;
      return sortAsc ? cmp : -cmp;
    });
    return copy;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [stock, items, sortKey, sortAsc]);

  const toggleSort = (key: SortKey) => {
    if (sortKey === key) setSortAsc((a) => !a);
    else {
      setSortKey(key);
      setSortAsc(true);
    }
  };

  const sortArrow = (key: SortKey) => (sortKey !== key ? "" : sortAsc ? " ↑" : " ↓");

  return (
    <div>
      <div className="page-header">
        <h1>Dashboard</h1>
        <span className="shortcut-hint">Last refresh: {new Date().toLocaleTimeString()}</span>
      </div>

      {error && <p className="error">{error}</p>}

      {dashboard && (
        <div className="kpi-grid">
          <div className="kpi-card">
            <div className="kpi-label">Inventory Value</div>
            <div className="kpi-value">{formatMoney(dashboard.inventory_value)}</div>
            <div className="kpi-sub">On-hand cost basis</div>
          </div>
          <div className="kpi-card success">
            <div className="kpi-label">Accounts Receivable</div>
            <div className="kpi-value success">
              {formatMoney(dashboard.total_receivable)}
            </div>
            <div className="kpi-sub">Owed to you</div>
          </div>
          <div className="kpi-card warning">
            <div className="kpi-label">Accounts Payable</div>
            <div className="kpi-value warning">{formatMoney(dashboard.total_payable)}</div>
            <div className="kpi-sub">You owe</div>
          </div>
          <div className={`kpi-card ${dashboard.checks_passing ? "success" : "danger"}`}>
            <div className="kpi-label">Integrity Status</div>
            <div
              className={`kpi-value text-value ${
                dashboard.checks_passing ? "success" : "danger"
              }`}
            >
              {dashboard.checks_passing ? "● All Passing" : "● FAILED"}
            </div>
            <div className="kpi-sub">Ledger consistency</div>
          </div>
        </div>
      )}

      {profit && (
        <>
          <h2>Profit (Last 6 Months)</h2>
          <div className="profit-row">
            <div className="profit-cell">
              <div className="profit-label">Revenue</div>
              <div className="profit-value">{formatMoney(profit.revenue_minor)}</div>
            </div>
            <div className="profit-cell">
              <div className="profit-label">COGS</div>
              <div className="profit-value">{formatMoney(profit.cogs_minor)}</div>
            </div>
            <div className="profit-cell">
              <div className="profit-label">Gross Profit</div>
              <div
                className={`profit-value ${
                  profit.gross_profit_minor >= 0 ? "success" : "danger"
                }`}
              >
                {formatMoney(profit.gross_profit_minor)}
              </div>
            </div>
            <div className="profit-cell">
              <div className="profit-label">Net Profit</div>
              <div
                className={`profit-value ${
                  profit.net_profit_minor >= 0 ? "success" : "danger"
                }`}
              >
                {formatMoney(profit.net_profit_minor)}
              </div>
            </div>
          </div>
        </>
      )}

      <h2>Stock on Hand</h2>
      {sortedStock.length === 0 ? (
        <div className="table-wrap">
          <div className="empty">No stock recorded yet. Record a purchase to begin.</div>
        </div>
      ) : (
        <div className="table-wrap">
          <table>
            <thead>
              <tr>
                <th style={{ cursor: "pointer" }} onClick={() => toggleSort("item")}>
                  Item{sortArrow("item")}
                </th>
                <th
                  className="num"
                  style={{ cursor: "pointer" }}
                  onClick={() => toggleSort("qty")}
                >
                  Qty{sortArrow("qty")}
                </th>
              </tr>
            </thead>
            <tbody>
              {sortedStock.map((s) => (
                <tr key={s.item_id}>
                  <td>{itemName(s.item_id)}</td>
                  <td className="num">{s.qty}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
