import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { formatMoney, majorToMinor, newId, today } from "../lib";

type Terms = "cash" | "credit";

interface Party {
  id: string;
  name: string;
  kind: string;
}

interface Item {
  id: string;
  name: string;
  sku: string;
  unit: string;
}

interface Sale {
  id: string;
  customer_id: string;
  date: string;
  terms: Terms;
  total_minor: number;
  outstanding_minor: number;
}

interface LineDraft {
  item_id: string;
  qty: string;
  unit_price_major: string;
}

const emptyLine = (): LineDraft => ({ item_id: "", qty: "", unit_price_major: "" });

export function Sales() {
  const [sales, setSales] = useState<Sale[]>([]);
  const [parties, setParties] = useState<Party[]>([]);
  const [items, setItems] = useState<Item[]>([]);

  const [customerId, setCustomerId] = useState("");
  const [date, setDate] = useState(today());
  const [terms, setTerms] = useState<Terms>("credit");
  const [lines, setLines] = useState<LineDraft[]>([emptyLine()]);
  const [error, setError] = useState("");
  const [submitting, setSubmitting] = useState(false);

  const refresh = async () => {
    try {
      const [s, pt, it] = await Promise.all([
        invoke<Sale[]>("list_sales"),
        invoke<Party[]>("list_parties"),
        invoke<Item[]>("list_items"),
      ]);
      setSales(s);
      setParties(pt);
      setItems(it);
    } catch (e: unknown) {
      setError(String(e));
    }
  };

  useEffect(() => {
    refresh();
  }, []);

  const customers = parties.filter((p) => p.kind === "customer" || p.kind === "both");

  const updateLine = (idx: number, patch: Partial<LineDraft>) => {
    setLines((ls) => ls.map((l, i) => (i === idx ? { ...l, ...patch } : l)));
  };

  const addLine = () => setLines((ls) => [...ls, emptyLine()]);
  const removeLine = (idx: number) =>
    setLines((ls) => (ls.length === 1 ? ls : ls.filter((_, i) => i !== idx)));

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setError("");
    setSubmitting(true);
    try {
      const parsed = lines.map((l) => ({
        item_id: l.item_id,
        qty: Number(l.qty),
        unit_price_minor: majorToMinor(l.unit_price_major),
      }));
      await invoke("record_sale", {
        input: {
          id: newId(),
          customer_id: customerId,
          date,
          terms,
          lines: parsed,
        },
      });
      setCustomerId("");
      setDate(today());
      setTerms("credit");
      setLines([emptyLine()]);
      await refresh();
    } catch (e: unknown) {
      setError(String(e));
    } finally {
      setSubmitting(false);
    }
  };

  const customerName = (id: string) => parties.find((p) => p.id === id)?.name ?? id;

  return (
    <div>
      <h1>Sales</h1>

      <section className="panel">
        <h2>Record Sale</h2>
        <form onSubmit={submit} className="form">
          <label>
            Customer
            <select
              value={customerId}
              onChange={(e) => setCustomerId(e.target.value)}
              required
            >
              <option value="">Select customer...</option>
              {customers.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.name}
                </option>
              ))}
            </select>
          </label>
          <label>
            Date
            <input
              type="date"
              value={date}
              onChange={(e) => setDate(e.target.value)}
              required
            />
          </label>
          <label>
            Terms
            <select value={terms} onChange={(e) => setTerms(e.target.value as Terms)}>
              <option value="cash">Cash</option>
              <option value="credit">Credit</option>
            </select>
          </label>

          <div className="lines">
            <div className="lines-header">
              <strong>Lines</strong>
              <button type="button" onClick={addLine}>
                + Add Line
              </button>
            </div>
            {lines.map((line, idx) => (
              <div className="line" key={idx}>
                <select
                  value={line.item_id}
                  onChange={(e) => updateLine(idx, { item_id: e.target.value })}
                  required
                >
                  <option value="">Item...</option>
                  {items.map((it) => (
                    <option key={it.id} value={it.id}>
                      {it.name} ({it.sku})
                    </option>
                  ))}
                </select>
                <input
                  type="number"
                  step="any"
                  placeholder="Qty"
                  value={line.qty}
                  onChange={(e) => updateLine(idx, { qty: e.target.value })}
                  required
                />
                <input
                  type="number"
                  step="0.01"
                  placeholder="Unit price"
                  value={line.unit_price_major}
                  onChange={(e) => updateLine(idx, { unit_price_major: e.target.value })}
                  required
                />
                <button
                  type="button"
                  onClick={() => removeLine(idx)}
                  disabled={lines.length === 1}
                >
                  Remove
                </button>
              </div>
            ))}
          </div>

          <button type="submit" disabled={submitting}>
            {submitting ? "Recording..." : "Record Sale"}
          </button>
        </form>
        {error && <p className="error">{error}</p>}
      </section>

      <h2>All Sales</h2>
      {sales.length === 0 ? (
        <p className="muted">No sales yet.</p>
      ) : (
        <table>
          <thead>
            <tr>
              <th>Date</th>
              <th>Customer</th>
              <th>Terms</th>
              <th>Total</th>
              <th>Outstanding</th>
            </tr>
          </thead>
          <tbody>
            {sales.map((s) => (
              <tr key={s.id}>
                <td>{s.date}</td>
                <td>{customerName(s.customer_id)}</td>
                <td>{s.terms}</td>
                <td>{formatMoney(s.total_minor)}</td>
                <td>{formatMoney(s.outstanding_minor)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
