// Thin client for the books:app HTTP API. The token lives in localStorage so a
// reload keeps you signed in.
let token: string | null = localStorage.getItem("books-tok");

export function setToken(t: string | null) {
  token = t;
  if (t) localStorage.setItem("books-tok", t);
  else localStorage.removeItem("books-tok");
}
export const hasToken = () => !!token;

export async function api<T = any>(
  path: string,
  method = "GET",
  body?: unknown,
): Promise<{ ok: boolean; status: number; data: T }> {
  const headers: Record<string, string> = { "content-type": "application/json" };
  if (token) headers.authorization = `Bearer ${token}`;
  const r = await fetch(`/api${path}`, {
    method,
    headers,
    body: body ? JSON.stringify(body) : undefined,
  });
  const data = (await r.json().catch(() => ({}))) as T;
  return { ok: r.ok, status: r.status, data };
}

// Fetch an authed file (the statements PDF) and trigger a browser download.
export async function download(path: string, filename: string) {
  const headers: Record<string, string> = {};
  if (token) headers.authorization = `Bearer ${token}`;
  const r = await fetch(`/api${path}`, { headers });
  if (!r.ok) return;
  const url = URL.createObjectURL(await r.blob());
  const a = document.createElement("a");
  a.href = url; a.download = filename; a.click();
  URL.revokeObjectURL(url);
}

// ---- types ----
export type AccType = "asset" | "liability" | "equity" | "income" | "expense";
export type Side = "debit" | "credit";
export interface Me { subject: string; roles: string[] }
export interface Account { id?: string; code: string; name: string; type: AccType }
export interface EntryLine { account: string; amount: number; side: Side }
export interface Entry { id: string; date: string; memo: string; lines: EntryLine[] }
export interface TrialRow { code: string; name: string; type: AccType; debits: number; credits: number; net: number }
export interface Trial { accounts: TrialRow[]; total_debits: number; total_credits: number; balanced: boolean }
export interface PnlRow { code: string; name: string; amount: number }
export interface Pnl { income: PnlRow[]; expenses: PnlRow[]; total_income: number; total_expenses: number; net_income: number }
export interface BalRow { code: string; name: string; amount: number }
export interface BalanceSheet {
  assets: BalRow[]; liabilities: BalRow[]; equity: BalRow[];
  total_assets: number; total_liabilities: number; total_equity: number; net_income: number; balanced: boolean;
}

export const ACC_TYPES: AccType[] = ["asset", "liability", "equity", "income", "expense"];
export const money = (cents: number) => {
  const neg = cents < 0, a = Math.abs(cents);
  return `${neg ? "-" : ""}$${Math.floor(a / 100)}.${String(a % 100).padStart(2, "0")}`;
};
export const today = () => new Date().toISOString().slice(0, 10);
