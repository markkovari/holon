import { useCallback, useEffect, useState } from "react";
import { Link, useNavigate, useParams } from "react-router-dom";
import { Area, AreaChart, CartesianGrid, ResponsiveContainer, Tooltip, XAxis, YAxis } from "recharts";
import { api, money, type Card } from "../api";
import type { Store } from "../App";

type Detail = {
  card: Card;
  held: number;
  cost_basis_minor: number;
  price_minor: number | null;
  currency: string;
  price_age_days: number | null;
  value_minor: number | null;
  series: { at: number; unit_minor: number; carried: boolean }[];
  quotes: { at: number; unit_minor: number; currency: string }[];
  events: { at: number; kind: string; quantity: number; unit_minor: number; currency: string }[];
  changes: { field: string; from: string; to: string; at: number }[];
};

const when = (s: number) =>
  new Date(s * 1000).toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" });

/** The tooltip reads a real sample, and says when a price was CARRIED rather than quoted. */
function PriceTip({ active, payload, currency }: any) {
  if (!active || !payload?.length) return null;
  const p = payload[0].payload;
  return (
    <div className="rounded-lg border bg-card px-3 py-2 text-xs shadow-lg tabular-nums">
      <div className="font-medium mb-0.5">{when(p.at)}</div>
      <div>{money(p.unit_minor, currency)}</div>
      {p.carried && <div className="text-muted-foreground">carried — nobody quoted it that day</div>}
    </div>
  );
}

function Row({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <div className="flex justify-between gap-4 py-1.5 border-b last:border-0 text-sm">
      <span className="text-muted-foreground">{label}</span>
      <span className="tabular-nums text-right">{value}</span>
    </div>
  );
}

