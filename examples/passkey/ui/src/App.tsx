import { useEffect, useState } from "react";
import { Fingerprint, KeyRound, LogOut, Plus, Trash2, ShieldCheck, Cloud, AlertTriangle } from "lucide-react";
import { api, createPasskey, session, signInWithPasskey, supported, type Credential, type Me } from "./api";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";

export default function App() {
  const [me, setMe] = useState<Me | null>(null);
  const [username, setUsername] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [rp, setRp] = useState<{ rp_id: string; origin: string } | null>(null);

  useEffect(() => {
    api.get<{ rp_id: string; origin: string }>("/config").then((r) => setRp(r.data));
    refresh();
  }, []);

  async function refresh() {
    if (!session.token) return setMe(null);
    const r = await api.get<Me>("/me");
    setMe(r.ok ? r.data : null);
    if (!r.ok) session.set(null);
  }

  async function run(fn: () => Promise<void>) {
    setBusy(true);
    setError("");
    try {
      await fn();
    } catch (e: any) {
      // A user cancelling the OS prompt is not an error worth shouting about.
      setError(e?.name === "NotAllowedError" ? "cancelled" : e?.message ?? String(e));
    } finally {
      setBusy(false);
    }
  }

  const register = () =>
    run(async () => {
      const r = await createPasskey(username.trim().toLowerCase());
      session.set(r.token);
      await refresh();
    });

  const signIn = (named: boolean) =>
    run(async () => {
      const r = await signInWithPasskey(named ? username.trim().toLowerCase() : undefined);
      session.set(r.token);
      await refresh();
    });

  const signOut = () =>
    run(async () => {
      await api.post("/logout");
      session.set(null);
      setMe(null);
    });

  return (
    <div className="min-h-[100dvh]">
      <header className="sticky top-0 z-10 flex flex-wrap items-center gap-2 border-b bg-card/80 px-4 py-3 backdrop-blur">
        <Fingerprint className="size-5 text-primary" />
        <span className="font-semibold">passkey</span>
        <span className="hidden text-sm text-muted-foreground sm:inline">· passwordless sign-in</span>
        <div className="flex-1" />
        {rp && <span className="hidden text-xs text-muted-foreground md:inline">rp: <code>{rp.rp_id}</code></span>}
        {me && (
          <Button variant="outline" size="sm" onClick={signOut} disabled={busy}>
            <LogOut className="size-4" /> Sign out
          </Button>
        )}
      </header>

      <main className="mx-auto grid max-w-2xl gap-4 p-4">
        {!supported && (
          <Card>
            <CardContent className="flex items-center gap-3 pt-6 text-sm">
              <AlertTriangle className="size-5 text-amber-600" />
              This browser has no WebAuthn. Passkeys also need a secure context — use{" "}
              <code>http://localhost:3053</code>, not a LAN address.
            </CardContent>
          </Card>
        )}

        {!me ? (
          <Card>
            <CardHeader>
              <CardTitle className="flex items-center gap-2 text-sm"><KeyRound className="size-4" /> Sign in — no password</CardTitle>
            </CardHeader>
            <CardContent className="grid gap-3">
              <Input
                placeholder="username"
                value={username}
                autoComplete="username webauthn"
                onChange={(e) => setUsername(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && username && signIn(true)}
              />
              <div className="flex flex-wrap gap-2">
                <Button size="sm" disabled={busy || !username} onClick={() => signIn(true)}>
                  <Fingerprint className="size-4" /> Sign in
                </Button>
                <Button size="sm" variant="secondary" disabled={busy || !username} onClick={register}>
                  <Plus className="size-4" /> Create a passkey
                </Button>
                <Button size="sm" variant="outline" disabled={busy} onClick={() => signIn(false)}>
                  Sign in without a username
                </Button>
              </div>
              {error && <p className="text-xs text-red-600">{error}</p>}
              <p className="text-xs text-muted-foreground">
                Your authenticator generates a key pair and keeps the private half. The server stores
                only the public key, and each sign-in is a signature over a fresh single-use challenge —
                nothing replayable, nothing to leak.
              </p>
            </CardContent>
          </Card>
        ) : (
          <>
            <Card>
              <CardContent className="flex items-center gap-3 pt-6">
                <ShieldCheck className="size-8 text-green-600" />
                <div>
                  <div className="font-medium">{me.username}</div>
                  <p className="text-xs text-muted-foreground">signed in with a passkey</p>
                </div>
              </CardContent>
            </Card>

            <Card>
              <CardHeader><CardTitle className="text-sm">Your passkeys</CardTitle></CardHeader>
              <CardContent className="grid gap-3">
                {me.credentials.map((c) => (
                  <PasskeyRow
                    key={c.id}
                    cred={c}
                    only={me.credentials.length <= 1}
                    onDelete={() =>
                      run(async () => {
                        const r = await api.post<{ error?: string }>("/credentials/delete", { id: c.id });
                        if (!r.ok) throw new Error(r.data.error ?? "could not remove");
                        await refresh();
                      })
                    }
                  />
                ))}
                <div className="flex flex-wrap items-center gap-2">
                  <Button
                    size="sm"
                    variant="secondary"
                    disabled={busy}
                    onClick={() => run(async () => { await createPasskey(me.username); await refresh(); })}
                  >
                    <Plus className="size-4" /> Add another device
                  </Button>
                  <span className="text-xs text-muted-foreground">
                    Adding one needs this session — otherwise anyone could enrol their own authenticator on your account.
                  </span>
                </div>
                {error && <p className="text-xs text-red-600">{error}</p>}
              </CardContent>
            </Card>
          </>
        )}
      </main>

      <footer className="mx-auto max-w-2xl px-4 pb-8 text-xs text-muted-foreground">
        The ceremony verification — CBOR + COSE parsing, the type / challenge / origin / RP-ID bindings,
        the ES256 or RS256 signature, and the counter that catches a cloned authenticator — is the{" "}
        <code>webauthn:verify</code> component. This app only decides who owns which credential.
        The RP ID and origin come from config, never from the request: a client-supplied origin would
        make the origin check verify nothing. See <code>PASSKEY.md</code>.
      </footer>
    </div>
  );
}

function PasskeyRow({ cred, only, onDelete }: { cred: Credential; only: boolean; onDelete: () => void }) {
  const when = (s: number | null) => (s ? new Date(s * 1000).toLocaleString() : "never");
  return (
    <div className="flex flex-wrap items-center gap-2 rounded-md border p-3 text-xs">
      <KeyRound className="size-4 text-muted-foreground" />
      <span className="font-mono">{cred.id.slice(0, 10)}…</span>
      <Badge className={cred.alg === -7 ? "bg-slate-600" : "bg-indigo-600"}>{cred.alg === -7 ? "ES256" : "RS256"}</Badge>
      {cred.backed_up && (
        <Badge className="bg-sky-600"><Cloud className="mr-1 size-3" /> synced</Badge>
      )}
      {cred.user_verified && <Badge className="bg-green-600">verified</Badge>}
      <span className="text-muted-foreground">used {when(cred.last_used)} · counter {cred.sign_count}</span>
      <div className="flex-1" />
      <Button size="sm" variant="ghost" disabled={only} onClick={onDelete} title={only ? "your only passkey" : "remove"}>
        <Trash2 className="size-4" />
      </Button>
    </div>
  );
}
