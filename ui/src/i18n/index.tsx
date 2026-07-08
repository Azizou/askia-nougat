import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import { fr, type Translations } from "./fr";
import { en } from "./en";

export type Locale = "fr" | "en";

const LOCALES: Record<Locale, Translations> = { fr, en };
const LOCALE_KEY = "accounting.locale";
const LOCALE_LABELS: Record<Locale, string> = { fr: "Français", en: "English" };

interface I18nCtx {
  locale: Locale;
  t: Translations;
  setLocale: (l: Locale) => void;
  cycleLocale: () => void;
  localeLabel: string;
  availableLocales: { key: Locale; label: string }[];
}

const I18nContext = createContext<I18nCtx | null>(null);

function readInitialLocale(): Locale {
  if (typeof window === "undefined") return "fr";
  const stored = window.localStorage.getItem(LOCALE_KEY);
  if (stored && stored in LOCALES) return stored as Locale;
  return "fr";
}

export function I18nProvider({ children }: { children: ReactNode }) {
  const [locale, setLocaleState] = useState<Locale>(readInitialLocale);

  const setLocale = useCallback((l: Locale) => {
    setLocaleState(l);
    window.localStorage.setItem(LOCALE_KEY, l);
  }, []);

  const cycleLocale = useCallback(() => {
    setLocaleState((current) => {
      const next: Locale = current === "fr" ? "en" : "fr";
      window.localStorage.setItem(LOCALE_KEY, next);
      return next;
    });
  }, []);

  const value = useMemo<I18nCtx>(
    () => ({
      locale,
      t: LOCALES[locale],
      setLocale,
      cycleLocale,
      localeLabel: LOCALE_LABELS[locale],
      availableLocales: (Object.keys(LOCALE_LABELS) as Locale[]).map((key) => ({
        key,
        label: LOCALE_LABELS[key],
      })),
    }),
    [locale, setLocale, cycleLocale],
  );

  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

export function useI18n(): I18nCtx {
  const ctx = useContext(I18nContext);
  if (!ctx) throw new Error("useI18n must be used within I18nProvider");
  return ctx;
}

export type { Translations };
