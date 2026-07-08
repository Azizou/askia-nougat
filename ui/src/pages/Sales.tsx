import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { formatMoney, majorToMinor, newId, today } from "../lib";
import { useToast } from "../theme";

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

  const [open, setOpen] = useState(false);
  const [customerId, setCustomerId] = useState("");
  const [date, setDate] = useState(today());
  const [terms, setTerms] = useState<Terms>("credit");
  const [lines, setLines] = useState<LineDraft[]>([emptyLine()]);
  const [error, setError] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const toast = useToast();

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

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "n") {
        e.preventDefault();
        setOpen(true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
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
      setOpen(false);
      toast.push("Sale recorded.");
      await refresh();
    } catch (e: unknown) {
      setError(String(e));
      toast.push(String(e), "error");
    } finally {
      setSubmitting(false);
    }
  };

  const customerName = (id: string) => parties.find((p) => p.id === id)?.name ?? id;

  return (
    <div>
      <div className="page-header">
        <h1>Sales</h1>
        <span className="shortcut-hint">Press Ctrl+N to add new</span>
      </div>

      <section className="panel">
        <div className="panel-header" onClick={() => setOpen((o) => !o)}>
          <h2>{open ? "Record Sale" : `${sales.length} Sales`}</h2>
          <button
            className="add-btn icon-only"
            onClick={(e) => {
              e.stopPropagation();
              setOpen((o) => !o);
            }}
            title={open ? "Close" : "Record sale"}
          >
            {open ? "×" : "+"}
          </button>
        </div>
        <div className={`form-collapse${open ? " open" : ""}`}>
          <form onSubmit={submit} className="form">
            <div className="form-row">
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
                <select
                  value={terms}
                  onChange={(e) => setTerms(e.target.value as Terms)}
                >
                  <option value="cash">Cash</option>
                  <option value="credit">Credit</option>
                </select>
              </label>
            </div>

            <div className="lines">
              <div className="lines-header">
                <strong>Lines</strong>
                <button type="button" className="secondary" onClick={addLine}>
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
                    onChange={(e) =>
                      updateLine(idx, { unit_price_major: e.target.value })
                    }
                    required
                  />
                  <button
                    type="button"
                    className="ghost"
                    onClick={() => removeLine(idx)}
                    disabled={lines.length === 1}
                  >
                    Remove
                  </button>
                </div>
              ))}
            </div>

            <div className="form-actions">
              <button
                type="button"
                className="secondary"
                onClick={() => setOpen(false)}
                disabled={submitting}
              >
                Cancel
              </button>
              <button type="submit" className="primary" disabled={submitting}>
                {submitting ? "Recording..." : "Record Sale"}
              </button>
            </div>
            {error && <p className="error">{error}</p>}
          </form>
        </div>
      </section>

      {sales.length === 0 ? (
        <div className="table-wrap">
          <div className="empty">No sales yet. Click + to record one.</div>
        </div>
      ) : (
        <div className="table-wrap">
          <table>
            <thead>
              <tr>
                <th>Date</th>
                <th>Customer</th>
                <th>Terms</th>
                <th className="num">Total</th>
                <th className="num">Outstanding</th>
              </tr>
            </thead>
            <tbody>
              {sales.map((s) => (
                <tr key={s.id}>
                  <td>{s.date}</td>
                  <td>{customerName(s.customer_id)}</td>
                  <td>
                    <span className={`badge ${s.terms === "cash" ? "success" : ""}`}>
                      {s.terms}
                    </span>
                  </td>
                  <td className="num">{formatMoney(s.total_minor)}</td>
                  <td className="num">
                    <span className={s.outstanding_minor > 0 ? "warn" : "ok"}>
                      {formatMoney(s.outstanding_minor)}
                    </span>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
