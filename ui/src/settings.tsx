import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { formatMoney, type Currency } from "./lib";

type SettingsMap = Record<string, string>;

const DEFAULTS: SettingsMap = {
  currency_symbol: "",
  currency_code: "",
  currency_decimals: "0",
  theme: "light",
  locale: "fr",
  font_size: "medium",
};

// Legacy localStorage keys migrated into the DB on first load.
const LEGACY_KEYS: Record<string, string> = {
  theme: "accounting.theme",
  locale: "accounting.locale",
};

interface SettingsCtx {
  settings: SettingsMap;
  ready: boolean;
  set: (key: string, value: string) => Promise<void>;
}

const SettingsContext = createContext<SettingsCtx | null>(null);

export function SettingsProvider({ children }: { children: ReactNode }) {
  const [settings, setSettings] = useState<SettingsMap>(DEFAULTS);
  const [ready, setReady] = useState(false);

  useEffect(() => {
    (async () => {
      try {
        const stored = await invoke<SettingsMap>("get_settings");
        const merged: SettingsMap = { ...DEFAULTS, ...stored };
        // One-time migration: if DB lacks a value but localStorage has one, seed DB.
        for (const [key, lsKey] of Object.entries(LEGACY_KEYS)) {
          if (stored[key] === undefined) {
            const legacy = window.localStorage.getItem(lsKey);
            if (legacy) {
              merged[key] = legacy;
              await invoke("set_setting", { key, value: legacy });
            }
          }
        }
        setSettings(merged);
      } catch {
        setSettings(DEFAULTS);
      } finally {
        setReady(true);
      }
    })();
  }, []);

  const set = useCallback(async (key: string, value: string) => {
    await invoke("set_setting", { key, value });
    setSettings((s) => ({ ...s, [key]: value }));
  }, []);

  const value = useMemo<SettingsCtx>(() => ({ settings, ready, set }), [settings, ready, set]);

  return <SettingsContext.Provider value={value}>{children}</SettingsContext.Provider>;
}

export function useSettings(): SettingsCtx {
  const ctx = useContext(SettingsContext);
  if (!ctx) throw new Error("useSettings must be used within SettingsProvider");
  return ctx;
}

export function useCurrency() {
  const { settings } = useSettings();
  const currency: Currency = {
    symbol: settings.currency_symbol ?? "",
    decimals: Number(settings.currency_decimals ?? "0"),
  };
  return {
    currency,
    format: (minor: number) => formatMoney(minor, currency, settings.locale),
  };
}
