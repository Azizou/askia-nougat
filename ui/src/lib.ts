export interface Currency {
  symbol: string;
  decimals: number;
}

// Stable ID of the seeded shared cash-sale party (see genesis.rs). Its stored
// name is a fixed English string, so the UI translates it by ID at display time.
export const WALKIN_PARTY_ID = "party_walkin";

// Display name for a party: the seeded walk-in party is localized via the
// supplied label; every other party shows its stored name.
export function displayPartyName(id: string, storedName: string, walkinLabel: string): string {
  return id === WALKIN_PARTY_ID ? walkinLabel : storedName;
}

export function formatMoney(minor: number, currency?: Currency, locale?: string): string {
  const decimals = currency?.decimals ?? 0;
  const num = (minor / 100).toLocaleString(locale ?? undefined, {
    minimumFractionDigits: decimals,
    maximumFractionDigits: decimals,
  });
  const symbol = currency?.symbol ?? "";
  return symbol ? `${symbol} ${num}` : num;
}

export function majorToMinor(major: string): number {
  const n = Number(major);
  if (!Number.isFinite(n)) throw new Error(`Invalid amount: ${major}`);
  return Math.round(n * 100);
}

export function today(): string {
  return new Date().toISOString().slice(0, 10);
}

export function newId(): string {
  return crypto.randomUUID();
}

export function errorMessage(e: unknown): string {
  if (typeof e === "string") return e;
  if (e && typeof e === "object" && "message" in e) return String((e as { message: unknown }).message);
  try { return JSON.stringify(e); } catch { return "Unknown error"; }
}
