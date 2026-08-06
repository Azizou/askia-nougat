export interface Currency {
  symbol: string;
  decimals: number;
}

// Stable IDs of the seeded shared parties (see genesis.rs). Their stored names
// are fixed English strings, so the UI translates them by ID at display time.
export const WALKIN_PARTY_ID = "party_walkin";
export const ANON_SUPPLIER_PARTY_ID = "party_anon_supplier";

// Display name for a party: the two seeded parties are localized via the
// supplied labels; every other party shows its stored name.
export function displayPartyName(
  id: string,
  storedName: string,
  walkinLabel: string,
  anonSupplierLabel?: string,
): string {
  if (id === WALKIN_PARTY_ID) return walkinLabel;
  if (id === ANON_SUPPLIER_PARTY_ID && anonSupplierLabel) return anonSupplierLabel;
  return storedName;
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
