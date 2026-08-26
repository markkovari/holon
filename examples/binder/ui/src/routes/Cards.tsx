import { useRef, useState } from "react";
import { Link } from "react-router-dom";
import { api, money, type Card } from "../api";
import { downscale } from "../photo";
import type { Store } from "../App";

const EDITABLE = ["name", "set_name", "number", "printing", "condition"] as const;
const PRINTINGS = ["", "normal", "holo", "reverse holo", "1st edition", "shadowless", "special"];
const CONDITIONS = ["", "mint", "near mint", "lightly played", "moderately played", "heavily played", "damaged"];

/** A field the AI guessed and nobody has confirmed, marked every time it is shown. */
function Field({ card, k }: { card: Card; k: string }) {
  const value = (card as any)[k] as string;
  if (!card.needs_review?.includes(k)) return <>{value || "—"}</>;
  return <span className="text-amber-600 text-[13px]">{value || "—"} · check</span>;
}

function Row({ card, reload }: { card: Card; reload: () => Promise<void> }) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState<Record<string, string>>({});
  const [price, setPrice] = useState("");

  if (!editing) {
    return (
      <tr className="border-b hover:bg-secondary/50 cursor-pointer"
        onClick={() => { setDraft(Object.fromEntries(EDITABLE.map((k) => [k, (card as any)[k] ?? ""]))); setEditing(true); }}>
        <td className="p-2">
          {/* The name opens the card; the rest of the row still edits in place, so a
              quick correction does not cost a page. */}
          <Link to={`/cards/${encodeURIComponent(card.id)}`} onClick={(e) => e.stopPropagation()}
            className="hover:underline">{card.name}</Link>
        </td>
        <td className="p-2"><Field card={card} k="set_name" /></td>
        <td className="p-2"><Field card={card} k="number" /></td>
        <td className="p-2"><Field card={card} k="printing" /></td>
        <td className="p-2"><Field card={card} k="condition" /></td>
        <td className="p-2 text-right tabular-nums">{card.held || "—"}</td>
        <td className="p-2 text-right tabular-nums">
          {card.price_minor === null ? (
            <span className="text-muted-foreground">unpriced</span>
          ) : (
            <span title={card.price_age_days ? `quoted ${card.price_age_days} day(s) ago` : "quoted today"}>
              {money(card.price_minor, card.currency)}
              {/* A price older than a month is still the price, and saying how old
                  is the difference between a number and a confident number. */}
              {(card.price_age_days ?? 0) > 30 && (
                <span className="text-muted-foreground text-xs"> · {card.price_age_days}d</span>
              )}
            </span>
          )}
        </td>
        <td className="p-2 text-right tabular-nums">
          {card.value_minor === null
            ? <span className="text-muted-foreground">—</span>
            : money(card.value_minor, card.currency)}
        </td>
        <td className="p-2 text-right tabular-nums">{card.confidence}%</td>
        <td className="p-2">
          {card.in_decks?.length
            ? card.in_decks.map((d) => (
                <span key={d} className="text-xs border rounded-full px-2 py-0.5 mr-1 text-muted-foreground">{d}</span>
              ))
            : <span className="text-muted-foreground">—</span>}
        </td>
      </tr>
    );
  }

  const save = async () => {
    await api("/cards", "PATCH", { id: card.id, ...draft });
    if (price) {
      await api("/quotes", "POST", {
        card_id: card.id,
        unit_minor: Math.round(Number(price) * 100),
        currency: card.currency ?? "EUR",
      });
    }
    setPrice("");
    setEditing(false);
    await reload();
  };
  const remove = async () => {
    // The card goes; its EVENTS stay. What you paid and what you sold it for is
    // history, and deleting a row must not silently rewrite a realised gain.
    await api("/cards", "DELETE", { id: card.id });
    await reload();
  };

  return (
    <tr className="border-b bg-secondary/30">
      {EDITABLE.map((k) => (
        <td key={k} className="p-1">
          <input className="w-full rounded border bg-background px-2 py-1 text-sm" value={draft[k] ?? ""}
            onChange={(e) => setDraft({ ...draft, [k]: e.target.value })} />
        </td>
      ))}
      <td className="p-1 text-right" colSpan={2}>
        {/* Recording a price is a QUOTE, not a field on the card: what a card sold
            for is a fact about the market with a date on it, and overwriting one
            number would throw away the history the chart is drawn from. */}
        <input className="w-24 rounded border bg-background px-2 py-1 text-sm text-right"
          placeholder="price" value={price} onChange={(e) => setPrice(e.target.value)} />
      </td>
      <td className="p-2 text-right tabular-nums">{card.confidence}%</td>
      <td className="p-1 whitespace-nowrap">
        <button onClick={save} className="rounded border px-2 py-1 text-xs hover:bg-secondary">Save</button>{" "}
        <button onClick={() => setEditing(false)} className="text-xs text-muted-foreground">cancel</button>{" "}
        <button onClick={remove} className="text-xs text-destructive" title="remove from the collection">✕</button>
      </td>
    </tr>
  );
}

