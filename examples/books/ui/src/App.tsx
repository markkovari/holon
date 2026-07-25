import { useEffect, useState } from "react";
import { BookOpen, LogOut, Plus, Download, Trash2, Scale } from "lucide-react";
import {
  api, download, setToken, hasToken, money, today, ACC_TYPES,
  type Me, type Account, type AccType, type Side, type Entry, type Trial, type Pnl, type BalanceSheet,
} from "./api";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "@/components/ui/tabs";
import { Select, SelectTrigger, SelectValue, SelectContent, SelectItem } from "@/components/ui/select";

const TYPE_COLOR: Record<AccType, string> = {
  asset: "bg-blue-600", liability: "bg-orange-600", equity: "bg-violet-600", income: "bg-green-600", expense: "bg-rose-600",
};

export default function App() {
  const [me, setMe] = useState<Me | null>(null);
  const [ready, setReady] = useState(false);
  useEffect(() => {
    if (!hasToken()) return setReady(true);
    api<Me>("/me").then((r) => { if (r.ok) setMe(r.data); else setToken(null); setReady(true); });
  }, []);
  if (!ready) return null;
  return me ? <Dashboard onLogout={() => { setToken(null); setMe(null); }} /> : <Login onAuthed={setMe} />;
}

function Login({ onAuthed }: { onAuthed: (m: Me) => void }) {
  const [email, setEmail] = useState("you@acme.io");
  const [password, setPassword] = useState("pw12345678");
  const [msg, setMsg] = useState("Register to get a demo chart of accounts + entries. Every journal entry must balance (debits = credits).");
  async function login() {
    const r = await api<any>("/login", "POST", { email, password });
    if (!r.ok) return setMsg(r.data.error || "login failed");
    setToken(r.data.access_token);
    const me = await api<Me>("/me");
    if (me.ok) onAuthed(me.data);
  }
  async function register() {
    const r = await api<any>("/register", "POST", { email, password });
    if (!r.ok && r.status !== 409) return setMsg(r.data.error || "register failed");
    login();
  }
  return (
    <div className="min-h-[100dvh] grid place-items-center p-4">
      <Card className="w-full max-w-sm">
        <CardHeader><CardTitle className="flex items-center gap-2 text-sm"><BookOpen className="size-4" /> books — sign in</CardTitle></CardHeader>
        <CardContent className="grid gap-3">
          <Input placeholder="email" value={email} onChange={(e) => setEmail(e.target.value)} />
          <Input type="password" placeholder="password" value={password} onChange={(e) => setPassword(e.target.value)} />
          <div className="flex gap-2">
            <Button className="flex-1" onClick={login}>Log in</Button>
            <Button className="flex-1" variant="outline" onClick={register}>Register</Button>
          </div>
          <p className="text-xs text-muted-foreground">{msg}</p>
        </CardContent>
      </Card>
    </div>
  );
}

function Dashboard({ onLogout }: { onLogout: () => void }) {
  const [accounts, setAccounts] = useState<Account[]>([]);
  const [entries, setEntries] = useState<Entry[]>([]);
  const [tick, setTick] = useState(0);
  const bump = () => setTick((t) => t + 1);
  async function load() {
    setAccounts((await api<{ items: Account[] }>("/accounts")).data.items || []);
    setEntries((await api<{ items: Entry[] }>("/entries")).data.items || []);
  }
  useEffect(() => { load(); }, [tick]);
  async function logout() { await api("/logout", "POST"); onLogout(); }

  return (
    <div className="min-h-[100dvh]">
      <header className="sticky top-0 z-10 flex items-center gap-2 border-b bg-card/80 px-4 py-3 backdrop-blur">
        <BookOpen className="size-5 text-primary" />
        <span className="font-semibold">books</span>
        <span className="hidden text-sm text-muted-foreground sm:inline">· double-entry</span>
        <div className="flex-1" />
        <Button variant="ghost" size="icon" onClick={logout} title="Log out"><LogOut className="size-4" /></Button>
      </header>

      <main className="mx-auto max-w-3xl p-4">
        <Tabs defaultValue="journal">
          <TabsList className="w-full sm:w-auto">
            <TabsTrigger value="journal" className="flex-1 sm:flex-none">Journal</TabsTrigger>
            <TabsTrigger value="accounts" className="flex-1 sm:flex-none">Accounts</TabsTrigger>
            <TabsTrigger value="reports" className="flex-1 sm:flex-none">Reports</TabsTrigger>
          </TabsList>
          <TabsContent value="journal"><JournalTab accounts={accounts} entries={entries} onChange={bump} /></TabsContent>
          <TabsContent value="accounts"><AccountsTab accounts={accounts} onChange={bump} /></TabsContent>
          <TabsContent value="reports"><ReportsTab tick={tick} /></TabsContent>
        </Tabs>
      </main>
    </div>
  );
}

