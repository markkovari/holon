// Two screens, one page. `?as=attendee` and `?as=organizer` pick which — so a
// recording can put the two side by side and every frame on both is the same
// running component answering.
import { useEffect, useState } from "react";
import { api, setToken } from "./api";

type Ev = { id: string; title: string; starts_at: string; capacity: number; state: string; claimed?: number; remaining?: number };
type Tk = { id: string; event_id: string; code: string; state: string; qr?: string };

const role = new URLSearchParams(location.search).get("as") ?? "attendee";
const isOrganizer = role === "organizer";

/** The fixture hands back bearers, because a browser cannot mint one — auth-guard
 *  signs them and the secret lives inside the composition. */
async function signIn(): Promise<string> {
  const [, seed] = await api.seed();
  const who = isOrganizer ? "organizer" : "attendee";
  return seed?.tokens?.[who]?.token ?? "";
}

function Card({ children, className = "" }: any) {
  return <div className={`rounded-xl border border-border bg-card/60 p-4 ${className}`}>{children}</div>;
}

export default function App() {
  const [ready, setReady] = useState(false);
  const [events, setEvents] = useState<Ev[]>([]);
  const [tickets, setTickets] = useState<Tk[]>([]);
  const [scan, setScan] = useState("");
  const [flash, setFlash] = useState<{ ok: boolean; text: string } | null>(null);

  const refresh = async () => {
    const [, list] = await api.events("open");
    // Each event again, singly, because `claimed`/`remaining` are only on the one.
    const full = await Promise.all(
      (list?.events ?? []).map(async (e: Ev) => (await api.event(e.id))[1] as Ev),
    );
    setEvents(full);
    if (!isOrganizer) {
      const [, mine] = await api.myTickets();
      const withQr = await Promise.all(
        (mine?.tickets ?? []).map(async (t: Tk) => (await api.ticket(t.id))[1] as Tk),
      );
      setTickets(withQr);
    }
  };

  useEffect(() => {
    (async () => {
      setToken(await signIn());
      await refresh();
      setReady(true);
    })();
  }, []);

  const claim = async (id: string) => {
    const [code, body] = await api.claim(id);
    setFlash(code === 201
      ? { ok: true, text: "ticket issued" }
      : { ok: false, text: body?.error ?? `refused (${code})` });
    await refresh();
  };

  const doScan = async (code: string) => {
    const [status, body] = await api.checkin(code);
    setFlash(status === 200
      ? { ok: true, text: `admitted — ${body.holder.slice(0, 12)}…` }
      : { ok: false, text: `${body?.error ?? status} (${body?.state ?? "—"})` });
    setScan("");
    await refresh();
  };

  if (!ready) return <div className="p-8 text-muted-foreground">connecting…</div>;

  return (
    <div className="min-h-screen bg-background text-foreground p-6 font-sans">
      <header className="mb-5 flex items-baseline gap-3">
        <h1 className="text-xl font-semibold">{isOrganizer ? "Door" : "My tickets"}</h1>
        <span className="rounded-full bg-primary/15 px-2.5 py-0.5 text-xs text-primary">{role}</span>
      </header>

      {flash && (
        <div className={`mb-4 rounded-lg px-3 py-2 text-sm ${flash.ok ? "bg-emerald-500/15 text-emerald-400" : "bg-red-500/15 text-red-400"}`}>
          {flash.text}
        </div>
      )}

      {isOrganizer ? (
        <>
          <Card className="mb-4">
            <div className="mb-2 text-sm text-muted-foreground">Scan a ticket</div>
            <div className="flex gap-2">
              <input
                value={scan}
                onChange={(e) => setScan(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && doScan(scan)}
                placeholder="paste or scan the code"
                className="flex-1 rounded-md border border-input bg-background px-3 py-2 font-mono text-sm"
              />
              <button onClick={() => doScan(scan)} className="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground">
                Check in
              </button>
            </div>
          </Card>
          <div className="grid gap-3">
            {events.map((e) => (
              <Card key={e.id}>
                <div className="flex items-center justify-between">
                  <div>
                    <div className="font-medium">{e.title}</div>
                    <div className="text-xs text-muted-foreground">{e.starts_at}</div>
                  </div>
                  <div className="text-right">
                    <div className="text-2xl font-semibold tabular-nums">{e.claimed}/{e.capacity}</div>
                    <div className="text-xs text-muted-foreground">claimed</div>
                  </div>
                </div>
              </Card>
            ))}
          </div>
        </>
      ) : (
        <>
          <div className="mb-5 grid gap-3">
            {tickets.map((t) => (
              <Card key={t.id} className="flex items-center gap-4">
                <div
                  className="h-28 w-28 shrink-0 rounded bg-white p-1"
                  dangerouslySetInnerHTML={{ __html: t.qr ?? "" }}
                />
                <div>
                  <div className="font-mono text-xs text-muted-foreground">{t.code}</div>
                  <div className={`mt-1 text-sm ${t.state === "checked-in" ? "text-emerald-400" : ""}`}>
                    {t.state}
                  </div>
                </div>
              </Card>
            ))}
            {!tickets.length && <div className="text-sm text-muted-foreground">no tickets yet</div>}
          </div>
          <div className="mb-2 text-sm text-muted-foreground">Open events</div>
          <div className="grid gap-3">
            {events.map((e) => (
              <Card key={e.id}>
                <div className="flex items-center justify-between">
                  <div>
                    <div className="font-medium">{e.title}</div>
                    <div className="text-xs text-muted-foreground">{e.remaining} of {e.capacity} left</div>
                  </div>
                  <button
                    onClick={() => claim(e.id)}
                    disabled={!e.remaining}
                    className="rounded-md bg-primary px-3 py-1.5 text-sm font-medium text-primary-foreground disabled:opacity-40"
                  >
                    {e.remaining ? "Claim" : "Full"}
                  </button>
                </div>
              </Card>
            ))}
          </div>
        </>
      )}
    </div>
  );
}
