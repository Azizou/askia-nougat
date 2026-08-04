import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { newId, errorMessage, displayPartyName, WALKIN_PARTY_ID, ANON_SUPPLIER_PARTY_ID } from "../lib";
import { useToast } from "../theme";
import { useI18n } from "../i18n";

type PartyKind = "supplier" | "customer" | "both";

interface Party {
  id: string;
  name: string;
  kind: PartyKind;
  active: boolean;
}

export function Parties() {
  const { t } = useI18n();
  const [parties, setParties] = useState<Party[]>([]);
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [kind, setKind] = useState<PartyKind>("supplier");
  const [error, setError] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [showArchived, setShowArchived] = useState(false);
  const [editing, setEditing] = useState<Party | null>(null);
  const [editName, setEditName] = useState("");
  const [editKind, setEditKind] = useState<PartyKind>("supplier");
  const nameInput = useRef<HTMLInputElement | null>(null);
  const toast = useToast();

  const visible = showArchived ? parties : parties.filter((p) => p.active);

  // The seeded cash-trade parties are auto-selected by the sales and purchases
  // forms, so the backend refuses to archive or delete them.
  const isSeeded = (id: string) => id === WALKIN_PARTY_ID || id === ANON_SUPPLIER_PARTY_ID;

  const partyLabel = (p: Party) =>
    displayPartyName(p.id, p.name, t.parties.walkinCustomer, t.parties.anonSupplier);

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
      setError(errorMessage(e));
      toast.push(errorMessage(e), "error");
    } finally {
      setSubmitting(false);
    }
  };

  const beginEdit = (p: Party) => {
    setEditing(p);
    setEditName(p.name);
    setEditKind(p.kind);
    setError("");
  };

  const saveEdit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!editing) return;
    setSubmitting(true);
    setError("");
    try {
      const changes: Record<string, string> = {};
      if (editName !== editing.name) changes.name = editName;
      if (editKind !== editing.kind) changes.kind = editKind;
      if (Object.keys(changes).length > 0) {
        await invoke("update_party", { input: { id: editing.id, changes } });
        toast.push(t.common.saved);
      }
      setEditing(null);
      await refresh();
    } catch (e: unknown) {
      setError(errorMessage(e));
      toast.push(errorMessage(e), "error");
    } finally {
      setSubmitting(false);
    }
  };

  const setArchived = async (p: Party, archived: boolean) => {
    try {
      await invoke("update_party", { input: { id: p.id, changes: { active: !archived } } });
      toast.push(archived ? t.common.archived : t.common.restored);
      await refresh();
    } catch (e: unknown) {
      toast.push(errorMessage(e), "error");
    }
  };

  const remove = async (p: Party) => {
    if (!window.confirm(t.common.deleteConfirm)) return;
    try {
      await invoke("delete_party", { input: { id: p.id } });
      toast.push(t.common.deleted);
      await refresh();
    } catch (e: unknown) {
      // The backend explains when a party has traded and must be archived
      // rather than removed.
      toast.push(errorMessage(e), "error");
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

      {editing && (
        <section className="panel">
          <h2 style={{ marginTop: 0 }}>{t.parties.editTitle}</h2>
          <form onSubmit={saveEdit} className="form">
            <div className="form-row">
              <label>
                {t.parties.name}
                <input value={editName} onChange={(e) => setEditName(e.target.value)} required />
              </label>
              <label>
                {t.parties.kind}
                <select value={editKind} onChange={(e) => setEditKind(e.target.value as PartyKind)}>
                  <option value="supplier">{t.parties.supplier}</option>
                  <option value="customer">{t.parties.customer}</option>
                  <option value="both">{t.parties.both}</option>
                </select>
              </label>
            </div>
            <div className="form-actions">
              <button type="button" className="secondary" onClick={() => setEditing(null)} disabled={submitting}>
                {t.common.cancel}
              </button>
              <button type="submit" className="primary" disabled={submitting}>
                {submitting ? t.common.saving : t.common.save}
              </button>
            </div>
            {error && <p className="error">{error}</p>}
          </form>
        </section>
      )}

      <label className="inline-toggle">
        <input
          type="checkbox"
          checked={showArchived}
          onChange={(e) => setShowArchived(e.target.checked)}
        />
        {t.common.showArchived}
      </label>

      {visible.length === 0 ? (
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
                <th>{t.common.actions}</th>
              </tr>
            </thead>
            <tbody>
              {visible.map((p) => (
                <tr key={p.id} className={p.active ? undefined : "row-archived"}>
                  <td>
                    {partyLabel(p)}
                    {!p.active && <span className="badge"> {t.common.archivedBadge}</span>}
                  </td>
                  <td>
                    <span className={`badge ${kindClass(p.kind)}`}>{kindLabel(p.kind)}</span>
                  </td>
                  <td className="mono">{p.id.slice(0, 8)}...</td>
                  <td>
                    <button className="ghost" onClick={() => beginEdit(p)}>{t.common.edit}</button>
                    {!isSeeded(p.id) && (
                      <>
                        <button className="ghost" onClick={() => setArchived(p, p.active)}>
                          {p.active ? t.common.archive : t.common.restore}
                        </button>
                        <button className="ghost" onClick={() => remove(p)}>{t.common.delete}</button>
                      </>
                    )}
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
