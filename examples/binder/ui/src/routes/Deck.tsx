import { useCallback, useEffect, useState } from "react";
import { useNavigate, useParams, Link } from "react-router-dom";
import { KINDS, api, money, type DeckCheck } from "../api";
import type { Store } from "../App";

export function DeckPage({ store }: { store: Store }) {
  const name = decodeURIComponent(useParams().name ?? "");
  const navigate = useNavigate();
  const [deck, setDeck] = useState<DeckCheck | null>(null);
  const [missing404, setMissing404] = useState(false);
  const [pick, setPick] = useState({ card_id: "", quantity: "1", kind: KINDS[0] as string });

  const load = useCallback(async () => {
    const r = await api<DeckCheck>(`/decks/${encodeURIComponent(name)}`);
    if (!r.ok) { setMissing404(true); return; }
    setDeck(r.data);
  }, [name]);
  useEffect(() => { load(); }, [load]);

  // One route for "how many" and "is it in", so the two cannot disagree: quantity
  // zero removes the slot.
  const setSlot = async (card_id: string, quantity: number, kind?: string) => {
    await api(`/decks/${encodeURIComponent(name)}/slots`, "POST", { card_id, quantity, kind });
    await load();
    await store.reload();
  };

  if (missing404) return <p className="text-muted-foreground">No deck called “{name}”.</p>;
  if (!deck) return null;

  return (
    <div className="space-y-6">
      <div>
        <Link to="/decks" className="text-sm text-muted-foreground hover:text-foreground">← all decks</Link>
        <div className="flex items-baseline gap-3 mt-1">
          <h1 className="text-2xl font-semibold tracking-tight">{deck.name}</h1>
          <span className="text-sm tabular-nums text-muted-foreground">{deck.cards} cards</span>
          {deck.legal
            ? <span className="text-sm font-medium text-emerald-600">legal</span>
            : <span className="text-sm font-medium text-destructive">not legal</span>}
          <button className="ml-auto text-xs text-destructive hover:underline"
            onClick={async () => {
              // The list goes; the cards it named are still owned.
              await api("/decks", "DELETE", { name: deck.name });
              await store.reload();
              navigate("/decks");
            }}>delete deck</button>
        </div>
      </div>

      {deck.illegal.length > 0 && (
        <ul className="rounded-xl border border-destructive/40 bg-destructive/5 p-4 space-y-1 text-sm">
          {deck.illegal.map((i, n) => (
            <li key={n}><b className="font-medium">{i.rule}</b> — {i.detail}</li>
          ))}
        </ul>
      )}

      <table className="w-full text-sm">
        <thead>
          <tr className="text-left text-xs uppercase tracking-wide text-muted-foreground border-b">
            <th className="p-2 font-medium">card</th><th className="p-2 font-medium">printing</th>
            <th className="p-2 font-medium">kind</th><th className="p-2 font-medium text-right">×</th><th />
          </tr>
        </thead>
        <tbody>
          {deck.slots.length ? deck.slots.map((s) => (
            <tr key={s.card_id} className="border-b">
              <td className="p-2">{s.name}</td>
              <td className="p-2 text-muted-foreground">{s.card_id}</td>
              <td className="p-1">
                <select className="rounded border bg-background px-2 py-1 text-sm" value={s.kind}
                  onChange={(e) => setSlot(s.card_id, s.quantity, e.target.value)}>
                  {KINDS.map((k) => <option key={k}>{k}</option>)}
                </select>
              </td>
              <td className="p-1 text-right">
                <input type="number" min={0} value={s.quantity} className="w-16 rounded border bg-background px-2 py-1 text-sm text-right"
                  onChange={(e) => setSlot(s.card_id, Number(e.target.value), s.kind)} />
              </td>
              <td className="p-1">
                <button className="text-destructive text-xs" title="remove from this deck"
                  onClick={() => setSlot(s.card_id, 0)}>✕</button>
              </td>
            </tr>
          )) : <tr><td colSpan={5} className="p-4 text-muted-foreground">Empty — add a card below.</td></tr>}
        </tbody>
      </table>

      <form className="flex flex-wrap gap-2" onSubmit={(e) => {
        e.preventDefault();
        if (pick.card_id) setSlot(pick.card_id, Number(pick.quantity), pick.kind);
      }}>
        <select className="rounded-md border bg-background px-2 py-1.5 text-sm min-w-64" value={pick.card_id}
          onChange={(e) => setPick({ ...pick, card_id: e.target.value })}>
          <option value="">choose a card from your collection…</option>
          {store.cards.map((c) => (
            <option key={c.id} value={c.id}>{c.name} — {c.set_name || c.set_code || "?"} {c.number}</option>
          ))}
        </select>
        <input type="number" min={1} className="w-16 rounded-md border bg-background px-2 py-1.5 text-sm"
          value={pick.quantity} onChange={(e) => setPick({ ...pick, quantity: e.target.value })} />
        <select className="rounded-md border bg-background px-2 py-1.5 text-sm" value={pick.kind}
          onChange={(e) => setPick({ ...pick, kind: e.target.value })}>
          {KINDS.map((k) => <option key={k}>{k}</option>)}
        </select>
        <button className="rounded-md bg-primary text-primary-foreground px-3 py-1.5 text-sm font-medium">
          Add to deck
        </button>
      </form>

      <div>
        <h2 className="text-base font-medium mb-2">Still to buy</h2>
        {deck.missing.length ? (
          <>
            <p className="text-sm mb-2">
              <b className="tabular-nums">{money(deck.cost_minor, deck.currency)}</b>
              {/* Named, because a shopping list that quietly omits the unpriced cards
                  is a total that is too low. */}
              {deck.unpriced > 0 && (
                <span className="text-muted-foreground"> + {deck.unpriced} card(s) nothing has priced</span>
              )}
            </p>
            <table className="w-full text-sm">
              <thead>
                <tr className="text-left text-xs uppercase tracking-wide text-muted-foreground border-b">
                  <th className="p-2 font-medium">card</th><th className="p-2 font-medium">printing</th>
                  <th className="p-2 font-medium text-right">need</th><th className="p-2 font-medium text-right">cost</th>
                </tr>
              </thead>
              <tbody>
                {deck.missing.map((m) => (
                  <tr key={m.card_id} className="border-b">
                    <td className="p-2">{m.name}</td>
                    <td className="p-2 text-muted-foreground">{m.card_id}</td>
                    <td className="p-2 text-right tabular-nums">{m.quantity}</td>
                    <td className="p-2 text-right tabular-nums">
                      {m.cost_minor === null
                        ? <span className="text-muted-foreground">unpriced</span>
                        : money(m.cost_minor, deck.currency)}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </>
        ) : <p className="text-sm text-muted-foreground">You own every card in it.</p>}
      </div>
    </div>
  );
}
