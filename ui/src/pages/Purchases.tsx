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

interface Purchase {
  id: string;
  supplier_id: string;
  date: string;
  terms: Terms;
  total_minor: number;
  outstanding_minor: number;
}

interface LineDraft {
  item_id: string;
  qty: string;
  unit_cost_major: string;
}

const emptyLine = (): LineDraft => ({ item_id: "", qty: "", unit_cost_major: "" });

export function Purchases() {
  const [purchases, setPurchases] = useState<Purchase[]>([]);
  const [parties, setParties] = useState<Party[]>([]);
  const [items, setItems] = useState<Item[]>([]);

  const [supplierId, setSupplierId] = useState("");
  const [date, setDate] = useState(today());
  const [terms, setTerms] = useState<Terms>("credit");
  const [lines, setLines] = useState<LineDraft[]>([emptyLine()]);
  const [error, setError] = useState("");
  const [submitting, setSubmitting] = useState(false);

  const refresh = async () => {
    try {
      const [p, pt, it] = await Promise.all([
        invoke<Purchase[]>("list_purchases"),
        invoke<Party[]>("list_parties"),
        invoke<Item[]>("list_items"),
      ]);
      setPurchases(p);
      setParties(pt);
      setItems(it);
    } catch (e: unknown) {
      setError(String(e));
    }
  };

  useEffect(() => {
    refresh();
  }, []);

  const suppliers = parties.filter((p) => p.kind === "supplier" || p.kind === "both");

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
        unit_cost_minor: majorToMinor(l.unit_cost_major),
      }));
      await invoke("record_purchase", {
        input: {
          id: newId(),
          supplier_id: supplierId,
          date,
          terms,
          lines: parsed,
        },
      });
      setSupplierId("");
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

  const supplierName = (id: string) => parties.find((p) => p.id === id)?.name ?? id;

  return (
    <div>
      <h1>Purchases</h1>

      <section className="panel">
        <h2>Record Purchase</h2>
        <form onSubmit={submit} className="form">
          <label>
            Supplier
            <select
              value={supplierId}
              onChange={(e) => setSupplierId(e.target.value)}
              required
            >
              <option value="">Select supplier...</option>
              {suppliers.map((p) => (
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
                  placeholder="Unit cost"
                  value={line.unit_cost_major}
                  onChange={(e) => updateLine(idx, { unit_cost_major: e.target.value })}
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
            {submitting ? "Recording..." : "Record Purchase"}
          </button>
        </form>
        {error && <p className="error">{error}</p>}
      </section>

      <h2>All Purchases</h2>
      {purchases.length === 0 ? (
        <p className="muted">No purchases yet.</p>
      ) : (
        <table>
          <thead>
            <tr>
              <th>Date</th>
              <th>Supplier</th>
              <th>Terms</th>
              <th>Total</th>
              <th>Outstanding</th>
            </tr>
          </thead>
          <tbody>
            {purchases.map((p) => (
              <tr key={p.id}>
                <td>{p.date}</td>
                <td>{supplierName(p.supplier_id)}</td>
                <td>{p.terms}</td>
                <td>{formatMoney(p.total_minor)}</td>
                <td>{formatMoney(p.outstanding_minor)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
