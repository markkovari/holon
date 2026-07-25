import { useEffect, useRef, useState } from "react";
import { Zap, LogOut, Play, Eye, ChevronRight, Trophy, Users } from "lucide-react";
import { api, setToken, hasToken, OPT, type Me, type Quiz, type HostView, type PlayView } from "./api";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";

// poll a fetcher on an interval while `on` is true.
function usePoll(fn: () => void, ms: number, on: boolean) {
  const ref = useRef(fn);
  ref.current = fn;
  useEffect(() => {
    if (!on) return;
    ref.current();
    const h = setInterval(() => ref.current(), ms);
    return () => clearInterval(h);
  }, [ms, on]);
}

type Player = { pin: string; id: string; nickname: string };

export default function App() {
  const [me, setMe] = useState<Me | null>(null);
  const [player, setPlayer] = useState<Player | null>(() => {
    const raw = localStorage.getItem("buzz-player");
    return raw ? JSON.parse(raw) : null;
  });
  const [ready, setReady] = useState(false);
  useEffect(() => {
    if (!hasToken()) return setReady(true);
    api<Me>("/me").then((r) => { if (r.ok) setMe(r.data); else setToken(null); setReady(true); });
  }, []);
  function enterGame(p: Player) { localStorage.setItem("buzz-player", JSON.stringify(p)); setPlayer(p); }
  function leaveGame() { localStorage.removeItem("buzz-player"); setPlayer(null); }
  if (!ready) return null;
  if (me) return <HostApp onLogout={() => { setToken(null); setMe(null); }} />;
  if (player) return <PlayerApp player={player} onLeave={leaveGame} />;
  return <Landing onJoined={enterGame} onHost={setMe} />;
}

// ---- landing (join or host) -------------------------------------------------

function Landing({ onJoined, onHost }: { onJoined: (p: { pin: string; id: string; nickname: string }) => void; onHost: (m: Me) => void }) {
  const [pin, setPin] = useState("");
  const [nick, setNick] = useState("");
  const [err, setErr] = useState("");
  const [hosting, setHosting] = useState(false);
  async function join() {
    if (pin.length < 4 || !nick.trim()) return;
    const r = await api<{ player: string; nickname: string }>(`/games/${pin}/join`, "POST", { nickname: nick });
    if (r.ok) onJoined({ pin, id: r.data.player, nickname: r.data.nickname });
    else setErr((r.data as any).error || "could not join");
  }
  if (hosting) return <Login onHost={onHost} onBack={() => setHosting(false)} />;
  return (
    <div className="min-h-[100dvh] grid place-items-center bg-gradient-to-b from-primary/10 to-transparent p-4">
      <Card className="w-full max-w-sm">
        <CardHeader><CardTitle className="flex items-center gap-2"><Zap className="size-5 text-primary" /> buzz — join a game</CardTitle></CardHeader>
        <CardContent className="grid gap-3">
          <Input className="text-center text-2xl font-bold tracking-widest tabular-nums" placeholder="GAME PIN" inputMode="numeric" maxLength={6} value={pin} onChange={(e) => setPin(e.target.value.replace(/\D/g, ""))} />
          <Input placeholder="Nickname" value={nick} onChange={(e) => setNick(e.target.value)} onKeyDown={(e) => e.key === "Enter" && join()} />
          <Button size="lg" onClick={join} disabled={pin.length < 4 || !nick.trim()}>Enter</Button>
          {err && <p className="text-xs text-destructive">{err}</p>}
          <button className="text-xs text-muted-foreground underline" onClick={() => setHosting(true)}>Host a game →</button>
        </CardContent>
      </Card>
    </div>
  );
}

