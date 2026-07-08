import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { majorToMinor, newId, today } from "../lib";

interface Party {
  id: string;
  name: string;
  kind: string;
}

export function Payments() {
  const [parties, setParties] = useState<Party[]>([]);
  const [customerId, setCustomerId] = useState("");
  const [amountMajor, setAmountMajor] = useState("");
  const [date, setDate] = useState(today());
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [submitting, setSubmitting] = useState(false);

  const refresh = async () => {
    try {
      setParties(await invoke<Party[]>("list_parties"));
    } catch (e: unknown) {
      setError(String(e));
    }
  };

  useEffect(() => {
    refresh();
  }, []);

  const customers = parties.filter((p) => p.kind === "customer" || p.kind === "both");

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setError("");
    setMessage("");
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
      setMessage("Payment recorded.");
      setCustomerId("");
      setAmountMajor("");
      setDate(today());
    } catch (e: unknown) {
      setError(String(e));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div>
      <h1>Payments</h1>

      <section className="panel">
        <h2>Record Payment</h2>
        <p className="muted">
          Records a customer payment as an unallocated prepayment.
        </p>
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
            Amount
            <input
              type="number"
              step="0.01"
              value={amountMajor}
              onChange={(e) => setAmountMajor(e.target.value)}
              required
            />
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
          <button type="submit" disabled={submitting}>
            {submitting ? "Recording..." : "Record Payment"}
          </button>
        </form>
        {error && <p className="error">{error}</p>}
        {message && <p className="ok">{message}</p>}
      </section>
    </div>
  );
}
