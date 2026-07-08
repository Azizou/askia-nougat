import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { majorToMinor, newId, today , errorMessage } from "../lib";
import { useToast } from "../theme";
import { useI18n } from "../i18n";

interface Party {
  id: string;
  name: string;
  kind: string;
}

export function Payments() {
  const { t } = useI18n();
  const [parties, setParties] = useState<Party[]>([]);
  const [customerId, setCustomerId] = useState("");
  const [amountMajor, setAmountMajor] = useState("");
  const [date, setDate] = useState(today());
  const [error, setError] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const toast = useToast();

  const refresh = async () => {
    try {
      setParties(await invoke<Party[]>("list_parties"));
    } catch (e: unknown) {
      setError(errorMessage(e));
    }
  };

  useEffect(() => {
    refresh();
  }, []);

  const customers = parties.filter((p) => p.kind === "customer" || p.kind === "both");

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setError("");
    setSubmitting(true);
    try {
      await invoke("record_payment", {
        input: {
          id: newId(),
          customer_id: customerId,
          amount_minor: majorToMinor(amountMajor),
          date,
          allocations: [],
        },
      });
      toast.push(t.payments.added);
      setCustomerId("");
      setAmountMajor("");
      setDate(today());
    } catch (e: unknown) {
      setError(errorMessage(e));
      toast.push(errorMessage(e), "error");
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div>
      <div className="page-header">
        <h1>{t.payments.title}</h1>
      </div>

      <section className="panel">
        <h2 style={{ marginTop: 0 }}>{t.payments.recordTitle}</h2>
        <p className="muted" style={{ marginTop: -4, marginBottom: 12 }}>
          {t.payments.hint}
        </p>
        <form onSubmit={submit} className="form">
          <div className="form-row">
            <label>
              {t.payments.customer}
              <select
                value={customerId}
                onChange={(e) => setCustomerId(e.target.value)}
                required
              >
                <option value="">{t.payments.selectCustomer}</option>
                {customers.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.name}
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
              <input
                type="date"
                value={date}
                onChange={(e) => setDate(e.target.value)}
                required
              />
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
    </div>
  );
}
