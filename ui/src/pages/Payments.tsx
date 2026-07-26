import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { majorToMinor, newId, today, errorMessage } from "../lib";
import { useToast } from "../theme";
import { useI18n } from "../i18n";
import { useCurrency } from "../settings";

type Direction = "in" | "out";

interface Party {
  id: string;
  name: string;
  kind: string;
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

  const eligible =
    direction === "in"
      ? parties.filter((p) => p.kind === "customer" || p.kind === "both")
      : parties.filter((p) => p.kind === "supplier" || p.kind === "both");

  const partyName = (id: string) => parties.find((p) => p.id === id)?.name ?? id;

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setError("");
    setSubmitting(true);
    try {
      const command = direction === "in" ? "record_payment" : "record_payment_made";
      const input =
        direction === "in"
          ? { id: newId(), customer_id: partyId, amount_minor: majorToMinor(amountMajor), date, allocations: [] }
          : { id: newId(), supplier_id: partyId, amount_minor: majorToMinor(amountMajor), date, allocations: [] };
      await invoke(command, { input });
      toast.push(direction === "in" ? t.payments.added : t.payments.paidMade);
      setPartyId("");
      setAmountMajor("");
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
              {t.payments.recordTitle}
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
                  <option key={p.id} value={p.id}>{p.name}</option>
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
                <th>{t.payments.recordTitle}</th>
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
