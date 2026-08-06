import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { majorToMinor, newId, today , errorMessage, displayPartyName, WALKIN_PARTY_ID } from "../lib";
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

interface Sale {
  id: string;
  event_id: string;
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
  const { t } = useI18n();
  const { format } = useCurrency();
  const [sales, setSales] = useState<Sale[]>([]);
  const [parties, setParties] = useState<Party[]>([]);
  const [items, setItems] = useState<Item[]>([]);

  const [open, setOpen] = useState(false);
  const [customerId, setCustomerId] = useState("");
  const [date, setDate] = useState(today());
  const [terms, setTerms] = useState<Terms>("cash");
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

  // Cash sales default to the walk-in customer; switching to credit must drop
  // that default, because a receivable against "Walk-in Customer" names nobody
  // to collect from and the backend refuses it (D10).
  //
  // Keyed on `terms` alone, because this belongs to the *transition* between
  // terms and not to the current selection. With `customerId` in the deps the
  // effect re-fired on every change of the dropdown, so a user on cash who
  // cleared it back to the placeholder had walk-in immediately re-imposed and
  // could not get to the empty option. `setCustomerId` is called with an updater
  // so the current value is read without becoming a dependency.
  useEffect(() => {
    setCustomerId((cur) => {
      if (terms === "cash") return cur || WALKIN_PARTY_ID;
      // A named customer stays selected when switching to credit; only the
      // walk-in default is dropped.
      return cur === WALKIN_PARTY_ID ? "" : cur;
    });
  }, [terms]);

  // Archived master data stays visible in history but must not be offered for
  // new transactions.
  const customers = parties.filter((p) => p.active && (p.kind === "customer" || p.kind === "both"));
  const activeItems = items.filter((i) => i.active);

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
      // Reset to the cash default explicitly. The terms effect keys on `terms`
      // alone, so it does not re-fire when terms were already "cash" and cannot
      // restore the default for us.
      setCustomerId(WALKIN_PARTY_ID);
      setDate(today());
      setTerms("cash");
      setLines([emptyLine()]);
      setOpen(false);
      toast.push(t.sales.added);
      await refresh();
    } catch (e: unknown) {
      setError(errorMessage(e));
      toast.push(errorMessage(e), "error");
    } finally {
      setSubmitting(false);
    }
  };

  const voidSale = async (s: Sale) => {
    const reason = window.prompt(t.common.voidConfirm);
    if (!reason) return;
    try {
      await invoke("reverse_transaction", { input: { target_event_id: s.event_id, reason } });
      toast.push(t.common.voided);
      await refresh();
    } catch (e: unknown) {
      toast.push(errorMessage(e), "error");
    }
  };

  const customerName = (id: string) =>
    displayPartyName(id, parties.find((p) => p.id === id)?.name ?? id, t.parties.walkinCustomer);
  const termsLabel = (val: Terms) => (val === "cash" ? t.sales.cash : t.sales.credit);

  return (
    <div>
      <div className="page-header">
        <h1>{t.sales.title}</h1>
        <span className="shortcut-hint">{t.common.shortcutHint}</span>
      </div>

      <section className="panel">
        <div className="panel-header" onClick={() => setOpen((o) => !o)}>
          <h2>{open ? t.sales.addNew : `${sales.length} ${t.sales.countSuffix}`}</h2>
          <button
            className="add-btn icon-only"
            onClick={(e) => {
              e.stopPropagation();
              setOpen((o) => !o);
            }}
            title={open ? t.sales.close : t.sales.addTooltip}
          >
            {open ? "×" : "+"}
          </button>
        </div>
        <div className={`form-collapse${open ? " open" : ""}`}>
          <form onSubmit={submit} className="form">
            <div className="form-row">
              <label>
                {t.sales.customer}
                <select
                  value={customerId}
                  onChange={(e) => setCustomerId(e.target.value)}
                  required
                >
                  <option value="">{t.sales.selectCustomer}</option>
                  {customers.map((p) => (
                    <option key={p.id} value={p.id}>
                      {displayPartyName(p.id, p.name, t.parties.walkinCustomer)}
                    </option>
                  ))}
                </select>
              </label>
              <label>
                {t.sales.date}
                <input
                  type="date"
                  value={date}
                  onChange={(e) => setDate(e.target.value)}
                  required
                />
              </label>
              <label>
                {t.sales.terms}
                <select
                  value={terms}
                  onChange={(e) => setTerms(e.target.value as Terms)}
                >
                  <option value="cash">{t.sales.cash}</option>
                  <option value="credit">{t.sales.credit}</option>
                </select>
              </label>
            </div>

            <div className="lines">
              <div className="lines-header">
                <strong>{t.sales.lines}</strong>
                <button type="button" className="secondary" onClick={addLine}>
                  {t.sales.addLine}
                </button>
              </div>
              {lines.map((line, idx) => (
                <div className="line" key={idx}>
                  <select
                    value={line.item_id}
                    onChange={(e) => updateLine(idx, { item_id: e.target.value })}
                    required
                  >
                    <option value="">{t.sales.selectItem}</option>
                    {activeItems.map((it) => (
                      <option key={it.id} value={it.id}>
                        {it.name} ({it.sku})
                      </option>
                    ))}
                  </select>
                  <input
                    type="number"
                    step="any"
                    placeholder={t.sales.qty}
                    value={line.qty}
                    onChange={(e) => updateLine(idx, { qty: e.target.value })}
                    required
                  />
                  <input
                    type="number"
                    step="0.01"
                    placeholder={t.sales.unitPrice}
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
                    {t.sales.removeLine}
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
                {submitting ? t.common.recording : t.sales.submit}
              </button>
            </div>
            {error && <p className="error">{error}</p>}
          </form>
        </div>
      </section>

      {sales.length === 0 ? (
        <div className="table-wrap">
          <div className="empty">{t.sales.empty}</div>
        </div>
      ) : (
        <div className="table-wrap">
          <table>
            <thead>
              <tr>
                <th>{t.sales.date}</th>
                <th>{t.sales.customer}</th>
                <th>{t.sales.terms}</th>
                <th className="num">{t.sales.total}</th>
                <th className="num">{t.sales.outstanding}</th>
                <th>{t.common.actions}</th>
              </tr>
            </thead>
            <tbody>
              {sales.map((s) => (
                <tr key={s.id}>
                  <td>{s.date}</td>
                  <td>{customerName(s.customer_id)}</td>
                  <td>
                    <span className={`badge ${s.terms === "cash" ? "success" : ""}`}>
                      {termsLabel(s.terms)}
                    </span>
                  </td>
                  <td className="num">{format(s.total_minor)}</td>
                  <td className="num">
                    <span className={s.outstanding_minor > 0 ? "warn" : "ok"}>
                      {format(s.outstanding_minor)}
                    </span>
                  </td>
                  <td>
                    <button className="ghost" onClick={() => voidSale(s)}>
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
