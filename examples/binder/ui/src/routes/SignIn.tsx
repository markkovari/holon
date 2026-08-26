import { useState } from "react";
import { api, setToken } from "../api";

export function SignIn({ onSignedIn }: { onSignedIn: () => Promise<void> }) {
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [err, setErr] = useState("");

  const submit = async (register: boolean) => {
    setErr("");
    if (register) {
      const r = await api("/register", "POST", { email, password });
      if (!r.ok) { setErr(`could not register — ${(r.data as any)?.error ?? r.status}`); return; }
    }
    const r = await api<{ access_token: string }>("/login", "POST", { email, password });
    // One message whichever half failed, so this cannot be used to find out which
    // addresses have accounts.
    if (!r.ok) { setErr("wrong email or password"); return; }
    setToken(r.data.access_token);
    await onSignedIn();
  };

  return (
    <div className="min-h-screen grid place-items-center bg-background text-foreground px-5">
      <form
        onSubmit={(e) => { e.preventDefault(); submit(false); }}
        className="w-full max-w-sm rounded-xl border bg-card p-6 space-y-3"
      >
        <div>
          <h1 className="text-lg font-semibold tracking-tight">binder</h1>
          <p className="text-sm text-muted-foreground">A Pokémon collection that prices itself.</p>
        </div>
        <input className="w-full rounded-md border bg-background px-3 py-2 text-sm" type="email"
          placeholder="email" value={email} required onChange={(e) => setEmail(e.target.value)} />
        <input className="w-full rounded-md border bg-background px-3 py-2 text-sm" type="password"
          placeholder="password (8+)" value={password} required minLength={8}
          onChange={(e) => setPassword(e.target.value)} />
        <div className="flex gap-2">
          <button className="flex-1 rounded-md bg-primary text-primary-foreground px-3 py-2 text-sm font-medium">
            Sign in
          </button>
          <button type="button" onClick={() => submit(true)}
            className="rounded-md border px-3 py-2 text-sm hover:bg-secondary">
            Register
          </button>
        </div>
        {err && <p className="text-sm text-destructive">{err}</p>}
      </form>
    </div>
  );
}
