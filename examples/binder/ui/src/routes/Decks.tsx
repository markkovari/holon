import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { api } from "../api";
import type { Store } from "../App";

export function DecksPage({ store }: { store: Store }) {
  const [name, setName] = useState("");
  const [err, setErr] = useState("");
  const navigate = useNavigate();

  const create = async (e: React.FormEvent) => {
    e.preventDefault();
    const r = await api("/decks", "POST", { name });
    if (!r.ok) { setErr((r.data as any)?.error ?? "could not create"); return; }
    await store.reload();
    navigate(`/decks/${encodeURIComponent(name)}`);
  };

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">Decks</h1>
        <p className="text-muted-foreground text-sm mt-1">
          A deck refers to your collection — a card can be in as many decks as you like, and building one
          does not take it out of the binder. 60 cards, max 4 of a name across every printing.
        </p>
      </div>

      <form onSubmit={create} className="flex gap-2">
        <input className="rounded-md border bg-background px-3 py-1.5 text-sm" placeholder="new deck name"
          value={name} required onChange={(e) => { setName(e.target.value); setErr(""); }} />
        <button className="rounded-md bg-primary text-primary-foreground px-3 py-1.5 text-sm font-medium">
          Create
        </button>
        {err && <span className="text-sm text-destructive self-center">{err}</span>}
      </form>

      {store.decks.length ? (
        <div className="grid sm:grid-cols-2 lg:grid-cols-3 gap-3">
          {store.decks.map((d) => {
            const n = d.slots.reduce((a, s) => a + s.quantity, 0);
            return (
              <button key={d.name} onClick={() => navigate(`/decks/${encodeURIComponent(d.name)}`)}
                className="rounded-xl border bg-card p-4 text-left hover:bg-secondary/50 transition-colors">
                <div className="font-medium">{d.name}</div>
                <div className="text-sm text-muted-foreground tabular-nums">
                  {n} cards {n === 60 ? "" : <span className="text-amber-600">· not 60</span>}
                </div>
              </button>
            );
          })}
        </div>
      ) : (
        <p className="text-sm text-muted-foreground">No decks yet.</p>
      )}
    </div>
  );
}
