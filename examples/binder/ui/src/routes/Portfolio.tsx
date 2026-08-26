import { useEffect, useState } from "react";
import {
  Area, Brush, CartesianGrid, ComposedChart, Line, ResponsiveContainer, Tooltip, XAxis, YAxis,
} from "recharts";
import { Link } from "react-router-dom";
import { api, money, type Point, type Portfolio } from "../api";
import type { Store } from "../App";

/**
 * The ranges the server can answer.
 *
 * `days=0` means everything, computed from the earliest event — which is why the
 * selector asks the SERVER rather than slicing what it already has. A selector that
 * only slices cannot show anything older than the default window.
 */
const RANGES = [
  { label: "7d", days: 7 },
  { label: "30d", days: 30 },
  { label: "90d", days: 90 },
  { label: "1y", days: 365 },
  { label: "All", days: 0 },
] as const;

/** Which lines are drawn. Each one is a number the valuation actually computed. */
const SERIES = [
  { key: "market_value_minor", label: "market value", kind: "area" },
  { key: "cost_basis_minor", label: "cost basis", kind: "line" },
  { key: "realised_minor", label: "realised", kind: "line" },
  { key: "unrealised_minor", label: "unrealised", kind: "line" },
] as const;
type SeriesKey = (typeof SERIES)[number]["key"];

const STROKE: Record<SeriesKey, string> = {
  market_value_minor: "hsl(var(--primary))",
  cost_basis_minor: "currentColor",
  realised_minor: "#17803d",
  unrealised_minor: "#b45309",
};

/**
 * The tooltip reads a REAL sample.
 *
 * Recharts hands back the datum under the cursor, and every point carries what the
 * valuation computed for that instant — so this shows numbers rather than a reading
 * off a pixel, and it shows every one of them whether or not its line is drawn.
 */