function Login({ onHost, onBack }: { onHost: (m: Me) => void; onBack: () => void }) {
  const [email, setEmail] = useState("host@acme.io");
  const [password, setPassword] = useState("pw12345678");
  const [msg, setMsg] = useState("Sign in to host — you get a demo quiz to run.");
  async function login() {
    const r = await api<any>("/login", "POST", { email, password });
    if (!r.ok) return setMsg(r.data.error || "login failed");
    setToken(r.data.access_token);
    const me = await api<Me>("/me");
    if (me.ok) onHost(me.data);
  }
  async function register() {
    const r = await api<any>("/register", "POST", { email, password });
    if (!r.ok && r.status !== 409) return setMsg(r.data.error || "register failed");
    login();
  }
  return (
    <div className="min-h-[100dvh] grid place-items-center p-4">
      <Card className="w-full max-w-sm">
        <CardHeader><CardTitle className="text-sm">Host sign in</CardTitle></CardHeader>
        <CardContent className="grid gap-3">
          <Input placeholder="email" value={email} onChange={(e) => setEmail(e.target.value)} />
          <Input type="password" placeholder="password" value={password} onChange={(e) => setPassword(e.target.value)} />
          <div className="flex gap-2">
            <Button className="flex-1" onClick={login}>Log in</Button>
            <Button className="flex-1" variant="outline" onClick={register}>Register</Button>
          </div>
          <p className="text-xs text-muted-foreground">{msg}</p>
          <button className="text-xs text-muted-foreground underline" onClick={onBack}>← join instead</button>
        </CardContent>
      </Card>
    </div>
  );
}

// ---- host -------------------------------------------------------------------

function HostApp({ onLogout }: { onLogout: () => void }) {
  const [quizzes, setQuizzes] = useState<Quiz[]>([]);
  const [pin, setPin] = useState<string | null>(null);
  useEffect(() => { api<{ items: Quiz[] }>("/quizzes").then((r) => setQuizzes(r.data.items || [])); }, []);
  async function host(quiz: string) {
    const r = await api<{ pin: string }>("/games", "POST", { quiz });
    if (r.ok) setPin(r.data.pin);
  }
  async function logout() { await api("/logout", "POST"); onLogout(); }

  if (pin) return <HostGame pin={pin} onEnd={() => setPin(null)} onLogout={logout} />;
  return (
    <div className="min-h-[100dvh]">
      <header className="flex items-center gap-2 border-b px-4 py-3">
        <Zap className="size-5 text-primary" /><span className="font-semibold">buzz</span>
        <span className="text-sm text-muted-foreground">· host</span>
        <div className="flex-1" />
        <Button variant="ghost" size="icon" onClick={logout}><LogOut className="size-4" /></Button>
      </header>
      <main className="mx-auto max-w-lg p-4">
        <Card>
          <CardHeader><CardTitle>Host a game</CardTitle></CardHeader>
          <CardContent className="grid gap-2">
            {quizzes.map((q) => (
              <div key={q.id} className="flex items-center gap-3 rounded-md border px-3 py-2 text-sm">
                <div className="min-w-0 flex-1"><div className="font-medium">{q.title}</div><div className="text-xs text-muted-foreground">{q.question_count} questions</div></div>
                <Button size="sm" onClick={() => host(q.id)}><Play className="size-4" /> Host</Button>
              </div>
            ))}
          </CardContent>
        </Card>
      </main>
    </div>
  );
}

function OptionTile({ i, text, big, state }: { i: number; text: string; big?: boolean; state?: "correct" | "wrong" | "" }) {
  const o = OPT[i % OPT.length];
  const dim = state === "wrong" ? "opacity-40" : "";
  const ring = state === "correct" ? "ring-4 ring-white" : "";
  return (
    <div className={`flex items-center gap-3 rounded-lg ${o.bg} ${dim} ${ring} px-4 text-white ${big ? "py-6 text-lg" : "py-4"} font-semibold`}>
      <span className="text-2xl">{o.shape}</span><span className="min-w-0 flex-1">{text}</span>
      {state === "correct" && <span className="text-xl">✓</span>}
    </div>
  );
}

