// Two screens, one page. `?as=attendee` and `?as=organizer` pick which — so a
// recording can put the two side by side and every frame on both is the same
// running component answering.
import { useEffect, useState } from "react";
import { api, setToken } from "./api";

type Ev = { id: string; title: string; starts_at: string; capacity: number; state: string; claimed?: number; remaining?: number; description?: string; image_type?: string };
type Tk = { id: string; event_id: string; code: string; state: string; qr?: string };
type Note = { seq: number; kind: string; title: string; body: string; at: number; read: boolean };

/** Which screen, once you are in. `?as=organizer` is a VIEW, not a permission —
 *  the routes are guarded by the bearer's roles and an attendee asking for the door
 *  gets 403s from the server, which is where that decision belongs. */
const view = new URLSearchParams(location.search).get("as") ?? "attendee";
const isOrganizer = view === "organizer";

/** Per-view so the two panes of the split screen are two different people. */
const TOKEN_KEY = `events.token.${view}`;

function Card({ children, className = "" }: any) {
  return <div className={`rounded-xl border border-border bg-card/60 p-4 ${className}`}>{children}</div>;
}

/** The poster, if the event has one.
 *
 * Keyed on `image_type` rather than trying the URL and hiding a broken image: the
 * record says whether there is one, so a 404 is never requested.
 */
function Poster({ ev, className = "" }: { ev: Ev; className?: string }) {
  if (!ev.image_type) return null;
  return (
    <img
      src={`/api/events/${ev.id}/image`}
      alt=""
      // A checker of light behind it: a PNG with transparency on a dark card looks
      // like no poster at all, which reads as an upload that silently failed.
      className={`shrink-0 rounded-lg bg-zinc-200 object-cover ${className}`}
    />
  );
}


/** The front door.
 *
 * There was no sign-in screen at all until now, and the reason is worth keeping:
 * the SPA called `/test/seed` on load, which registered people and handed their
 * bearers back. That is a gate's fixture, it was compiled into the artifact that
 * got deployed, and so anyone who could reach the app could mint an organizer
 * token for it. The route is off unless `allow-test-routes` says otherwise now,
 * and this is what replaces it.
 */