// ---- accounts ---------------------------------------------------------------

function AccountsTab({ accounts, onChange }: { accounts: Account[]; onChange: () => void }) {
  const [code, setCode] = useState("");
  const [name, setName] = useState("");
  const [type, setType] = useState<AccType>("asset");
  const [err, setErr] = useState("");
  async function add() {
    const r = await api("/accounts", "POST", { code, name, type });
    if (r.ok) { setCode(""); setName(""); setErr(""); onChange(); } else setErr((r.data as any).error || "failed");
  }
  return (
    <div className="grid gap-4">
      <Card>
        <CardHeader><CardTitle>New account</CardTitle></CardHeader>
        <CardContent className="flex flex-wrap items-end gap-2">
          <label className="grid gap-1 text-xs text-muted-foreground">Code<Input className="w-24" placeholder="1000" value={code} onChange={(e) => setCode(e.target.value)} /></label>
          <label className="grid gap-1 text-xs text-muted-foreground">Name<Input className="w-44" placeholder="Cash" value={name} onChange={(e) => setName(e.target.value)} /></label>
          <label className="grid gap-1 text-xs text-muted-foreground">Type
            <Select value={type} onValueChange={(v) => setType(v as AccType)}><SelectTrigger className="w-32"><SelectValue /></SelectTrigger>
              <SelectContent>{ACC_TYPES.map((t) => <SelectItem key={t} value={t}>{t}</SelectItem>)}</SelectContent></Select></label>
          <Button onClick={add}><Plus className="size-4" /> Add</Button>
          {err && <span className="text-xs text-destructive">{err}</span>}
        </CardContent>
      </Card>
      <Card>
        <CardHeader><CardTitle>Chart of accounts</CardTitle></CardHeader>
        <CardContent className="grid gap-1.5">
          {accounts.map((a) => (
            <div key={a.code} className="flex items-center gap-3 rounded-md border px-3 py-2 text-sm">
              <span className="w-14 shrink-0 font-mono text-muted-foreground">{a.code}</span>
              <span className="min-w-0 flex-1 truncate">{a.name}</span>
              <Badge className={TYPE_COLOR[a.type]}>{a.type}</Badge>
            </div>
          ))}
        </CardContent>
      </Card>
    </div>
  );
}

// ---- journal (the double-entry editor) --------------------------------------

type EditLine = { account: string; amount: string; side: Side };

