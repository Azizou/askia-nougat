import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { majorToMinor, newId, today, errorMessage, displayPartyName, ANON_SUPPLIER_PARTY_ID } from "../lib";
import { useToast } from "../theme";
import { useI18n } from "../i18n";
import { useCurrency } from "../settings";

type Terms = "cash" | "credit";

interface Party {
  id: string;
  name: string;
  kind: string;
  active: boolean;
}

interface Item {
  id: string;
  name: string;
  sku: string;
  unit: string;
  active: boolean;
}

interface Purchase {
  id: string;
  event_id: string;
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
  const { t } = useI18n();
  const { format } = useCurrency();
  const [purchases, setPurchases] = useState<Purchase[]>([]);
  const [parties, setParties] = useState<Party[]>([]);
  const [items, setItems] = useState<Item[]>([]);

  const [open, setOpen] = useState(false);
  const [supplierId, setSupplierId] = useState("");
  const [date, setDate] = useState(today());
  const [terms, setTerms] = useState<Terms>("cash");
  const [lines, setLines] = useState<LineDraft[]>([emptyLine()]);
  const [error, setError] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const toast = useToast();

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
      setError(errorMessage(e));
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

  // Mirrors the walk-in customer on the sales form: a cash purchase from an
  // unrecorded seller needs no named supplier, so default to the seeded one.
  // Credit must clear it — a payable to "Cash Supplier" names nobody to pay,
  // and the backend refuses it.
  useEffect(() => {
    if (terms === "cash" && !supplierId) setSupplierId(ANON_SUPPLIER_PARTY_ID);
    if (terms === "credit" && supplierId === ANON_SUPPLIER_PARTY_ID) setSupplierId("");
  }, [terms, supplierId]);

  // Archived master data stays visible in history but must not be offered for
  // new transactions.
  const suppliers = parties.filter((p) => p.active && (p.kind === "supplier" || p.kind === "both"));
  const activeItems = items.filter((i) => i.active);

  const supplierName = (id: string) =>
    displayPartyName(
      id,
      parties.find((p) => p.id === id)?.name ?? id,
      t.parties.walkinCustomer,
      t.parties.anonSupplier,
    );

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
      setTerms("cash");
      setLines([emptyLine()]);
      setOpen(false);
      toast.push(t.purchases.added);
      await refresh();
    } catch (e: unknown) {
      setError(errorMessage(e));
      toast.push(errorMessage(e), "error");
    } finally {
      setSubmitting(false);
    }
  };

  const voidPurchase = async (p: Purchase) => {
    const reason = window.prompt(t.common.voidConfirm);
    if (!reason) return;
    try {
      await invoke("reverse_transaction", { input: { target_event_id: p.event_id, reason } });
      toast.push(t.common.voided);
      await refresh();
    } catch (e: unknown) {
      toast.push(errorMessage(e), "error");
    }
  };

  const termsLabel = (val: Terms) => (val === "cash" ? t.purchases.cash : t.purchases.credit);

  return (
    <div>
      <div className="page-header">
        <h1>{t.purchases.title}</h1>
        <span className="shortcut-hint">{t.common.shortcutHint}</span>
      </div>

      <section className="panel">
        <div className="panel-header" onClick={() => setOpen((o) => !o)}>
          <h2>
            {open ? t.purchases.addNew : `${purchases.length} ${t.purchases.countSuffix}`}
          </h2>
          <button
            className="add-btn icon-only"
            onClick={(e) => {
              e.stopPropagation();
              setOpen((o) => !o);
            }}
            title={open ? t.purchases.close : t.purchases.addTooltip}
          >
            {open ? "×" : "+"}
          </button>
        </div>
        <div className={`form-collapse${open ? " open" : ""}`}>
          <form onSubmit={submit} className="form">
            <div className="form-row">
              <label>
                {t.purchases.supplier}
                <select
                  value={supplierId}
                  onChange={(e) => setSupplierId(e.target.value)}
                  required
                >
                  <option value="">{t.purchases.selectSupplier}</option>
                  {suppliers.map((p) => (
                    <option key={p.id} value={p.id}>
                      {supplierName(p.id)}
                    </option>
                  ))}
                </select>
              </label>
              <label>
                {t.purchases.date}
                <input
                  type="date"
                  value={date}
                  onChange={(e) => setDate(e.target.value)}
                  required
                />
              </label>
              <label>
                {t.purchases.terms}
                <select
                  value={terms}
                  onChange={(e) => setTerms(e.target.value as Terms)}
                >
                  <option value="cash">{t.purchases.cash}</option>
                  <option value="credit">{t.purchases.credit}</option>
                </select>
              </label>
            </div>

            <div className="lines">
              <div className="lines-header">
                <strong>{t.purchases.lines}</strong>
                <button type="button" className="secondary" onClick={addLine}>
                  {t.purchases.addLine}
                </button>
              </div>
              {lines.map((line, idx) => (
                <div className="line" key={idx}>
                  <select
                    value={line.item_id}
                    onChange={(e) => updateLine(idx, { item_id: e.target.value })}
                    required
                  >
                    <option value="">{t.purchases.selectItem}</option>
                    {activeItems.map((it) => (
                      <option key={it.id} value={it.id}>
                        {it.name} ({it.sku})
                      </option>
                    ))}
                  </select>
                  <input
                    type="number"
                    step="any"
                    placeholder={t.purchases.qty}
                    value={line.qty}
                    onChange={(e) => updateLine(idx, { qty: e.target.value })}
                    required
                  />
                  <input
                    type="number"
                    step="0.01"
                    placeholder={t.purchases.unitCost}
                    value={line.unit_cost_major}
                    onChange={(e) =>
                      updateLine(idx, { unit_cost_major: e.target.value })
                    }
                    required
                  />
                  <button
                    type="button"
                    className="ghost"
                    onClick={() => removeLine(idx)}
                    disabled={lines.length === 1}
                  >
                    {t.purchases.removeLine}
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
                {t.common.cancel}
              </button>
              <button type="submit" className="primary" disabled={submitting}>
                {submitting ? t.common.recording : t.purchases.submit}
              </button>
            </div>
            {error && <p className="error">{error}</p>}
          </form>
        </div>
      </section>

      {purchases.length === 0 ? (
        <div className="table-wrap">
          <div className="empty">{t.purchases.empty}</div>
        </div>
      ) : (
        <div className="table-wrap">
          <table>
            <thead>
              <tr>
                <th>{t.purchases.date}</th>
                <th>{t.purchases.supplier}</th>
                <th>{t.purchases.terms}</th>
                <th className="num">{t.purchases.total}</th>
                <th className="num">{t.purchases.outstanding}</th>
                <th>{t.common.actions}</th>
              </tr>
            </thead>
            <tbody>
              {purchases.map((p) => (
                <tr key={p.id}>
                  <td>{p.date}</td>
                  <td>{supplierName(p.supplier_id)}</td>
                  <td>
                    <span className={`badge ${p.terms === "cash" ? "success" : ""}`}>
                      {termsLabel(p.terms)}
                    </span>
                  </td>
                  <td className="num">{format(p.total_minor)}</td>
                  <td className="num">
                    <span
                      className={
                        p.outstanding_minor > 0 ? "warn" : "ok"
                      }
                    >
                      {format(p.outstanding_minor)}
                    </span>
                  </td>
                  <td>
                    <button className="ghost" onClick={() => voidPurchase(p)}>
                      {t.common.void}
                    </button>
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
