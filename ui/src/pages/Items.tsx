import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { newId } from "../lib";

interface Item {
  id: string;
  name: string;
  sku: string;
  unit: string;
}

export function Items() {
  const [items, setItems] = useState<Item[]>([]);
  const [sku, setSku] = useState("");
  const [name, setName] = useState("");
  const [unit, setUnit] = useState("ea");
  const [error, setError] = useState("");
  const [submitting, setSubmitting] = useState(false);

  const refresh = async () => {
    try {
      setItems(await invoke<Item[]>("list_items"));
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
      await invoke("create_item", {
        input: { id: newId(), sku, name, unit },
      });
      setSku("");
      setName("");
      setUnit("ea");
      await refresh();
    } catch (e: unknown) {
      setError(String(e));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div>
      <h1>Items</h1>

      <section className="panel">
        <h2>Add Item</h2>
        <form onSubmit={submit} className="form">
          <label>
            SKU
            <input value={sku} onChange={(e) => setSku(e.target.value)} required />
          </label>
          <label>
            Name
            <input value={name} onChange={(e) => setName(e.target.value)} required />
          </label>
          <label>
            Unit
            <input value={unit} onChange={(e) => setUnit(e.target.value)} required />
          </label>
          <button type="submit" disabled={submitting}>
            {submitting ? "Adding..." : "Add Item"}
          </button>
        </form>
        {error && <p className="error">{error}</p>}
      </section>

      <h2>All Items</h2>
      {items.length === 0 ? (
        <p className="muted">No items yet.</p>
      ) : (
        <table>
          <thead>
            <tr>
              <th>SKU</th>
              <th>Name</th>
              <th>Unit</th>
              <th>ID</th>
            </tr>
          </thead>
          <tbody>
            {items.map((i) => (
              <tr key={i.id}>
                <td>{i.sku}</td>
                <td>{i.name}</td>
                <td>{i.unit}</td>
                <td className="mono">{i.id}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