function JournalTab({ accounts, entries, onChange }: { accounts: Account[]; entries: Entry[]; onChange: () => void }) {
  const [date, setDate] = useState(today());
  const [memo, setMemo] = useState("");
  const [lines, setLines] = useState<EditLine[]>([{ account: "", amount: "", side: "debit" }, { account: "", amount: "", side: "credit" }]);
  const [err, setErr] = useState("");
  const name = (code: string) => accounts.find((a) => a.code === code)?.name || code;

  const cents = (s: string) => Math.round((Number(s) || 0) * 100);
  const debits = lines.filter((l) => l.side === "debit").reduce((n, l) => n + cents(l.amount), 0);
  const credits = lines.filter((l) => l.side === "credit").reduce((n, l) => n + cents(l.amount), 0);
  const balanced = debits > 0 && debits === credits;
  const complete = lines.every((l) => l.account && cents(l.amount) > 0);

  function setLine(i: number, patch: Partial<EditLine>) { setLines((ls) => ls.map((l, j) => (j === i ? { ...l, ...patch } : l))); }
  async function post() {
    if (!balanced || !complete) return;
    const body = { date, memo, lines: lines.map((l) => ({ account: l.account, amount: cents(l.amount), side: l.side })) };
    const r = await api("/entries", "POST", body);
    if (r.ok) { setMemo(""); setLines([{ account: "", amount: "", side: "debit" }, { account: "", amount: "", side: "credit" }]); setErr(""); onChange(); }
    else setErr((r.data as any).error || "failed");
  }

  return (
    <div className="grid gap-4">
      <Card>
        <CardHeader><CardTitle className="flex items-center gap-2">New journal entry
          <Badge className={balanced ? "bg-green-600" : "bg-amber-600"}><Scale className="mr-1 size-3" />{balanced ? "balanced" : `${money(debits)} / ${money(credits)}`}</Badge>
        </CardTitle></CardHeader>
        <CardContent className="grid gap-3">
          <div className="flex flex-wrap gap-2">
            <Input type="date" className="w-40" value={date} onChange={(e) => setDate(e.target.value)} />
            <Input className="flex-1 min-w-40" placeholder="memo" value={memo} onChange={(e) => setMemo(e.target.value)} />
          </div>
          {lines.map((l, i) => (
            <div key={i} className="flex flex-wrap items-center gap-2">
              <Select value={l.account} onValueChange={(v) => setLine(i, { account: v })}>
                <SelectTrigger className="w-48"><SelectValue placeholder="account" /></SelectTrigger>
                <SelectContent>{accounts.map((a) => <SelectItem key={a.code} value={a.code}>{a.code} · {a.name}</SelectItem>)}</SelectContent></Select>
              <div className="flex overflow-hidden rounded-md border text-xs">
                {(["debit", "credit"] as Side[]).map((s) => (
                  <button key={s} onClick={() => setLine(i, { side: s })}
                    className={`px-3 py-2 ${l.side === s ? "bg-primary text-primary-foreground" : "text-muted-foreground"}`}>{s}</button>
                ))}
              </div>
              <Input className="w-28" type="number" step="0.01" min="0" placeholder="0.00" value={l.amount} onChange={(e) => setLine(i, { amount: e.target.value })} />
              {lines.length > 2 && <Button variant="ghost" size="icon" className="size-8" onClick={() => setLines((ls) => ls.filter((_, j) => j !== i))}><Trash2 className="size-4" /></Button>}
            </div>
          ))}
          <div className="flex items-center gap-2">
            <Button variant="outline" size="sm" onClick={() => setLines((ls) => [...ls, { account: "", amount: "", side: "debit" }])}><Plus className="size-4" /> Line</Button>
            <div className="flex-1" />
            <Button onClick={post} disabled={!balanced || !complete}>Post entry</Button>
          </div>
          {err && <p className="text-xs text-destructive">{err}</p>}
          {!balanced && <p className="text-xs text-muted-foreground">Debits must equal credits before you can post.</p>}
        </CardContent>
      </Card>

      <Card>
        <CardHeader><CardTitle>Journal</CardTitle></CardHeader>
        <CardContent className="grid gap-1.5">
          {entries.length === 0 && <p className="text-sm text-muted-foreground">No entries yet.</p>}
          {entries.map((e) => (
            <div key={e.id} className="rounded-md border px-3 py-2 text-sm">
              <div className="flex items-center gap-2">
                <span className="w-24 shrink-0 tabular-nums text-muted-foreground">{e.date}</span>
                <span className="min-w-0 flex-1 truncate font-medium">{e.memo}</span>
              </div>
              <div className="mt-1 grid gap-0.5 pl-3 text-xs text-muted-foreground sm:pl-24">
                {e.lines.map((l, i) => (
                  <div key={i} className="flex gap-2">
                    <span className="w-40 truncate">{l.side === "credit" ? "    " : ""}{name(l.account)}</span>
                    <span className="w-20 text-right tabular-nums">{l.side === "debit" ? money(l.amount) : ""}</span>
                    <span className="w-20 text-right tabular-nums">{l.side === "credit" ? money(l.amount) : ""}</span>
                  </div>
                ))}
              </div>
            </div>
          ))}
        </CardContent>
      </Card>
    </div>
  );
}

// ---- reports ----------------------------------------------------------------

