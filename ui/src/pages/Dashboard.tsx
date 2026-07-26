import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { today , errorMessage } from "../lib";
import { useI18n } from "../i18n";
import { useCurrency } from "../settings";

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
  const { t } = useI18n();
  const { format } = useCurrency();
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
      setError(errorMessage(e));
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
        <h1>{t.dashboard.title}</h1>
        <span className="shortcut-hint">
          {t.dashboard.lastRefresh}: {new Date().toLocaleTimeString()}
        </span>
      </div>

      {error && <p className="error">{error}</p>}

      {dashboard && (
        <div className="kpi-grid">
          <div className="kpi-card">
            <div className="kpi-label">{t.dashboard.inventoryValue}</div>
            <div className="kpi-value">{format(dashboard.inventory_value)}</div>
            <div className="kpi-sub">{t.dashboard.inventorySub}</div>
          </div>
          <div className="kpi-card success">
            <div className="kpi-label">{t.dashboard.accountsReceivable}</div>
            <div className="kpi-value success">
              {format(dashboard.total_receivable)}
            </div>
            <div className="kpi-sub">{t.dashboard.receivableSub}</div>
          </div>
          <div className="kpi-card warning">
            <div className="kpi-label">{t.dashboard.accountsPayable}</div>
            <div className="kpi-value warning">{format(dashboard.total_payable)}</div>
            <div className="kpi-sub">{t.dashboard.payableSub}</div>
          </div>
          <div className={`kpi-card ${dashboard.checks_passing ? "success" : "danger"}`}>
            <div className="kpi-label">{t.dashboard.integrityStatus}</div>
            <div
              className={`kpi-value text-value ${
                dashboard.checks_passing ? "success" : "danger"
              }`}
            >
              {dashboard.checks_passing
                ? `● ${t.dashboard.allPassing}`
                : `● ${t.dashboard.failed}`}
            </div>
            <div className="kpi-sub">{t.dashboard.integritySub}</div>
          </div>
        </div>
      )}

      {profit && (
        <>
          <h2>{t.dashboard.profit}</h2>
          <div className="profit-row">
            <div className="profit-cell">
              <div className="profit-label">{t.dashboard.revenue}</div>
              <div className="profit-value">{format(profit.revenue_minor)}</div>
            </div>
            <div className="profit-cell">
              <div className="profit-label">{t.dashboard.cogs}</div>
              <div className="profit-value">{format(profit.cogs_minor)}</div>
            </div>
            <div className="profit-cell">
              <div className="profit-label">{t.dashboard.grossProfit}</div>
              <div
                className={`profit-value ${
                  profit.gross_profit_minor >= 0 ? "success" : "danger"
                }`}
              >
                {format(profit.gross_profit_minor)}
              </div>
            </div>
            <div className="profit-cell">
              <div className="profit-label">{t.dashboard.netProfit}</div>
              <div
                className={`profit-value ${
                  profit.net_profit_minor >= 0 ? "success" : "danger"
                }`}
              >
                {format(profit.net_profit_minor)}
              </div>
            </div>
          </div>
        </>
      )}

      <h2>{t.dashboard.stockOnHand}</h2>
      {sortedStock.length === 0 ? (
        <div className="table-wrap">
          <div className="empty">{t.dashboard.noStock}</div>
        </div>
      ) : (
        <div className="table-wrap">
          <table>
            <thead>
              <tr>
                <th style={{ cursor: "pointer" }} onClick={() => toggleSort("item")}>
                  {t.dashboard.item}
                  {sortArrow("item")}
                </th>
                <th
                  className="num"
                  style={{ cursor: "pointer" }}
                  onClick={() => toggleSort("qty")}
                >
                  {t.dashboard.qty}
                  {sortArrow("qty")}
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
