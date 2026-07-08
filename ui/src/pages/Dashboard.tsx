import { useEffect, useState } from "react";
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

export function Dashboard() {
  const [dashboard, setDashboard] = useState<DashboardData | null>(null);
  const [stock, setStock] = useState<StockRow[]>([]);
  const [profit, setProfit] = useState<ProfitData | null>(null);
  const [error, setError] = useState<string>("");

  const refresh = async () => {
    try {
      const [d, s, p] = await Promise.all([
        invoke<DashboardData>("get_dashboard"),
        invoke<StockRow[]>("get_stock"),
        invoke<ProfitData>("get_profit", { anchor: today() }),
      ]);
      setDashboard(d);
      setStock(s);
      setProfit(p);
      setError("");
    } catch (e: unknown) {
      setError(String(e));
    }
  };

  useEffect(() => {
    refresh();
  }, []);

  return (
    <div>
      <h1>Dashboard</h1>
      {error && <p className="error">{error}</p>}

      {dashboard && (
        <div className="dashboard">
          <div className="card">
            <div className="label">Inventory Value</div>
            <div className="value">{formatMoney(dashboard.inventory_value)}</div>
          </div>
          <div className="card">
            <div className="label">Accounts Receivable</div>
            <div className="value">{formatMoney(dashboard.total_receivable)}</div>
          </div>
          <div className="card">
            <div className="label">Accounts Payable</div>
            <div className="value">{formatMoney(dashboard.total_payable)}</div>
          </div>
          <div className="card">
            <div className="label">Integrity Checks</div>
            <div className={`value ${dashboard.checks_passing ? "ok" : "warn"}`}>
              {dashboard.checks_passing ? "All Passing" : "FAILED"}
            </div>
          </div>
        </div>
      )}

      {profit && (
        <>
          <h2>Profit (Last 6 Months)</h2>
          <div className="dashboard">
            <div className="card">
              <div className="label">Revenue</div>
              <div className="value">{formatMoney(profit.revenue_minor)}</div>
            </div>
            <div className="card">
              <div className="label">COGS</div>
              <div className="value">{formatMoney(profit.cogs_minor)}</div>
            </div>
            <div className="card">
              <div className="label">Gross Profit</div>
              <div className="value ok">{formatMoney(profit.gross_profit_minor)}</div>
            </div>
            <div className="card">
              <div className="label">Net Profit</div>
              <div className="value ok">{formatMoney(profit.net_profit_minor)}</div>
            </div>
          </div>
        </>
      )}

      <h2>Stock on Hand</h2>
      {stock.length === 0 ? (
        <p className="muted">No stock recorded yet.</p>
      ) : (
        <table>
          <thead>
            <tr>
              <th>Item</th>
              <th>Qty</th>
            </tr>
          </thead>
          <tbody>
            {stock.map((s) => (
              <tr key={s.item_id}>
                <td>{s.item_id}</td>
                <td>{s.qty}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