function ReportsTab({ tick }: { tick: number }) {
  const [trial, setTrial] = useState<Trial | null>(null);
  const [pnl, setPnl] = useState<Pnl | null>(null);
  const [bs, setBs] = useState<BalanceSheet | null>(null);
  useEffect(() => {
    api<Trial>("/reports/trial").then((r) => setTrial(r.data));
    api<Pnl>("/reports/pnl").then((r) => setPnl(r.data));
    api<BalanceSheet>("/reports/balance-sheet").then((r) => setBs(r.data));
  }, [tick]);

  return (
    <div className="grid gap-4">
      <div className="flex justify-end">
        <Button variant="outline" size="sm" onClick={() => download("/reports/statement.pdf", "books-statements.pdf")}><Download className="size-4" /> Statements PDF</Button>
      </div>

      <Card>
        <CardHeader><CardTitle className="flex items-center gap-2">Trial balance
          {trial && <Badge className={trial.balanced ? "bg-green-600" : "bg-red-600"}>{trial.balanced ? "balanced" : "off"}</Badge>}
        </CardTitle></CardHeader>
        <CardContent className="text-sm">
          <div className="grid grid-cols-[1fr_6rem_6rem] gap-1">
            <div className="text-xs font-medium text-muted-foreground">Account</div>
            <div className="text-right text-xs font-medium text-muted-foreground">Debit</div>
            <div className="text-right text-xs font-medium text-muted-foreground">Credit</div>
            {(trial?.accounts || []).map((a) => (
              <div key={a.code} className="contents">
                <div className="truncate"><span className="font-mono text-muted-foreground">{a.code}</span> {a.name}</div>
                <div className="text-right tabular-nums">{a.debits ? money(a.debits) : ""}</div>
                <div className="text-right tabular-nums">{a.credits ? money(a.credits) : ""}</div>
              </div>
            ))}
            <div className="border-t pt-1 font-semibold">Totals</div>
            <div className="border-t pt-1 text-right font-semibold tabular-nums">{trial && money(trial.total_debits)}</div>
            <div className="border-t pt-1 text-right font-semibold tabular-nums">{trial && money(trial.total_credits)}</div>
          </div>
        </CardContent>
      </Card>

      <div className="grid gap-4 sm:grid-cols-2">
        <Card>
          <CardHeader><CardTitle>Profit &amp; Loss</CardTitle></CardHeader>
          <CardContent className="grid gap-1 text-sm">
            {(pnl?.income || []).map((r) => <Row key={r.code} label={r.name} amount={r.amount} />)}
            <Row label="Total income" amount={pnl?.total_income ?? 0} bold />
            {(pnl?.expenses || []).map((r) => <Row key={r.code} label={r.name} amount={r.amount} />)}
            <Row label="Total expenses" amount={pnl?.total_expenses ?? 0} bold />
            <div className="mt-1 border-t pt-1"><Row label="Net income" amount={pnl?.net_income ?? 0} bold /></div>
          </CardContent>
        </Card>

        <Card>
          <CardHeader><CardTitle className="flex items-center gap-2">Balance sheet
            {bs && <Badge className={bs.balanced ? "bg-green-600" : "bg-red-600"}>{bs.balanced ? "balances" : "off"}</Badge>}
          </CardTitle></CardHeader>
          <CardContent className="grid gap-1 text-sm">
            {(bs?.assets || []).map((r) => <Row key={r.code} label={r.name} amount={r.amount} />)}
            <Row label="Total assets" amount={bs?.total_assets ?? 0} bold />
            <div className="mt-1 border-t pt-1" />
            {(bs?.liabilities || []).map((r) => <Row key={r.code} label={r.name} amount={r.amount} />)}
            {(bs?.equity || []).map((r) => <Row key={r.code} label={r.name} amount={r.amount} />)}
            <Row label="Net income (current)" amount={bs?.net_income ?? 0} />
            <Row label="Liabilities + equity" amount={(bs?.total_liabilities ?? 0) + (bs?.total_equity ?? 0) + (bs?.net_income ?? 0)} bold />
          </CardContent>
        </Card>
      </div>
    </div>
  );
}

function Row({ label, amount, bold }: { label: string; amount: number; bold?: boolean }) {
  return (
    <div className={`flex justify-between ${bold ? "font-semibold" : ""}`}>
      <span className="truncate">{label}</span>
      <span className="tabular-nums">{money(amount)}</span>
    </div>
  );
}