function SignIn({ onToken }: { onToken: (t: string, email: string) => void }) {
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");

  const go = async (mode: "login" | "register") => {
    setBusy(true);
    setErr("");
    const [status, body] =
      mode === "login" ? await api.login(email, password) : await api.register(email, password);
    setBusy(false);
    if (body?.token) return onToken(body.token, email);
    setErr(
      { bad_credentials: "wrong email or password", already_registered: "that email is taken", invalid: "check the address, and use 8 characters or more" }[
        body?.error as string
      ] ?? `${body?.error ?? "could not sign in"} (${status})`,
    );
  };

  return (
    <div className="flex min-h-screen items-center justify-center bg-background p-6 text-foreground">
      <div className="w-full max-w-sm">
        <h1 className="text-xl font-semibold">Free tickets</h1>
        <p className="mb-5 mt-1 text-sm text-muted-foreground">
          Signing in as <span className="text-primary">{view}</span>. A new account is an
          attendee — the door is granted, never claimed.
        </p>
        <form
          onSubmit={(e) => {
            e.preventDefault();
            go("login");
          }}
          className="grid gap-2"
        >
          <input
            type="email"
            autoComplete="username"
            value={email}
            onChange={(e) => setEmail(e.target.value)}
            placeholder="you@example.test"
            className="rounded-md border border-input bg-background px-3 py-2 text-sm"
          />
          <input
            type="password"
            autoComplete="current-password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            placeholder="password"
            className="rounded-md border border-input bg-background px-3 py-2 text-sm"
          />
          {err && <div className="rounded-md bg-red-500/15 px-3 py-2 text-sm text-red-400">{err}</div>}
          <div className="mt-1 flex gap-2">
            <button
              type="submit"
              disabled={busy}
              className="flex-1 rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground disabled:opacity-40"
            >
              Sign in
            </button>
            <button
              type="button"
              disabled={busy}
              onClick={() => go("register")}
              className="rounded-md border border-input px-4 py-2 text-sm disabled:opacity-40"
            >
              Create account
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}


/** Opening an event.
 *
 * There was no way to do this in the app at all — events arrived from the fixture,
 * and the fixture is gone. An organizer with no route to create anything is an
 * organizer role that does nothing.
 *
 * The poster is a SECOND request, deliberately: the event is created first, and the
 * image is uploaded against its id. One multipart request would mean parsing
 * multipart in a wasm component to save a round trip nobody is counting.
 */
function NewEvent({ onDone }: { onDone: () => void }) {
  const [open, setOpen] = useState(false);
  const [title, setTitle] = useState("");
  const [startsAt, setStartsAt] = useState("");
  const [capacity, setCapacity] = useState("40");
  const [description, setDescription] = useState("");
  const [file, setFile] = useState<File | null>(null);
  const [err, setErr] = useState("");
  const [busy, setBusy] = useState(false);

  if (!open) {
    return (
      <button
        onClick={() => setOpen(true)}
        className="mb-4 w-full rounded-xl border border-dashed border-border py-2.5 text-sm text-muted-foreground hover:border-primary hover:text-foreground"
      >
        + Open an event
      </button>
    );
  }

  const submit = async () => {
    setBusy(true);
    setErr("");
    const [status, body] = await api.createEvent({
      title,
      starts_at: startsAt ? `${startsAt}:00Z` : "",
      capacity: Number(capacity),
      ...(description.trim() ? { description: description.trim() } : {}),
    });
    if (status !== 201) {
      setBusy(false);
      return setErr(`${body?.error ?? "could not create it"} (${status})`);
    }
    if (file) {
      const [up, ub] = await api.uploadImage(body.id, file);
      if (up !== 201) {
        setBusy(false);
        // The event exists; only the poster failed. Say which, or somebody creates
        // it three more times looking for the one that works.
        return setErr(`event created, but the poster was refused: ${ub?.error ?? up}`);
      }
    }
    setBusy(false);
    setOpen(false);
    setTitle("");
    setDescription("");
    setFile(null);
    onDone();
  };

  return (
    <Card className="mb-4">
      <div className="mb-2 text-sm text-muted-foreground">Open an event</div>
      <div className="grid gap-2">
        <input
          value={title}
          onChange={(e) => setTitle(e.target.value)}
          placeholder="what is it called"
          className="rounded-md border border-input bg-background px-3 py-2 text-sm"
        />
        <textarea
          value={description}
          onChange={(e) => setDescription(e.target.value)}
          placeholder="description (optional)"
          rows={2}
          className="resize-none rounded-md border border-input bg-background px-3 py-2 text-sm"
        />
        <div className="flex gap-2">
          <input
            type="datetime-local"
            value={startsAt}
            onChange={(e) => setStartsAt(e.target.value)}
            className="flex-1 rounded-md border border-input bg-background px-3 py-2 text-sm"
          />
          <input
            type="number"
            min={1}
            value={capacity}
            onChange={(e) => setCapacity(e.target.value)}
            title="how many places"
            className="w-24 rounded-md border border-input bg-background px-3 py-2 text-sm"
          />
        </div>
        <label className="flex items-center gap-2 text-xs text-muted-foreground">
          <span className="cursor-pointer rounded-md border border-input px-3 py-1.5 hover:text-foreground">
            Poster…
          </span>
          <input
            type="file"
            accept="image/png,image/jpeg,image/webp"
            onChange={(e) => setFile(e.target.files?.[0] ?? null)}
            className="hidden"
          />
          {file ? `${file.name} (${Math.round(file.size / 1024)} KB)` : "optional"}
        </label>
        {err && <div className="rounded-md bg-red-500/15 px-3 py-2 text-sm text-red-400">{err}</div>}
        <div className="flex gap-2">
          <button
            onClick={submit}
            disabled={busy || !title || !startsAt}
            className="flex-1 rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground disabled:opacity-40"
          >
            Open it
          </button>
          <button
            onClick={() => setOpen(false)}
            className="rounded-md border border-input px-4 py-2 text-sm"
          >
            Cancel
          </button>
        </div>
      </div>
    </Card>
  );
}


/** The bell: a badge, a list, and a live tail.
 *
 * The badge is `unread-count` and not the length of the list, because the list is
 * one page and the badge is the whole inbox — deriving it by paging is how that
 * gets slow on the one number people look at most.
 *
 * The tail is `EventSource`, which cannot send an Authorization header. So it opens
 * with a 60-second signed ticket minted by an authenticated POST. Verified through
 * `tailscale serve` before any of this was written: frames arrive one per second,
 * unbuffered.
 */
function Bell({ email }: { email: string }) {
  const [notes, setNotes] = useState<Note[]>([]);
  const [unread, setUnread] = useState(0);
  const [open, setOpen] = useState(false);
  const [flash, setFlash] = useState(false);
  const [wantsEmail, setWantsEmail] = useState(false);

  const refresh = async () => {
    const [, n] = await api.notifications();
    setNotes((n?.notifications ?? []).slice().reverse());
    const [, u] = await api.unread();
    setUnread(u?.unread ?? 0);
    const [, pr] = await api.prefs();
    setWantsEmail((pr?.default_channels ?? []).includes("email"));
  };

  useEffect(() => {
    refresh();
    let es: EventSource | null = null;
    let stopped = false;
    (async () => {
      const [, t] = await api.streamTicket();
      if (!t?.ticket || stopped) return;
      es = new EventSource(`/api/notifications/stream?ticket=${encodeURIComponent(t.ticket)}`);
      es.onmessage = () => {
        // The frame carries the note, but the badge and the list are re-read rather
        // than patched: two sources of truth for "what is unread" is how a badge
        // ends up disagreeing with the list under it.
        refresh();
        setFlash(true);
        setTimeout(() => setFlash(false), 1200);
      };
    })();
    return () => {
      stopped = true;
      es?.close();
    };
  }, []);

  const markAll = async () => {
    await api.markRead();
    refresh();
  };

  return (
    <div className="relative">
      <button
        onClick={() => setOpen((o) => !o)}
        aria-label="notifications"
        className={`relative rounded-md border border-input px-2.5 py-1.5 text-sm transition ${
          flash ? "border-primary bg-primary/20" : ""
        }`}
      >
        🔔
        {unread > 0 && (
          <span className="absolute -right-1.5 -top-1.5 min-w-[18px] rounded-full bg-primary px-1 text-[11px] font-semibold leading-[18px] text-primary-foreground">
            {unread}
          </span>
        )}
      </button>
      {open && (
        <div className="absolute right-0 z-40 mt-2 w-80 rounded-xl border border-border bg-card p-2 shadow-xl">
          <div className="flex items-center justify-between px-2 py-1">
            <span className="text-sm font-medium">Notifications</span>
            {unread > 0 && (
              <button onClick={markAll} className="text-xs text-muted-foreground hover:text-foreground">
                mark all read
              </button>
            )}
          </div>
          {/* The settings screen is the APP's job, not the capability's — it is
              the app that knows its own kinds. This is the smallest honest version
              of one: the two channels, and the address email goes to. */}
          <label className="mx-1 mb-1 flex cursor-pointer items-center gap-2 rounded-lg bg-muted/40 px-2 py-1.5 text-xs">
            <input
              type="checkbox"
              checked={wantsEmail}
              onChange={async (e) => {
                const on = e.target.checked;
                setWantsEmail(on);
                await api.putPrefs({
                  default_channels: on ? ["in-app", "email"] : ["in-app"],
                  email_address: email,
                  overrides: {},
                });
              }}
            />
            <span>email me too</span>
            <span className="ml-auto font-mono text-[10px] text-muted-foreground">{email}</span>
          </label>
          <div className="max-h-72 overflow-y-auto">
            {notes.length === 0 && (
              <div className="px-2 py-3 text-sm text-muted-foreground">nothing yet</div>
            )}
            {notes.map((n) => (
              <div
                key={n.seq}
                className={`rounded-lg px-2 py-2 ${n.read ? "opacity-55" : "bg-primary/5"}`}
              >
                <div className="text-sm font-medium">{n.title}</div>
                <div className="text-xs text-muted-foreground">{n.body}</div>
                <div className="mt-0.5 text-[10px] uppercase tracking-wide text-muted-foreground/70">
                  {n.kind}
                </div>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

const EMAIL_KEY = `events.email.${view}`;

export default function App() {
  const [email, setEmail] = useState<string>(() => {
    try {
      return localStorage.getItem(EMAIL_KEY) ?? "";
    } catch {
      return "";
    }
  });
  const [token, setTok] = useState<string>(() => {
    try {
      return localStorage.getItem(TOKEN_KEY) ?? "";
    } catch {
      return "";
    }
  });
  const [ready, setReady] = useState(false);
  const [events, setEvents] = useState<Ev[]>([]);
  const [tickets, setTickets] = useState<Tk[]>([]);
  const [scan, setScan] = useState("");
  const [zoom, setZoom] = useState<Tk | null>(null);
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

  // DURING RENDER, not in an effect. React runs a child's effects before its
  // parent's, so `Bell` opened its stream and asked for its badge before this had
  // run — every call went out with no bearer, answered 401, and the badge sat at
  // zero while the notification was sitting in the inbox. An effect is the wrong
  // place for something a child needs in order to mount at all.
  if (token) setToken(token);

  useEffect(() => {
    if (!token) return;
    (async () => {
      await refresh();
      setReady(true);
    })();
  }, [token]);

  const authed = (t: string, e?: string) => {
    if (e) {
      try {
        localStorage.setItem(EMAIL_KEY, e);
      } catch {
        /* a private window still works for this session */
      }
      setEmail(e);
    }
    try {
      localStorage.setItem(TOKEN_KEY, t);
    } catch {
      /* a private window still works for this session */
    }
    setTok(t);
  };

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

  if (!token) return <SignIn onToken={authed} />;
  if (!ready) return <div className="p-8 text-muted-foreground">connecting…</div>;

  return (
    <div className="min-h-screen bg-background text-foreground p-6 font-sans">
      <header className="mb-5 flex items-baseline gap-3">
        <h1 className="text-xl font-semibold">{isOrganizer ? "Door" : "My tickets"}</h1>
        <span className="rounded-full bg-primary/15 px-2.5 py-0.5 text-xs text-primary">{view}</span>
        <div className="ml-auto" />
        <Bell email={email} />
        <button
          onClick={() => {
            try {
              localStorage.removeItem(TOKEN_KEY);
            } catch {
              /* nothing to clear */
            }
            setTok("");
            setReady(false);
          }}
          className="text-xs text-muted-foreground hover:text-foreground"
        >
          sign out
        </button>
      </header>

      {zoom && (
        // A phone at a door is held up to a scanner, so the code has to be as big as
        // the screen allows and on WHITE — a QR on a dark card is what a reader
        // fails on. Escape and a click both close it; there is nothing else to do here.
        <div
          role="dialog"
          aria-label="ticket code"
          onClick={() => setZoom(null)}
          className="fixed inset-0 z-50 flex flex-col items-center justify-center gap-4 bg-black/85 p-4 backdrop-blur-sm"
        >
          <div
            className="w-[min(88vw,88vh)] max-w-[560px] rounded-2xl bg-white p-4 [&>svg]:h-full [&>svg]:w-full"
            dangerouslySetInnerHTML={{ __html: zoom.qr ?? "" }}
          />
          <div className="text-center">
            <div className="font-mono text-sm text-white/90">{zoom.code}</div>
            <div className="mt-1 text-xs text-white/50">tap anywhere to close</div>
          </div>
        </div>
      )}

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
          <NewEvent onDone={refresh} />
          <button
            onClick={async () => {
              const [, r] = await api.runReminders();
              setFlash(
                r?.fired
                  ? { ok: true, text: `sent ${r.fired} reminder${r.fired === 1 ? "" : "s"}` }
                  : { ok: false, text: "nothing due yet" },
              );
              await refresh();
            }}
            // In a deployment this is `comp-relay` on a schedule — the app spec's
            // [triggers] block — hitting the same route. A component has no loop of
            // its own, so "the clock ticked" is always something calling in. Here it
            // is a button, which is the same work with a person as the scheduler.
            title="fire every reminder whose time has come"
            className="mb-4 w-full rounded-xl border border-dashed border-border py-2 text-xs text-muted-foreground hover:border-primary hover:text-foreground"
          >
            ⏰ run due reminders
          </button>
          <div className="grid gap-3">
            {events.map((e) => (
              <Card key={e.id}>
                <div className="flex items-center gap-3">
                  <Poster ev={e} className="h-14 w-14" />
                  <div className="min-w-0 flex-1">
                    <div className="font-medium">{e.title}</div>
                    <div className="text-xs text-muted-foreground">{e.starts_at}</div>
                    {e.description && (
                      <p className="mt-0.5 line-clamp-1 text-xs text-muted-foreground">{e.description}</p>
                    )}
                  </div>
                  <div className="shrink-0 text-right">
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
                <button
                  onClick={() => setZoom(t)}
                  title="show it big enough to scan"
                  aria-label="enlarge ticket code"
                  className="h-28 w-28 shrink-0 cursor-zoom-in rounded bg-white p-1 transition hover:ring-2 hover:ring-primary [&>svg]:h-full [&>svg]:w-full"
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
                <div className="flex items-start gap-3">
                  <Poster ev={e} className="h-16 w-16" />
                  <div className="min-w-0 flex-1">
                    <div className="font-medium">{e.title}</div>
                    {e.description && (
                      <p className="mt-0.5 line-clamp-2 text-xs text-muted-foreground">{e.description}</p>
                    )}
                    <div className="mt-1 text-xs text-muted-foreground">
                      {e.remaining} of {e.capacity} left
                    </div>
                  </div>
                  <button
                    onClick={() => claim(e.id)}
                    disabled={!e.remaining}
                    className="shrink-0 rounded-md bg-primary px-3 py-1.5 text-sm font-medium text-primary-foreground disabled:opacity-40"
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
