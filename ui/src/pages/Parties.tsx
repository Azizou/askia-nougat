import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { newId } from "../lib";
import { useToast } from "../theme";

type PartyKind = "supplier" | "customer" | "both";

interface Party {
  id: string;
  name: string;
  kind: PartyKind;
}

export function Parties() {
  const [parties, setParties] = useState<Party[]>([]);
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [kind, setKind] = useState<PartyKind>("supplier");
  const [error, setError] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const nameInput = useRef<HTMLInputElement | null>(null);
  const toast = useToast();

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

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "n") {
        e.preventDefault();
        setOpen(true);
        setTimeout(() => nameInput.current?.focus(), 100);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
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
      setOpen(false);
      toast.push(`Party "${name}" added.`);
      await refresh();
    } catch (e: unknown) {
      setError(String(e));
      toast.push(String(e), "error");
    } finally {
      setSubmitting(false);
    }
  };

  const kindClass = (k: PartyKind) => {
    if (k === "customer") return "success";
    if (k === "supplier") return "warning";
    return "";
  };

  return (
    <div>
      <div className="page-header">
        <h1>Parties</h1>
        <span className="shortcut-hint">Press Ctrl+N to add new</span>
      </div>

      <section className="panel">
        <div className="panel-header" onClick={() => setOpen((o) => !o)}>
          <h2>{open ? "New Party" : `${parties.length} Parties`}</h2>
          <button
            className="add-btn icon-only"
            onClick={(e) => {
              e.stopPropagation();
              setOpen((o) => !o);
            }}
            title={open ? "Close" : "Add party"}
          >
            {open ? "×" : "+"}
          </button>
        </div>
        <div className={`form-collapse${open ? " open" : ""}`}>
          <form onSubmit={submit} className="form">
            <div className="form-row">
              <label>
                Name
                <input
                  ref={nameInput}
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  required
                />
              </label>
              <label>
                Kind
                <select
                  value={kind}
                  onChange={(e) => setKind(e.target.value as PartyKind)}
                >
                  <option value="supplier">Supplier</option>
                  <option value="customer">Customer</option>
                  <option value="both">Both</option>
                </select>
              </label>
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
                {submitting ? "Adding..." : "Add Party"}
              </button>
            </div>
            {error && <p className="error">{error}</p>}
          </form>
        </div>
      </section>

      {parties.length === 0 ? (
        <div className="table-wrap">
          <div className="empty">No parties yet. Click + to add one.</div>
        </div>
      ) : (
        <div className="table-wrap">
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
                  <td>
                    <span className={`badge ${kindClass(p.kind)}`}>{p.kind}</span>
                  </td>
                  <td className="mono">{p.id.slice(0, 8)}...</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