function HostGame({ pin, onEnd, onLogout }: { pin: string; onEnd: () => void; onLogout: () => void }) {
  const [v, setV] = useState<HostView | null>(null);
  const [busy, setBusy] = useState(false);
  usePoll(() => api<HostView>(`/games/${pin}/host`).then((r) => r.ok && setV(r.data)), 800, true);
  async function act(a: string) { setBusy(true); await api(`/games/${pin}/${a}`, "POST"); setBusy(false); }
  if (!v) return null;

  return (
    <div className="min-h-[100dvh] bg-gradient-to-b from-primary/10 to-transparent">
      <header className="flex items-center gap-2 border-b bg-card/70 px-4 py-3 backdrop-blur">
        <Zap className="size-5 text-primary" /><span className="font-semibold">{v.quiz_title}</span>
        {v.phase !== "lobby" && <Badge variant="secondary">Q{(v.current ?? 0) + 1}/{v.total}</Badge>}
        <div className="flex-1" />
        <Badge className="gap-1"><Users className="size-3" />{v.players.length}</Badge>
        <Button variant="ghost" size="icon" onClick={() => { onEnd(); onLogout(); }}><LogOut className="size-4" /></Button>
      </header>
      <main className="mx-auto max-w-2xl p-4">
        {v.phase === "lobby" && (
          <div className="grid gap-6 py-8 text-center">
            <div>
              <div className="text-sm uppercase tracking-widest text-muted-foreground">Join at this device's URL — Game PIN</div>
              <div className="text-7xl font-black tracking-widest tabular-nums">{v.pin}</div>
            </div>
            <div className="flex flex-wrap justify-center gap-2">
              {v.players.map((p) => <Badge key={p.nickname} className="text-sm">{p.nickname}</Badge>)}
              {v.players.length === 0 && <span className="text-sm text-muted-foreground">waiting for players…</span>}
            </div>
            <div><Button size="lg" disabled={busy || v.players.length === 0} onClick={() => act("start")}><Play className="size-4" /> Start game</Button></div>
          </div>
        )}

        {(v.phase === "question" || v.phase === "reveal") && v.question && (
          <div className="grid gap-4 py-4">
            <h2 className="text-center text-2xl font-bold">{v.question.prompt}</h2>
            <div className="grid gap-2 sm:grid-cols-2">
              {v.question.options.map((o, i) => (
                <OptionTile key={i} i={i} text={o} big state={v.phase === "reveal" ? (i === v.question!.answer ? "correct" : "wrong") : ""} />
              ))}
            </div>
            {v.phase === "question" ? (
              <div className="flex items-center justify-between">
                <span className="text-sm text-muted-foreground"><b>{v.answered ?? 0}</b> of {v.players.length} answered</span>
                <Button disabled={busy} onClick={() => act("reveal")}><Eye className="size-4" /> Reveal</Button>
              </div>
            ) : (
              <>
                <div className="grid gap-1 text-xs text-muted-foreground">
                  {v.question.options.map((o, i) => <div key={i} className="flex justify-between"><span>{OPT[i % OPT.length].shape} {o}</span><span>{v.counts?.[i] ?? 0}</span></div>)}
                </div>
                <Leaderboard rows={v.leaderboard} />
                <div className="text-right"><Button disabled={busy} onClick={() => act("next")}><ChevronRight className="size-4" /> Next</Button></div>
              </>
            )}
          </div>
        )}

        {v.phase === "final" && (
          <div className="grid gap-4 py-8 text-center">
            <Trophy className="mx-auto size-12 text-yellow-500" />
            <h2 className="text-2xl font-bold">Final results</h2>
            <Leaderboard rows={v.leaderboard} podium />
            <div><Button variant="outline" onClick={onEnd}>New game</Button></div>
          </div>
        )}
      </main>
    </div>
  );
}