function PointTip({ active, payload, currency }: any) {
  if (!active || !payload?.length) return null;
  const p: Point = payload[0].payload;
  const tone = p.unrealised_minor > 0 ? "text-emerald-600" : p.unrealised_minor < 0 ? "text-destructive" : "";
  return (
    <div className="rounded-lg border bg-card px-3 py-2 text-xs shadow-lg tabular-nums space-y-0.5">
      <div className="font-medium">
        {new Date(p.at * 1000).toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" })}
      </div>
      <div>value {money(p.market_value_minor, currency)}</div>
      <div className="text-muted-foreground">cost {money(p.cost_basis_minor, currency)}</div>
      <div className={tone}>unrealised {money(p.unrealised_minor, currency)}</div>
      <div>realised {money(p.realised_minor, currency)}</div>
      {p.unquoted > 0 && <div className="text-muted-foreground">{p.unquoted} card(s) unpriced that day</div>}
    </div>
  );
}

function Tile({ label, value, tone }: { label: string; value: string; tone?: string }) {
  return (
    <div className="bg-card p-4">
      <div className={`text-2xl font-semibold tracking-tight tabular-nums ${tone ?? ""}`}>{value}</div>
      <div className="text-xs text-muted-foreground">{label}</div>
    </div>
  );
}

export function PortfolioPage({ store }: { store: Store }) {
  const [days, setDays] = useState<number>(90);
  const [shown, setShown] = useState<SeriesKey[]>(["market_value_minor", "cost_basis_minor"]);
  const [data, setData] = useState<Portfolio | null>(store.portfolio);

  // The window is a server query, so switching range recomputes rather than crops.
  useEffect(() => {
    let live = true;
    api<Portfolio>(`/portfolio?days=${days}`).then((r) => { if (live && r.ok) setData(r.data); });
    return () => { live = false; };
  }, [days, store.portfolio]);

  const p = data ?? store.portfolio;
  if (!p) return null;
  const c = p.currency;
  const up = "text-emerald-600", down = "text-destructive";
  const toggle = (k: SeriesKey) =>
    setShown((s) => (s.includes(k) ? s.filter((x) => x !== k) : [...s, k]));

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">Portfolio</h1>
        <p className="text-muted-foreground text-sm mt-1">
          {store.cards.length} card(s) across {store.decks.length} deck(s). Every number is computed by a
          composed capability — <code>portfolio:value</code>, <code>price:history</code> — over WIT.
        </p>
      </div>

      {/* A log that cannot be valued says so, names the card, and offers the way
          back — rather than a page of zeroes with no explanation, or the 422 that
          used to take out the portfolio, the cards and the decks at once. */}
      {p.blocked && (
        <div className="rounded-xl border border-destructive/40 bg-destructive/5 p-4 text-sm space-y-1">
          <b className="text-destructive">This collection cannot be valued yet.</b>
          <p>{p.blocked}</p>
          {p.blocked_card && (
            <Link to={`/cards/${encodeURIComponent(p.blocked_card)}`} className="underline">
              open {p.blocked_card} and fix it →
            </Link>
          )}
        </div>
      )}

      <div className="grid grid-cols-2 md:grid-cols-5 gap-px bg-border border rounded-xl overflow-hidden">
        <Tile label="market value" value={money(p.market_value_minor, c)} />
        <Tile label="cost basis" value={money(p.cost_basis_minor, c)} />
        <Tile label="unrealised" value={money(p.unrealised_minor, c)}
          tone={p.unrealised_minor > 0 ? up : p.unrealised_minor < 0 ? down : undefined} />
        <Tile label="realised" value={money(p.realised_minor, c)}
          tone={p.realised_minor > 0 ? up : p.realised_minor < 0 ? down : undefined} />
        {/* Named rather than folded into the total: unpriced cards are carried at
            COST, and a screen that hides that shows a number pretending to be
            complete. */}
        <Tile label="unpriced" value={`${p.unquoted} card(s)`} />
      </div>

      <div className="rounded-xl border bg-card p-4 space-y-3">
        <div className="flex flex-wrap items-center gap-2">
          <div className="flex rounded-md border overflow-hidden">
            {RANGES.map((r) => (
              <button key={r.label} onClick={() => setDays(r.days)}
                className={"px-2.5 py-1 text-xs transition-colors " +
                  (days === r.days ? "bg-primary text-primary-foreground" : "hover:bg-secondary")}>
                {r.label}
              </button>
            ))}
          </div>
          <div className="flex flex-wrap gap-1.5 ml-auto">
            {SERIES.map((s) => (
              <button key={s.key} onClick={() => toggle(s.key)}
                className={"flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-xs transition-colors " +
                  (shown.includes(s.key) ? "bg-secondary" : "opacity-45 hover:opacity-80")}>
                <span className="inline-block w-2.5 h-2.5 rounded-full"
                  style={{ background: STROKE[s.key] }} />
                {s.label}
              </button>
            ))}
          </div>
        </div>

        {p.series.length > 1 ? (
          <>
            <ResponsiveContainer width="100%" height={300}>
              <ComposedChart data={p.series} margin={{ top: 8, right: 8, bottom: 0, left: 8 }}>
                <defs>
                  <linearGradient id="fill" x1="0" y1="0" x2="0" y2="1">
                    <stop offset="0%" stopColor="hsl(var(--primary))" stopOpacity={0.24} />
                    <stop offset="100%" stopColor="hsl(var(--primary))" stopOpacity={0} />
                  </linearGradient>
                </defs>
                <CartesianGrid strokeOpacity={0.15} vertical={false} />
                <XAxis dataKey="at" tickLine={false} axisLine={false} fontSize={11} minTickGap={40}
                  tickFormatter={(t) =>
                    new Date(t * 1000).toLocaleDateString(undefined, { day: "numeric", month: "short" })} />
                <YAxis tickLine={false} axisLine={false} fontSize={11} width={70}
                  tickFormatter={(v) => money(v, c)} />
                <Tooltip content={<PointTip currency={c} />} />

                {/* stepAfter, not a curve: the value changes when something HAPPENED,
                    and a smooth line between two events draws movement on days
                    nothing moved. */}
                {shown.includes("market_value_minor") && (
                  <Area type="stepAfter" dataKey="market_value_minor" stroke={STROKE.market_value_minor}
                    strokeWidth={2} fill="url(#fill)" isAnimationActive={false} />
                )}
                {SERIES.filter((s) => s.kind === "line" && shown.includes(s.key)).map((s) => (
                  <Line key={s.key} type="stepAfter" dataKey={s.key} stroke={STROKE[s.key]} dot={false}
                    strokeWidth={1.5} strokeOpacity={s.key === "cost_basis_minor" ? 0.45 : 0.9}
                    strokeDasharray={s.key === "cost_basis_minor" ? "4 3" : undefined}
                    isAnimationActive={false} />
                ))}

                <Brush dataKey="at" height={22} travellerWidth={8} stroke="hsl(var(--primary))"
                  tickFormatter={(t: number) =>
                    new Date(t * 1000).toLocaleDateString(undefined, { day: "numeric", month: "short" })} />
              </ComposedChart>
            </ResponsiveContainer>
            <p className="text-xs text-muted-foreground">
              {p.series.length} samples, one every {Math.round((p.step ?? 86400) / 86400)} day(s).
              Hover for the numbers behind any of them, or drag the bar below the chart to zoom.
            </p>
          </>
        ) : (
          <p className="text-sm text-muted-foreground">
            Not enough history to draw yet — add a card and what you paid for it.
          </p>
        )}
      </div>
    </div>
  );
}
