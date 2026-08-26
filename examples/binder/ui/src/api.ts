// Thin client for the binder:app HTTP API. The token lives in localStorage so a
// reload keeps you signed in.
let token: string | null = localStorage.getItem("binder-tok");

export const hasToken = () => !!token;
export function setToken(t: string | null) {
  token = t;
  if (t) localStorage.setItem("binder-tok", t);
  else localStorage.removeItem("binder-tok");
}

export async function api<T = any>(path: string, method = "GET", body?: unknown) {
  const headers: Record<string, string> = { "content-type": "application/json" };
  if (token) headers.authorization = `Bearer ${token}`;
  const r = await fetch(`/api${path}`, { method, headers, body: body ? JSON.stringify(body) : undefined });
  const data = (await r.json().catch(() => ({}))) as T;
  return { ok: r.ok, status: r.status, data };
}

// ---- what the API answers with ---------------------------------------------

export type Card = {
  id: string; name: string; set_name: string; set_code: string; number: string;
  rarity: string; language: string; printing: string; condition: string; graded: string;
  /** 0..100, the model's own. A card typed in by hand is 100. */
  confidence: number;
  /** Fields the AI guessed and nobody has confirmed. */
  needs_review: string[];
  /** A card is not consumed by a deck, so this is a list rather than a flag. */
  in_decks: string[];
  /** Copies still held, from the same event log the portfolio is valued from. */
  held: number;
  /** What those copies cost. */
  cost_basis_minor: number;
  /** The market price, carried forward. NULL when nothing has priced it — which is
   *  not the same as zero, and the row has to be able to say so. */
  price_minor: number | null;
  /** `held × price`, or null for the same reason. */
  value_minor: number | null;
  currency?: string;
  priced_at?: number;
  /** How stale the quote is. A four-month-old price is the best information there
   *  is and also barely information. */
  price_age_days?: number;
};

/** Every field the valuation computed for that instant — not just a height. */
export type Point = {
  at: number; market_value_minor: number; cost_basis_minor: number;
  realised_minor: number; unrealised_minor: number; unquoted: number;
};

export type Portfolio = {
  cost_basis_minor: number; market_value_minor: number; unrealised_minor: number;
  realised_minor: number; currency: string;
  /** Cards nothing has priced. They are inside market value AT COST. */
  unquoted: number;
  series: Point[]; empty?: boolean;
  /** Why the collection cannot be valued, in words, or absent. One bad event must
   *  not take out every screen, so this comes back with a 200 and zeroes. */
  blocked?: string;
  /** The card to go and look at, when there is one. */
  blocked_card?: string;
  /** The window the server actually computed, so the client is not guessing. */
  since?: number; until?: number; step?: number;
  /** The first thing that ever happened, so "All" can be offered honestly. */
  earliest_event?: number;
};

export type Slot = { card_id: string; name: string; kind: string; quantity: number };
export type Deck = { name: string; slots: Slot[] };
export type DeckCheck = {
  name: string; cards: number; legal: boolean;
  illegal: { rule: string; detail: string }[];
  slots: Slot[];
  missing: { card_id: string; name: string; quantity: number; cost_minor: number | null }[];
  cost_minor: number; currency: string; unpriced: number;
};

export const KINDS = [
  "basic-pokemon", "evolved-pokemon", "trainer", "basic-energy", "special-energy",
] as const;

export const money = (minor: number, currency = "EUR") =>
  new Intl.NumberFormat(undefined, { style: "currency", currency }).format(minor / 100);