function Leaderboard({ rows, podium }: { rows: { nickname: string; score: number }[]; podium?: boolean }) {
  return (
    <div className="mx-auto grid w-full max-w-md gap-1">
      {rows.map((r, i) => (
        <div key={r.nickname} className={`flex items-center gap-3 rounded-md border px-3 py-2 ${podium && i === 0 ? "border-yellow-500" : ""}`}>
          <span className="w-6 text-center font-bold text-muted-foreground">{i + 1}</span>
          <span className="min-w-0 flex-1 truncate text-left font-medium">{r.nickname}</span>
          <span className="tabular-nums font-semibold">{r.score}</span>
        </div>
      ))}
      {rows.length === 0 && <p className="text-sm text-muted-foreground">no scores yet</p>}
    </div>
  );
}

// ---- player -----------------------------------------------------------------

function PlayerApp({ player, onLeave }: { player: { pin: string; id: string; nickname: string }; onLeave: () => void }) {
  const [v, setV] = useState<PlayView | null>(null);
  const [picked, setPicked] = useState<number | null>(null);
  usePoll(() => api<PlayView>(`/games/${player.pin}/play?player=${player.id}`).then((r) => { if (r.ok) setV(r.data); }), 700, true);
  useEffect(() => { if (v?.phase === "question" && !v.question?.answered) setPicked(null); }, [v?.question?.index]);
  async function pick(i: number) {
    setPicked(i);
    await api(`/games/${player.pin}/answer`, "POST", { player: player.id, option: i });
  }
  if (!v) return null;
  const answered = v.question?.answered || picked !== null;

  return (
    <div className="min-h-[100dvh]">
      <header className="flex items-center gap-2 border-b px-4 py-3">
        <Zap className="size-5 text-primary" /><span className="font-semibold">{v.nickname}</span>
        <div className="flex-1" />
        {v.my_score > 0 && <Badge variant="secondary">{v.my_score} pts · #{v.my_rank}</Badge>}
        <Button variant="ghost" size="icon" onClick={onLeave}><LogOut className="size-4" /></Button>
      </header>
      <main className="mx-auto grid max-w-md gap-4 p-4">
        {v.phase === "lobby" && <p className="py-16 text-center text-lg">You're in! Waiting for the host to start…</p>}

        {v.phase === "question" && v.question && (
          <div className="grid gap-3">
            <div className="text-center text-sm text-muted-foreground">Question {v.question.index + 1} of {v.question.total}</div>
            <h2 className="text-center text-lg font-semibold">{v.question.prompt}</h2>
            {answered ? (
              <p className="py-10 text-center text-lg font-medium text-muted-foreground">Locked in — waiting for others…</p>
            ) : (
              <div className="grid gap-2 sm:grid-cols-2">
                {v.question.options.map((o, i) => (
                  <button key={i} onClick={() => pick(i)}>
                    <OptionTile i={i} text={o} big />
                  </button>
                ))}
              </div>
            )}
          </div>
        )}

        {v.phase === "reveal" && v.reveal && (
          <div className={`grid place-items-center gap-2 rounded-lg py-14 text-center text-white ${v.reveal.my_correct ? "bg-green-600" : "bg-red-600"}`}>
            <div className="text-3xl font-black">{v.reveal.my_correct ? "Correct!" : "Wrong"}</div>
            {v.reveal.my_correct && <div className="text-xl">+{v.reveal.my_points}</div>}
            <div className="text-sm opacity-90">{v.my_score} pts · rank #{v.my_rank}</div>
          </div>
        )}

        {v.phase === "final" && (
          <div className="grid gap-3 py-8 text-center">
            <Trophy className="mx-auto size-10 text-yellow-500" />
            <div className="text-2xl font-bold">You finished #{v.my_rank}</div>
            <div className="text-muted-foreground">{v.my_score} points</div>
            <Leaderboard rows={v.podium || []} podium />
            <Button variant="outline" onClick={onLeave}>Leave</Button>
          </div>
        )}
      </main>
    </div>
  );
}
