import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { newId } from "../lib";

type PartyKind = "supplier" | "customer" | "both";

interface Party {
  id: string;
  name: string;
  kind: PartyKind;
}

export function Parties() {
  const [parties, setParties] = useState<Party[]>([]);
  const [name, setName] = useState("");
  const [kind, setKind] = useState<PartyKind>("supplier");
  const [error, setError] = useState("");
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

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setError("");
    setSubmitting(true);
    try {
      await invoke("create_party", {
        input: { id: newId(), name, kind },
      });
      setName("");
      setKind("supplier");
      await refresh();
    } catch (e: unknown) {
      setError(String(e));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div>
      <h1>Parties</h1>

      <section className="panel">
        <h2>Add Party</h2>
        <form onSubmit={submit} className="form">
          <label>
            Name
            <input value={name} onChange={(e) => setName(e.target.value)} required />
          </label>
          <label>
            Kind
            <select value={kind} onChange={(e) => setKind(e.target.value as PartyKind)}>
              <option value="supplier">Supplier</option>
              <option value="customer">Customer</option>
              <option value="both">Both</option>
            </select>
          </label>
          <button type="submit" disabled={submitting}>
            {submitting ? "Adding..." : "Add Party"}
          </button>
        </form>
        {error && <p className="error">{error}</p>}
      </section>

      <h2>All Parties</h2>
      {parties.length === 0 ? (
        <p className="muted">No parties yet.</p>
      ) : (
        <table>
          <thead>
            <tr>
              <th>Name</th>
              <th>Kind</th>
              <th>ID</th>
            </tr>
          </thead>
          <tbody>
            {parties.map((p) => (
              <tr key={p.id}>
                <td>{p.name}</td>
                <td>{p.kind}</td>
                <td className="mono">{p.id}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
