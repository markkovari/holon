import { useEffect, useState } from "react";
import { NotebookPen, LogOut, Plus, Download, Trash2, FileArchive } from "lucide-react";
import { api, download, setToken, hasToken, type Me, type Note } from "./api";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";

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
  const [msg, setMsg] = useState("Register to get a few demo notes. Export them all as a real .zip (Markdown + index.csv + manifest).");
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
        <CardHeader><CardTitle className="flex items-center gap-2 text-sm"><NotebookPen className="size-4" /> stash — sign in</CardTitle></CardHeader>
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
  const [notes, setNotes] = useState<Note[]>([]);
  const [sel, setSel] = useState<string | null>(null);
  const [title, setTitle] = useState("");
  const [bodyText, setBodyText] = useState("");

  async function load(selectId?: string) {
    const items = (await api<{ items: Note[] }>("/notes")).data.items || [];
    setNotes(items);
    const pick = selectId ?? sel ?? items[0]?.id ?? null;
    setSel(pick);
    const n = items.find((x) => x.id === pick);
    if (n) { setTitle(n.title); setBodyText(n.body); }
  }
  useEffect(() => { load(); }, []);

  function open(n: Note) { setSel(n.id); setTitle(n.title); setBodyText(n.body); }
  async function newNote() {
    const r = await api<Note>("/notes", "POST", { title: "Untitled", body: "" });
    if (r.ok) await load(r.data.id);
  }
  async function save() {
    if (!sel) return;
    await api(`/notes/${sel}`, "PATCH", { title, body: bodyText });
    load(sel);
  }
  async function del(id: string) {
    await api(`/notes/${id}`, "DELETE");
    setSel(null);
    load();
  }
  async function logout() { await api("/logout", "POST"); onLogout(); }

  return (
    <div className="min-h-[100dvh]">
      <header className="sticky top-0 z-10 flex items-center gap-2 border-b bg-card/80 px-4 py-3 backdrop-blur">
        <NotebookPen className="size-5 text-primary" />
        <span className="font-semibold">stash</span>
        <span className="hidden text-sm text-muted-foreground sm:inline">· notes</span>
        <div className="flex-1" />
        <Button variant="default" size="sm" onClick={() => download("/export.zip", "stash-export.zip")}>
          <FileArchive className="size-4" /> Export .zip
        </Button>
        <Button variant="ghost" size="icon" onClick={logout} title="Log out"><LogOut className="size-4" /></Button>
      </header>

      <main className="mx-auto grid max-w-4xl gap-4 p-4 sm:grid-cols-[16rem_1fr]">
        <Card className="h-fit">
          <CardHeader className="flex-row items-center justify-between space-y-0 pb-2">
            <CardTitle className="text-sm">Notes ({notes.length})</CardTitle>
            <button className="text-muted-foreground hover:text-foreground" onClick={newNote} title="New note"><Plus className="size-4" /></button>
          </CardHeader>
          <CardContent className="grid gap-1">
            {notes.map((n) => (
              <button key={n.id} onClick={() => open(n)}
                className={`truncate rounded-md px-2 py-1.5 text-left text-sm ${sel === n.id ? "bg-muted font-medium" : "hover:bg-muted/50"}`}>
                {n.title || "Untitled"}
              </button>
            ))}
          </CardContent>
        </Card>

        <Card>
          <CardContent className="grid gap-3 pt-4">
            {sel ? (
              <>
                <div className="flex items-center gap-2">
                  <Input className="flex-1 text-base font-medium" value={title} onChange={(e) => setTitle(e.target.value)} placeholder="Title" />
                  <Button variant="ghost" size="icon" onClick={() => del(sel)}><Trash2 className="size-4 text-destructive" /></Button>
                </div>
                <textarea className="min-h-72 rounded-md border bg-transparent p-3 font-mono text-sm"
                  value={bodyText} onChange={(e) => setBodyText(e.target.value)} placeholder="Write in Markdown…" />
                <div className="flex items-center gap-2">
                  <Button onClick={save}>Save</Button>
                  <span className="text-xs text-muted-foreground">Your notes bundle to Markdown in the export.</span>
                </div>
              </>
            ) : (
              <p className="text-sm text-muted-foreground">Pick a note, or hit + to add one.</p>
            )}
          </CardContent>
        </Card>
      </main>
    </div>
  );
}