export function CardDetailPage({ store }: { store: Store }) {
  const id = decodeURIComponent(useParams().id ?? "");
  const navigate = useNavigate();
  const [d, setDetail] = useState<Detail | null>(null);
  const [missing, setMissing] = useState(false);
  const [price, setPrice] = useState("");
  const [ev, setEv] = useState({ kind: "acquired", quantity: "1", unit: "" });

  const load = useCallback(async () => {
    const r = await api<Detail>(`/cards/${encodeURIComponent(id)}`);
    if (!r.ok) { setMissing(true); return; }
    setDetail(r.data);
  }, [id]);
  useEffect(() => { load(); }, [load]);

  if (missing) return <p className="text-muted-foreground">No card by that id.</p>;
  if (!d) return null;
  const c = d.card;

  const quote = async () => {
    if (!price) return;
    // A price is a QUOTE with a date, never a field on the card: overwriting one
    // number would throw away the history this page is drawn from.
    await api("/quotes", "POST", {
      card_id: c.id, unit_minor: Math.round(Number(price) * 100), currency: d.currency,
    });
    setPrice("");
    await load();
    await store.reload();
  };

  const record = async () => {
    if (!ev.unit) return;
    await api("/events", "POST", {
      card_id: c.id, kind: ev.kind, quantity: Number(ev.quantity || 1),
      unit_minor: Math.round(Number(ev.unit) * 100), currency: d.currency,
    });
    setEv({ ...ev, unit: "" });
    await load();
    await store.reload();
  };

  return (
    <div className="space-y-6">
      <div>
        <Link to="/cards" className="text-sm text-muted-foreground hover:text-foreground">← all cards</Link>
        <div className="flex items-baseline gap-3 mt-1 flex-wrap">
          <h1 className="text-2xl font-semibold tracking-tight">{c.name}</h1>
          <span className="text-sm text-muted-foreground">
            {c.set_name || c.set_code || "unknown set"} {c.number}
          </span>
          {c.in_decks?.map((n) => (
            <Link key={n} to={`/decks/${encodeURIComponent(n)}`}
              className="text-xs border rounded-full px-2 py-0.5 text-muted-foreground hover:text-foreground">
              {n}
            </Link>
          ))}
          <button className="ml-auto text-xs text-destructive hover:underline"
            onClick={async () => {
              // The row goes; its events stay, so a realised gain is not rewritten
              // by removing the card it came from.
              await api("/cards", "DELETE", { id: c.id });
              await store.reload();
              navigate("/cards");
            }}>delete card</button>
        </div>
      </div>

      <div className="grid md:grid-cols-3 gap-4">
        <div className="rounded-xl border bg-card p-4">
          <h2 className="text-sm font-medium mb-2 mt-0">What it is</h2>
          <Row label="printing" value={c.printing || <span className="text-amber-600">not set</span>} />
          <Row label="condition" value={c.condition || <span className="text-amber-600">not set</span>} />
          <Row label="rarity" value={c.rarity || "—"} />
          <Row label="language" value={c.language || "—"} />
          <Row label="graded" value={c.graded || "raw"} />
          <Row label="confidence" value={`${c.confidence}%`} />
          {c.needs_review?.length > 0 && (
            <p className="text-xs text-amber-600 mt-2">
              still a guess: {c.needs_review.join(", ")}
            </p>
          )}
        </div>

        <div className="rounded-xl border bg-card p-4">
          <h2 className="text-sm font-medium mb-2 mt-0">What it is worth</h2>
          <Row label="held" value={d.held} />
          <Row label="cost basis" value={money(d.cost_basis_minor, d.currency)} />
          <Row
            label="price"
            value={d.price_minor === null
              ? <span className="text-muted-foreground">unpriced</span>
              : <>{money(d.price_minor, d.currency)}
                  {(d.price_age_days ?? 0) > 0 && (
                    <span className="text-muted-foreground text-xs"> · {d.price_age_days}d old</span>
                  )}</>}
          />
          <Row label="value" value={d.value_minor === null
            ? <span className="text-muted-foreground">—</span> : money(d.value_minor, d.currency)} />
          <div className="flex gap-2 mt-3">
            <input className="w-24 rounded-md border bg-background px-2 py-1 text-sm" placeholder="price today"
              value={price} onChange={(e) => setPrice(e.target.value)} />
            <button onClick={quote} className="rounded-md border px-2 py-1 text-sm hover:bg-secondary">
              Record price
            </button>
          </div>
        </div>

        <div className="rounded-xl border bg-card p-4">
          <h2 className="text-sm font-medium mb-2 mt-0">Bought or sold</h2>
          <div className="flex flex-wrap gap-2">
            <select className="rounded-md border bg-background px-2 py-1 text-sm" value={ev.kind}
              onChange={(e) => setEv({ ...ev, kind: e.target.value })}>
              <option value="acquired">bought</option>
              <option value="disposed">sold</option>
            </select>
            <input className="w-14 rounded-md border bg-background px-2 py-1 text-sm" type="number" min={1}
              value={ev.quantity} onChange={(e) => setEv({ ...ev, quantity: e.target.value })} />
            <input className="w-24 rounded-md border bg-background px-2 py-1 text-sm" placeholder="each"
              value={ev.unit} onChange={(e) => setEv({ ...ev, unit: e.target.value })} />
            <button onClick={record} className="rounded-md border px-2 py-1 text-sm hover:bg-secondary">
              Record
            </button>
          </div>
          <p className="text-xs text-muted-foreground mt-2">
            A swap is two of these — what left at the agreed value, and what arrived at the same one.
          </p>
        </div>
      </div>

      <div className="rounded-xl border bg-card p-4">
        <h2 className="text-sm font-medium mb-2 mt-0">Price</h2>
        {d.series.length > 1 ? (
          <ResponsiveContainer width="100%" height={200}>
            <AreaChart data={d.series} margin={{ top: 8, right: 8, bottom: 0, left: 8 }}>
              <defs>
                <linearGradient id="p" x1="0" y1="0" x2="0" y2="1">
                  <stop offset="0%" stopColor="hsl(var(--primary))" stopOpacity={0.22} />
                  <stop offset="100%" stopColor="hsl(var(--primary))" stopOpacity={0} />
                </linearGradient>
              </defs>
              <CartesianGrid strokeOpacity={0.15} vertical={false} />
              <XAxis dataKey="at" tickLine={false} axisLine={false} fontSize={11} minTickGap={40}
                tickFormatter={(t) => new Date(t * 1000).toLocaleDateString(undefined, { day: "numeric", month: "short" })} />
              <YAxis tickLine={false} axisLine={false} fontSize={11} width={64}
                tickFormatter={(v) => money(v, d.currency)} />
              <Tooltip content={<PriceTip currency={d.currency} />} />
              {/* stepAfter: a price changes when somebody quoted it, and a curve
                  between two quotes draws movement on days nothing moved. */}
              <Area type="stepAfter" dataKey="unit_minor" stroke="hsl(var(--primary))"
                strokeWidth={2} fill="url(#p)" isAnimationActive={false} />
            </AreaChart>
          </ResponsiveContainer>
        ) : (
          <p className="text-sm text-muted-foreground">
            {d.quotes.length ? "One quote so far — record another and this becomes a line." : "Nobody has priced it yet."}
          </p>
        )}
      </div>

      <div className="grid md:grid-cols-2 gap-4">
        <div>
          <h2 className="text-base font-medium mb-2">Everything that happened</h2>
          <table className="w-full text-sm">
            <tbody>
              {[
                ...d.events.map((e) => ({
                  at: e.at,
                  what: `${e.kind === "disposed" ? "sold" : "bought"} ${e.quantity} at ${money(e.unit_minor, e.currency)}`,
                  tone: e.kind === "disposed" ? "text-emerald-600" : "",
                })),
                ...d.quotes.map((q) => ({
                  at: q.at, what: `priced at ${money(q.unit_minor, q.currency)}`, tone: "text-muted-foreground",
                })),
                ...d.changes.map((ch) => ({
                  at: ch.at,
                  // "from nothing" is the ordinary case for a field the AI left
                  // flagged, and reads better than an empty pair of quotes.
                  what: ch.from
                    ? `${ch.field}: ${ch.from} → ${ch.to}`
                    : `${ch.field} set to ${ch.to}`,
                  tone: "text-amber-600",
                })),
              ]
                .sort((a, b) => b.at - a.at)
                .map((r, i) => (
                  <tr key={i} className="border-b">
                    <td className="p-2 text-muted-foreground whitespace-nowrap">{when(r.at)}</td>
                    <td className={`p-2 ${r.tone}`}>{r.what}</td>
                  </tr>
                ))}
              {!d.events.length && !d.quotes.length && !d.changes.length && (
                <tr><td className="p-2 text-muted-foreground">Nothing recorded yet.</td></tr>
              )}
            </tbody>
          </table>
        </div>
      </div>
    </div>
  );
}
