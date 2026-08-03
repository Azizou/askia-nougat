import { useToast } from "../theme";
import { useI18n } from "../i18n";
import { useSettings } from "../settings";
import { formatMoney, errorMessage } from "../lib";
import { invoke } from "@tauri-apps/api/core";
import { confirm, open } from "@tauri-apps/plugin-dialog";

const DECIMALS = ["0", "2"] as const;

export function Preferences() {
  const { t } = useI18n();
  const { settings, set } = useSettings();
  const toast = useToast();

  const update = async (key: string, value: string) => {
    await set(key, value);
    toast.push(t.preferences.saved);
  };

  const backupNow = async () => {
    const dir = await open({ directory: true, title: t.preferences.chooseFolder });
    if (typeof dir !== "string") return;
    try {
      const r = await invoke<{ at: number }>("backup_database", { destDir: dir });
      // Rust wrote these two settings; mirror them into the settings context so
      // the "last backup" line updates without a relaunch.
      await set("backup_folder", dir);
      await set("last_backup_at", String(r.at));
      toast.push(t.preferences.backupDone);
    } catch (e: unknown) {
      toast.push(errorMessage(e), "error");
    }
  };

  const restore = async () => {
    const file = await open({
      title: t.preferences.chooseFile,
      filters: [{ name: "Backup", extensions: ["db"] }],
    });
    if (typeof file !== "string") return;
    if (!(await confirm(t.preferences.restoreConfirm, { kind: "warning" }))) return;
    try {
      await invoke("restore_database", { srcPath: file });
      toast.push(t.preferences.restoreDone);
    } catch (e: unknown) {
      toast.push(errorMessage(e), "error");
    }
  };

  const exportLog = async () => {
    const dir = await open({ directory: true, title: t.preferences.chooseFolder });
    if (typeof dir !== "string") return;
    try {
      await invoke("export_event_log", { destDir: dir });
      toast.push(t.preferences.exportDone);
    } catch (e: unknown) {
      toast.push(errorMessage(e), "error");
    }
  };

  const importLog = async () => {
    const file = await open({
      title: t.preferences.chooseFile,
      filters: [{ name: "Event log", extensions: ["jsonl"] }],
    });
    if (typeof file !== "string") return;
    if (!(await confirm(t.preferences.importConfirm, { kind: "warning" }))) return;
    try {
      const r = await invoke<{ inserted: number; skipped_duplicates: number }>(
        "import_event_log",
        { srcPath: file },
      );
      toast.push(`${t.preferences.importDone} (+${r.inserted}, ${r.skipped_duplicates})`);
    } catch (e: unknown) {
      toast.push(errorMessage(e), "error");
    }
  };

  const previewCurrency = {
    symbol: settings.currency_symbol ?? "",
    decimals: Number(settings.currency_decimals ?? "0"),
  };

  return (
    <div>
      <div className="page-header">
        <h1>{t.preferences.title}</h1>
      </div>

      <section className="panel">
        <h2 style={{ marginTop: 0 }}>{t.preferences.appearance}</h2>
        <div className="form-row">
          <label>
            {t.preferences.theme}
            <select value={settings.theme} onChange={(e) => update("theme", e.target.value)}>
              <option value="light">{t.preferences.themeLight}</option>
              <option value="dark">{t.preferences.themeDark}</option>
              <option value="midnight">{t.preferences.themeMidnight}</option>
            </select>
          </label>
          <label>
            {t.preferences.fontSize}
            <select value={settings.font_size} onChange={(e) => update("font_size", e.target.value)}>
              <option value="small">{t.preferences.fontSmall}</option>
              <option value="medium">{t.preferences.fontMedium}</option>
              <option value="large">{t.preferences.fontLarge}</option>
            </select>
          </label>
        </div>
      </section>

      <section className="panel">
        <h2 style={{ marginTop: 0 }}>{t.preferences.language}</h2>
        <div className="form-row">
          <label>
            {t.preferences.language}
            <select value={settings.locale} onChange={(e) => update("locale", e.target.value)}>
              <option value="fr">Français</option>
              <option value="en">English</option>
            </select>
          </label>
        </div>
      </section>

      <section className="panel">
        <h2 style={{ marginTop: 0 }}>{t.preferences.currency}</h2>
        <div className="form-row">
          <label>
            {t.preferences.currencySymbol}
            <input
              type="text"
              value={settings.currency_symbol}
              placeholder="€"
              onChange={(e) => update("currency_symbol", e.target.value)}
            />
          </label>
          <label>
            {t.preferences.currencyCode}
            <input
              type="text"
              value={settings.currency_code}
              placeholder="EUR"
              onChange={(e) => update("currency_code", e.target.value)}
            />
          </label>
          <label>
            {t.preferences.currencyDecimals}
            <select value={settings.currency_decimals} onChange={(e) => update("currency_decimals", e.target.value)}>
              {DECIMALS.map((d) => (
                <option key={d} value={d}>{d}</option>
              ))}
            </select>
          </label>
        </div>
        <p className="muted">
          {t.preferences.currencyPreview}: {formatMoney(123456, previewCurrency)}
        </p>
      </section>

      <section className="panel">
        <h2 style={{ marginTop: 0 }}>{t.preferences.data}</h2>
        <p className="muted">
          {t.preferences.lastBackup}:{" "}
          {settings.last_backup_at
            ? new Date(Number(settings.last_backup_at)).toLocaleString(settings.locale)
            : t.preferences.neverBackedUp}
        </p>
        <div className="form-row">
          <button onClick={backupNow}>{t.preferences.backupNow}</button>
          <button onClick={restore}>{t.preferences.restore}</button>
          <button onClick={exportLog}>{t.preferences.exportLog}</button>
          <button onClick={importLog}>{t.preferences.importLog}</button>
        </div>
      </section>
    </div>
  );
}
