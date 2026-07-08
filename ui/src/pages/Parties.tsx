import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { newId } from "../lib";
import { useToast } from "../theme";
import { useI18n } from "../i18n";

type PartyKind = "supplier" | "customer" | "both";

interface Party {
  id: string;
  name: string;
  kind: PartyKind;
}

export function Parties() {
  const { t } = useI18n();
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
      toast.push(t.parties.added);
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

  const kindLabel = (k: PartyKind) => {
    if (k === "customer") return t.parties.customer;
    if (k === "supplier") return t.parties.supplier;
    return t.parties.both;
  };

  return (
    <div>
      <div className="page-header">
        <h1>{t.parties.title}</h1>
        <span className="shortcut-hint">{t.common.shortcutHint}</span>
      </div>

      <section className="panel">
        <div className="panel-header" onClick={() => setOpen((o) => !o)}>
          <h2>{open ? t.parties.addNew : `${parties.length} ${t.parties.countSuffix}`}</h2>
          <button
            className="add-btn icon-only"
            onClick={(e) => {
              e.stopPropagation();
              setOpen((o) => !o);
            }}
            title={open ? t.parties.close : t.parties.addTooltip}
          >
            {open ? "×" : "+"}
          </button>
        </div>
        <div className={`form-collapse${open ? " open" : ""}`}>
          <form onSubmit={submit} className="form">
            <div className="form-row">
              <label>
                {t.parties.name}
                <input
                  ref={nameInput}
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  required
                />
              </label>
              <label>
                {t.parties.kind}
                <select
                  value={kind}
                  onChange={(e) => setKind(e.target.value as PartyKind)}
                >
                  <option value="supplier">{t.parties.supplier}</option>
                  <option value="customer">{t.parties.customer}</option>
                  <option value="both">{t.parties.both}</option>
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
                {t.common.cancel}
              </button>
              <button type="submit" className="primary" disabled={submitting}>
                {submitting ? t.common.adding : t.parties.add}
              </button>
            </div>
            {error && <p className="error">{error}</p>}
          </form>
        </div>
      </section>

      {parties.length === 0 ? (
        <div className="table-wrap">
          <div className="empty">{t.parties.empty}</div>
        </div>
      ) : (
        <div className="table-wrap">
          <table>
            <thead>
              <tr>
                <th>{t.parties.name}</th>
                <th>{t.parties.kind}</th>
                <th>{t.common.id}</th>
              </tr>
            </thead>
            <tbody>
              {parties.map((p) => (
                <tr key={p.id}>
                  <td>{p.name}</td>
                  <td>
                    <span className={`badge ${kindClass(p.kind)}`}>{kindLabel(p.kind)}</span>
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
