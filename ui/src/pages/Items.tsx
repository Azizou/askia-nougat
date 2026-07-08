import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { newId , errorMessage } from "../lib";
import { useToast } from "../theme";
import { useI18n } from "../i18n";

interface Item {
  id: string;
  name: string;
  sku: string;
  unit: string;
}

export function Items() {
  const { t } = useI18n();
  const [items, setItems] = useState<Item[]>([]);
  const [open, setOpen] = useState(false);
  const [sku, setSku] = useState("");
  const [name, setName] = useState("");
  const [unit, setUnit] = useState("ea");
  const [error, setError] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const skuInput = useRef<HTMLInputElement | null>(null);
  const toast = useToast();

  const refresh = async () => {
    try {
      setItems(await invoke<Item[]>("list_items"));
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
        setTimeout(() => skuInput.current?.focus(), 100);
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
      await invoke("create_item", {
        input: { id: newId(), sku, name, unit },
      });
      setSku("");
      setName("");
      setUnit("ea");
      setOpen(false);
      toast.push(t.items.added);
      await refresh();
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
        <h1>{t.items.title}</h1>
        <span className="shortcut-hint">{t.common.shortcutHint}</span>
      </div>

      <section className="panel">
        <div className="panel-header" onClick={() => setOpen((o) => !o)}>
          <h2>{open ? t.items.addNew : `${items.length} ${t.items.countSuffix}`}</h2>
          <button
            className="add-btn icon-only"
            onClick={(e) => {
              e.stopPropagation();
              setOpen((o) => !o);
            }}
            title={open ? t.items.close : t.items.addTooltip}
          >
            {open ? "×" : "+"}
          </button>
        </div>
        <div className={`form-collapse${open ? " open" : ""}`}>
          <form onSubmit={submit} className="form">
            <div className="form-row">
              <label>
                {t.items.sku}
                <input
                  ref={skuInput}
                  value={sku}
                  onChange={(e) => setSku(e.target.value)}
                  required
                />
              </label>
              <label>
                {t.items.name}
                <input value={name} onChange={(e) => setName(e.target.value)} required />
              </label>
              <label>
                {t.items.unit}
                <input value={unit} onChange={(e) => setUnit(e.target.value)} required />
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
                {submitting ? t.common.adding : t.items.add}
              </button>
            </div>
            {error && <p className="error">{error}</p>}
          </form>
        </div>
      </section>

      {items.length === 0 ? (
        <div className="table-wrap">
          <div className="empty">{t.items.empty}</div>
        </div>
      ) : (
        <div className="table-wrap">
          <table>
            <thead>
              <tr>
                <th>{t.items.sku}</th>
                <th>{t.items.name}</th>
                <th>{t.items.unit}</th>
                <th>{t.common.id}</th>
              </tr>
            </thead>
            <tbody>
              {items.map((i) => (
                <tr key={i.id}>
                  <td>{i.sku}</td>
                  <td>{i.name}</td>
                  <td>{i.unit}</td>
                  <td className="mono">{i.id.slice(0, 8)}...</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
