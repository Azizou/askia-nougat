import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { majorToMinor, newId, today, errorMessage, displayPartyName, WALKIN_PARTY_ID, ANON_SUPPLIER_PARTY_ID } from "../lib";
import { useToast } from "../theme";
import { useI18n } from "../i18n";
import { useCurrency } from "../settings";

type Direction = "in" | "out";

interface Party {
  id: string;
  name: string;
  kind: string;
  active: boolean;
}

interface OpenInvoice {
  id: string;
  date: string;
  total_minor: number;
  outstanding_minor: number;
}

interface Payment {
  id: string;
  event_id: string;
  party_id: string;
  direction: string;
  amount_minor: number;
  date: string;
}

export function Payments() {
  const { t } = useI18n();
  const { format } = useCurrency();
  const [parties, setParties] = useState<Party[]>([]);
  const [payments, setPayments] = useState<Payment[]>([]);
  const [direction, setDirection] = useState<Direction>("in");
  const [partyId, setPartyId] = useState("");
  const [amountMajor, setAmountMajor] = useState("");
  const [date, setDate] = useState(today());
  const [error, setError] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [openInvoices, setOpenInvoices] = useState<OpenInvoice[]>([]);
  // Invoice id -> major-unit string the user typed. Absent or blank means
  // "apply nothing to this invoice".
  const [applied, setApplied] = useState<Record<string, string>>({});
  const toast = useToast();

  const refresh = async () => {
    try {
      const [pt, pay] = await Promise.all([
        invoke<Party[]>("list_parties"),
        invoke<Payment[]>("list_payments"),
      ]);
      setParties(pt);
      setPayments(pay);
    } catch (e: unknown) {
      setError(errorMessage(e));
    }
  };

  useEffect(() => {
    refresh();
  }, []);

  useEffect(() => {
    setApplied({});
    if (!partyId) {
      setOpenInvoices([]);
      return;
    }
    let cancelled = false;
    invoke<OpenInvoice[]>("list_open_invoices", { input: { party_id: partyId, direction } })
      .then((rows) => {
        // Guard against a stale response landing after the user has moved on.
        if (!cancelled) setOpenInvoices(rows);
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(errorMessage(e));
      });
    return () => {
      cancelled = true;
    };
  }, [partyId, direction]);

  // The seeded parties trade for cash only, so they can never hold an invoice to
  // settle; offering them here could only produce a prepayment credited to nobody,
  // which the backend refuses.
  const isSeeded = (id: string) => id === WALKIN_PARTY_ID || id === ANON_SUPPLIER_PARTY_ID;
  const eligible =
    direction === "in"
      ? parties.filter((p) => p.active && !isSeeded(p.id) && (p.kind === "customer" || p.kind === "both"))
      : parties.filter((p) => p.active && !isSeeded(p.id) && (p.kind === "supplier" || p.kind === "both"));

  const partyName = (id: string) =>
    displayPartyName(
      id,
      parties.find((p) => p.id === id)?.name ?? id,
      t.parties.walkinCustomer,
      t.parties.anonSupplier,
    );

  const allocations = () =>
    Object.entries(applied)
      .filter(([, major]) => major.trim() !== "" && Number(major) > 0)
      .map(([target_id, major]) => ({
        target_id,
        // `check_allocation_party_ownership` requires the target type to match
        // the direction: money in settles sales, money out settles purchases.
        target_type: direction === "in" ? "sale" : "purchase",
        amount_minor: majorToMinor(major),
      }));

  const allocatedMinor = allocations().reduce((sum, a) => sum + a.amount_minor, 0);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setError("");
    const amount_minor = majorToMinor(amountMajor);
    const allocs = allocations();
    const total = allocs.reduce((sum, a) => sum + a.amount_minor, 0);
    if (total > amount_minor) {
      // The backend rejects this too; catching it here avoids a round trip and
      // gives the message in the user's language.
      setError(t.payments.allocationExceeds);
      return;
    }
    setSubmitting(true);
    try {
      const command = direction === "in" ? "record_payment" : "record_payment_made";
      const base = { id: newId(), amount_minor, date, allocations: allocs };
      const input =
        direction === "in"
          ? { ...base, customer_id: partyId }
          : { ...base, supplier_id: partyId };
      await invoke(command, { input });
      toast.push(direction === "in" ? t.payments.added : t.payments.paidMade);
      setPartyId("");
      setAmountMajor("");
      setApplied({});
      setDate(today());
      await refresh();
    } catch (e: unknown) {
      setError(errorMessage(e));
      toast.push(errorMessage(e), "error");
    } finally {
      setSubmitting(false);
    }
  };

  const voidPayment = async (p: Payment) => {
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

  return (
    <div>
      <div className="page-header">
        <h1>{t.payments.title}</h1>
      </div>

      <section className="panel">
        <h2 style={{ marginTop: 0 }}>{t.payments.recordTitle}</h2>
        <form onSubmit={submit} className="form">
          <div className="form-row">
            <label>
              {t.payments.direction}
              <select value={direction} onChange={(e) => { setDirection(e.target.value as Direction); setPartyId(""); }}>
                <option value="in">{t.payments.directionReceived}</option>
                <option value="out">{t.payments.directionPaid}</option>
              </select>
            </label>
            <label>
              {direction === "in" ? t.payments.customer : t.payments.supplier}
              <select value={partyId} onChange={(e) => setPartyId(e.target.value)} required>
                <option value="">
                  {direction === "in" ? t.payments.selectCustomer : t.payments.selectSupplier}
                </option>
                {eligible.map((p) => (
                  <option key={p.id} value={p.id}>
                    {displayPartyName(p.id, p.name, t.parties.walkinCustomer, t.parties.anonSupplier)}
                  </option>
                ))}
              </select>
            </label>
            <label>
              {t.payments.amount}
              <input
                type="number"
                step="0.01"
                placeholder="0.00"
                value={amountMajor}
                onChange={(e) => setAmountMajor(e.target.value)}
                required
              />
            </label>
            <label>
              {t.payments.date}
              <input type="date" value={date} onChange={(e) => setDate(e.target.value)} required />
            </label>
          </div>

          {partyId && (
            <div className="lines">
              <div className="lines-header">
                <strong>{t.payments.allocate}</strong>
                <span className="shortcut-hint">{t.payments.allocateHint}</span>
              </div>
              {openInvoices.length === 0 ? (
                <div className="empty">{t.payments.noOpenInvoices}</div>
              ) : (
                <>
                  <table>
                    <thead>
                      <tr>
                        <th>{t.payments.invoice}</th>
                        <th>{t.common.date}</th>
                        <th className="num">{t.payments.invoiceOutstanding}</th>
                        <th className="num">{t.payments.allocateAmount}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {openInvoices.map((inv) => (
                        <tr key={inv.id}>
                          <td className="mono">{inv.id.slice(0, 8)}...</td>
                          <td>{inv.date}</td>
                          <td className="num">{format(inv.outstanding_minor)}</td>
                          <td className="num">
                            <input
                              type="number"
                              step="0.01"
                              min="0"
                              // The backend refuses to over-allocate an
                              // invoice; capping here says so before submitting.
                              max={inv.outstanding_minor / 100}
                              placeholder="0.00"
                              value={applied[inv.id] ?? ""}
                              onChange={(e) =>
                                setApplied((prev) => ({ ...prev, [inv.id]: e.target.value }))
                              }
                            />
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                  <p>
                    {t.payments.allocationTotal}: {format(allocatedMinor)}
                  </p>
                </>
              )}
            </div>
          )}

          <div className="form-actions">
            <button type="submit" className="primary" disabled={submitting}>
              {submitting ? t.common.recording : t.payments.submit}
            </button>
          </div>
          {error && <p className="error">{error}</p>}
        </form>
      </section>

      <div className="table-wrap">
        {payments.length === 0 ? (
          <div className="empty">{t.payments.noPayments}</div>
        ) : (
          <table>
            <thead>
              <tr>
                <th>{t.payments.date}</th>
                <th>{t.parties.title}</th>
                <th>{t.payments.direction}</th>
                <th className="num">{t.payments.amount}</th>
                <th>{t.common.actions}</th>
              </tr>
            </thead>
            <tbody>
              {payments.map((p) => (
                <tr key={p.id}>
                  <td>{p.date}</td>
                  <td>{partyName(p.party_id)}</td>
                  <td>
                    <span className="badge">
                      {p.direction === "in" ? t.payments.directionReceived : t.payments.directionPaid}
                    </span>
                  </td>
                  <td className="num">{format(p.amount_minor)}</td>
                  <td>
                    <button className="ghost" onClick={() => voidPayment(p)}>
                      {t.common.void}
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}
