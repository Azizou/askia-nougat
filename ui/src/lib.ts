export function formatMoney(minor: number): string {
  return (minor / 100).toLocaleString(undefined, {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  });
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