export function CardsPage({ store }: { store: Store }) {
  const [answer, setAnswer] = useState("");
  const [scanErr, setScanErr] = useState("");
  const [prompt, setPrompt] = useState("");
  const [form, setForm] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [preview, setPreview] = useState<string>("");
  const [photoErr, setPhotoErr] = useState("");
  const [stage, setStage] = useState("");
  const fileRef = useRef<HTMLInputElement>(null);

  /**
   * Upload, then WATCH.
   *
   * The upload only stores the picture and answers with a job; the vision call
   * happens on the event stream, which says what it is doing while it does it. A
   * POST that held the connection for the length of a model call would be a spinner
   * with nothing behind it, and a proxy might cut it before the answer arrived.
   */
  const takePhoto = async (file: File) => {
    setPhotoErr("");
    setStage("preparing the photo");
    setBusy(true);
    try {
      const shot = await downscale(file);
      setPreview(`data:${shot.media_type};base64,${shot.data}`);

      const r = await api<{ job: string; events: string }>("/photo", "POST", shot);
      if (!r.ok) { setPhotoErr((r.data as any)?.error ?? String(r.status)); setBusy(false); return; }

      // EventSource cannot carry an Authorization header, so the stream is read as a
      // fetch and split on the blank line between SSE frames. Fewer moving parts than
      // a token in a query string, which would also put it in a server log.
      // `events` is already a complete path — prefixing `/api` again fetched
      // `/api/api/...`, which 404s and leaves the upload looking like it hung.
      const resp = await fetch(r.data.events, {
        headers: { authorization: `Bearer ${localStorage.getItem("binder-tok")}` },
      });
      if (!resp.ok) { setPhotoErr(`the stream would not open (${resp.status})`); setBusy(false); return; }
      let settled = false;
      const reader = resp.body!.getReader();
      const dec = new TextDecoder();
      let buf = "";
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        buf += dec.decode(value, { stream: true });
        const frames = buf.split("\n\n");
        buf = frames.pop() ?? "";
        for (const f of frames) {
          const line = f.split("\n").find((l) => l.startsWith("data: "));
          if (!line) continue;
          const ev = JSON.parse(line.slice(6));
          if (ev.stage === "done") {
            settled = true;
            setStage(""); setPreview(""); await store.reload();
          } else if (ev.stage === "refused") {
            settled = true;
            // What the model actually said — "that is a booster wrapper" is worth
            // showing the person holding the phone.
            setStage("");
            setPhotoErr(ev.said ? `${ev.error} — it said: ${ev.said}` : ev.error);
          } else if (ev.stage === "failed") {
            settled = true;
            setStage(""); setPreview(""); setPhotoErr(ev.error);
          } else {
            setStage(ev.detail ?? ev.stage);
          }
        }
      }
      // A stream that ended without saying how is a failure, not a success. Left
      // unsaid it looks like the upload hung: the preview stays and nothing moves.
      if (!settled) {
        setPreview("");
        setPhotoErr("the stream ended before the model answered");
      }
    } catch (e) {
      setPhotoErr(String(e));
      setPreview("");
    } finally {
      setBusy(false);
      setStage("");
      if (fileRef.current) fileRef.current.value = "";
    }
  };

  const scan = async () => {
    const r = await api("/scan", "POST", { answer });
    // A refusal is shown as a refusal: the point of `card:identify` returning one is
    // that no blank row appears in the collection instead.
    if (!r.ok) { setScanErr(`not added — ${(r.data as any)?.error}`); return; }
    setScanErr(""); setAnswer(""); await store.reload();
  };

  const add = async () => {
    const body: Record<string, unknown> = { ...form, quantity: Number(form.quantity || 1) };
    // Minor units on the way in, exactly as stored and valued. The input is the only
    // place a decimal exists.
    if (form.paid) body.paid_minor = Math.round(Number(form.paid) * 100);
    delete (body as any).paid;
    await api("/cards", "POST", body);
    setForm({}); await store.reload();
  };

  const field = (k: string, ph: string, extra = "") => (
    <input className={`rounded-md border bg-background px-2 py-1.5 text-sm ${extra}`} placeholder={ph}
      value={form[k] ?? ""} onChange={(e) => setForm({ ...form, [k]: e.target.value })} />
  );
  const select = (k: string, opts: string[], ph: string) => (
    <select className="rounded-md border bg-background px-2 py-1.5 text-sm" value={form[k] ?? ""}
      onChange={(e) => setForm({ ...form, [k]: e.target.value })}>
      {opts.map((o) => <option key={o} value={o}>{o || ph}</option>)}
    </select>
  );

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">Cards</h1>
        <p className="text-muted-foreground text-sm mt-1">
          Click a row to correct it, or to record what it is worth today. A field the AI guessed and
          nobody has confirmed stays flagged.
        </p>
      </div>

      <div className="rounded-xl border bg-card p-4 space-y-3">
        <div className="flex items-baseline gap-3">
          <b className="text-sm">Take a photo</b>
          <span className="text-xs text-muted-foreground">
            the AI reads the card and flags what it could not establish
          </span>
        </div>
        <div className="flex items-center gap-3">
          {/* `capture="environment"` opens the rear camera on a phone and is ignored
              on a desktop, where it is an ordinary file picker. */}
          <input ref={fileRef} type="file" accept="image/*" capture="environment" className="hidden"
            onChange={(e) => { const f = e.target.files?.[0]; if (f) takePhoto(f); }} />
          <button disabled={busy} onClick={() => fileRef.current?.click()}
            className="rounded-md bg-primary text-primary-foreground px-3 py-1.5 text-sm font-medium disabled:opacity-50">
            {busy ? "Working…" : "Choose or take a photo"}
          </button>
          {stage && (
            <span className="text-sm text-muted-foreground flex items-center gap-2">
              <span className="inline-block w-3 h-3 rounded-full border-2 border-current border-t-transparent animate-spin" />
              {stage}
            </span>
          )}
          {preview && <img src={preview} alt="" className="h-16 rounded-md border" />}
          {photoErr && <span className="text-sm text-destructive">{photoErr}</span>}
        </div>
        <p className="text-xs text-muted-foreground">
          Downscaled in the browser, then uploaded and watched over SSE — the stream reports each
          stage while the model looks. Runs through <code>tools/claude-shim.mjs</code> by default,
          so no API key lives in the app.
        </p>
      </div>

      <div className="rounded-xl border bg-card p-4 space-y-2">
        <div className="flex items-baseline gap-3">
          <b className="text-sm">…or paste what a model said</b>
          <span className="text-xs text-muted-foreground">if you ran the vision call yourself</span>
          <button className="ml-auto text-xs text-muted-foreground hover:text-foreground"
            onClick={async () => setPrompt((await api<{ prompt: string }>("/prompt")).data.prompt)}>
            show the prompt to give the model
          </button>
        </div>
        <textarea className="w-full rounded-md border bg-background p-2 font-mono text-xs min-h-24"
          placeholder={prompt || '{"name":"Charizard ex","set_code":"sv3","number":"125/197","condition":"near mint","confidence":88}'}
          value={answer} onChange={(e) => setAnswer(e.target.value)} />
        <div className="flex items-center gap-3">
          <button onClick={scan} className="rounded-md bg-primary text-primary-foreground px-3 py-1.5 text-sm font-medium">
            Scan
          </button>
          {scanErr && <span className="text-sm text-destructive">{scanErr}</span>}
        </div>
      </div>

      <div className="rounded-xl border bg-card p-4 space-y-2">
        <div className="flex items-baseline gap-3">
          <b className="text-sm">Add by hand</b>
          <span className="text-xs text-muted-foreground">typed in, so nothing is flagged</span>
        </div>
        <div className="flex flex-wrap gap-2">
          {field("name", "name", "min-w-40")}
          {field("set_name", "set name", "w-32")}
          {field("set_code", "code", "w-20")}
          {field("number", "058/165", "w-24")}
          {select("printing", PRINTINGS, "printing…")}
          {select("condition", CONDITIONS, "condition…")}
          {field("paid", "paid", "w-24")}
          {field("quantity", "×1", "w-16")}
          <button onClick={add} className="rounded-md bg-primary text-primary-foreground px-3 py-1.5 text-sm font-medium">
            Add
          </button>
        </div>
      </div>

      <table className="w-full text-sm">
        <thead>
          <tr className="text-left text-xs uppercase tracking-wide text-muted-foreground border-b">
            <th className="p-2 font-medium">card</th><th className="p-2 font-medium">set</th>
            <th className="p-2 font-medium">№</th><th className="p-2 font-medium">printing</th>
            <th className="p-2 font-medium">condition</th>
            <th className="p-2 font-medium text-right">held</th>
            <th className="p-2 font-medium text-right">price</th>
            <th className="p-2 font-medium text-right">value</th>
            <th className="p-2 font-medium text-right">conf.</th>
            <th className="p-2 font-medium">in decks</th>
          </tr>
        </thead>
        <tbody>
          {store.cards.length
            ? store.cards.map((c) => <Row key={c.id} card={c} reload={store.reload} />)
            : <tr><td colSpan={10} className="p-4 text-muted-foreground">Nothing here yet — scan one or type one in.</td></tr>}
        </tbody>
      </table>
    </div>
  );
}
