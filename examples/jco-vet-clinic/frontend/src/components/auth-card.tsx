import { useState, type FormEvent } from "react"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Button } from "@/components/ui/button"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { ApiError, type Role } from "@/lib/api"

interface AuthCardProps {
  onLogin: (email: string, password: string) => Promise<void>
  onRegister: (email: string, password: string, role: Role) => Promise<void>
}

const DEMO_LOGINS = [
  { who: "owner", email: "owner@acme-vet.test", password: "ownerpass1" },
  { who: "doctor", email: "doctor@acme-vet.test", password: "doctorpass1" },
  { who: "admin", email: "admin@acme-vet.test", password: "adminpass1" },
]

export function AuthCard({ onLogin, onRegister }: AuthCardProps) {
  const [error, setError] = useState("")
  const [busy, setBusy] = useState(false)

  // login fields
  const [email, setEmail] = useState("")
  const [password, setPassword] = useState("")

  // register fields
  const [rEmail, setREmail] = useState("")
  const [rPassword, setRPassword] = useState("")
  const [rRole, setRRole] = useState<Role>("pet-owner")

  function describe(err: unknown): string {
    if (err instanceof ApiError) return err.message
    if (err instanceof Error) return err.message
    return String(err)
  }

  async function handleLogin(e: FormEvent) {
    e.preventDefault()
    setError("")
    setBusy(true)
    try {
      await onLogin(email, password)
      setPassword("")
    } catch (err) {
      setError(`Login failed: ${describe(err)}`)
    } finally {
      setBusy(false)
    }
  }

  async function handleRegister(e: FormEvent) {
    e.preventDefault()
    setError("")
    setBusy(true)
    try {
      await onRegister(rEmail, rPassword, rRole)
      setRPassword("")
    } catch (err) {
      setError(`Register failed: ${describe(err)}`)
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="mx-auto w-full max-w-md">
      <Card>
        <CardHeader>
          <CardTitle>Welcome to Acme Vet Clinic</CardTitle>
          <CardDescription>
            Sign in or create an account to manage pets and appointments.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <Tabs
            defaultValue="login"
            onValueChange={() => setError("")}
            className="w-full"
          >
            <TabsList className="grid w-full grid-cols-2">
              <TabsTrigger value="login">Sign in</TabsTrigger>
              <TabsTrigger value="register">Register</TabsTrigger>
            </TabsList>

            <TabsContent value="login">
              <form onSubmit={handleLogin} className="space-y-4 pt-4">
                <div className="space-y-2">
                  <Label htmlFor="email">Email</Label>
                  <Input
                    id="email"
                    type="email"
                    autoComplete="username"
                    required
                    value={email}
                    onChange={(e) => setEmail(e.target.value)}
                  />
                </div>
                <div className="space-y-2">
                  <Label htmlFor="password">Password</Label>
                  <Input
                    id="password"
                    type="password"
                    autoComplete="current-password"
                    required
                    value={password}
                    onChange={(e) => setPassword(e.target.value)}
                  />
                </div>
                <Button type="submit" className="w-full" disabled={busy}>
                  {busy ? "Signing in…" : "Sign in"}
                </Button>
              </form>
            </TabsContent>

            <TabsContent value="register">
              <form onSubmit={handleRegister} className="space-y-4 pt-4">
                <div className="space-y-2">
                  <Label htmlFor="r-email">Email</Label>
                  <Input
                    id="r-email"
                    type="email"
                    autoComplete="username"
                    required
                    value={rEmail}
                    onChange={(e) => setREmail(e.target.value)}
                  />
                </div>
                <div className="space-y-2">
                  <Label htmlFor="r-password">Password</Label>
                  <Input
                    id="r-password"
                    type="password"
                    autoComplete="new-password"
                    minLength={8}
                    required
                    placeholder="min 8 characters"
                    value={rPassword}
                    onChange={(e) => setRPassword(e.target.value)}
                  />
                </div>
                <div className="space-y-2">
                  <Label htmlFor="r-role">Role</Label>
                  <Select
                    value={rRole}
                    onValueChange={(v) => setRRole(v as Role)}
                  >
                    <SelectTrigger id="r-role" className="w-full">
                      <SelectValue placeholder="Select a role" />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="pet-owner">Pet owner</SelectItem>
                      <SelectItem value="doctor">Doctor</SelectItem>
                      <SelectItem value="admin">Admin</SelectItem>
                    </SelectContent>
                  </Select>
                </div>
                <Button type="submit" className="w-full" disabled={busy}>
                  {busy ? "Creating account…" : "Create account"}
                </Button>
              </form>
            </TabsContent>
          </Tabs>

          {error && (
            <p className="mt-4 text-sm text-destructive">{error}</p>
          )}

          <div className="mt-6 rounded-md border bg-muted/40 p-4 text-sm">
            <p className="mb-2 font-medium">Demo logins</p>
            <ul className="space-y-1 text-muted-foreground">
              {DEMO_LOGINS.map((d) => (
                <li key={d.who}>
                  <span className="font-medium text-foreground">{d.who}</span>{" "}
                  — {d.email} / {d.password}
                </li>
              ))}
            </ul>
          </div>
        </CardContent>
      </Card>
    </div>
  )
}
