import { useCallback, useEffect, useState, type FormEvent } from "react"
import { toast } from "sonner"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import {
  api,
  ApiError,
  downloadCsv,
  type AuditEvent,
  type AuditResponse,
  type Role,
} from "@/lib/api"

function describe(err: unknown): string {
  if (err instanceof ApiError || err instanceof Error) return err.message
  return String(err)
}

function outcomeVariant(outcome: string): "default" | "secondary" | "destructive" {
  const o = outcome.toLowerCase()
  if (o.includes("deny") || o.includes("fail") || o.includes("error"))
    return "destructive"
  if (o.includes("allow") || o.includes("ok") || o.includes("success"))
    return "default"
  return "secondary"
}

export function AdminView() {
  const [events, setEvents] = useState<AuditEvent[]>([])
  const [error, setError] = useState("")
  const [subject, setSubject] = useState("")
  const [role, setRole] = useState<Role>("pet-owner")

  const loadAudit = useCallback(async () => {
    setError("")
    try {
      const { events } = await api<AuditResponse>("GET", "/admin/audit")
      setEvents(events)
    } catch (err) {
      setError(describe(err))
    }
  }, [])

  useEffect(() => {
    void loadAudit()
  }, [loadAudit])

  async function handleExport(path: string, filename: string) {
    setError("")
    try {
      await downloadCsv(path, filename)
    } catch (err) {
      setError(describe(err))
    }
  }

  async function handleAssign(e: FormEvent) {
    e.preventDefault()
    try {
      await api("POST", "/admin/assign-role", { subject, role })
      toast.success("Role assigned")
      void loadAudit()
    } catch (err) {
      toast.error(`Failed: ${describe(err)}`)
    }
  }

  return (
    <div className="space-y-6">
      <Card>
        <CardHeader>
          <CardTitle>Assign a role</CardTitle>
          <CardDescription>
            Grant a role to a user by their subject ID.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <form onSubmit={handleAssign} className="flex flex-wrap items-end gap-3">
            <div className="flex-1 space-y-2">
              <Label htmlFor="role-subject">Subject ID</Label>
              <Input
                id="role-subject"
                required
                value={subject}
                onChange={(e) => setSubject(e.target.value)}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="role-name">Role</Label>
              <Select value={role} onValueChange={(v) => setRole(v as Role)}>
                <SelectTrigger id="role-name" className="w-40">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="pet-owner">pet-owner</SelectItem>
                  <SelectItem value="doctor">doctor</SelectItem>
                  <SelectItem value="admin">admin</SelectItem>
                </SelectContent>
              </Select>
            </div>
            <Button type="submit">Assign</Button>
          </form>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Export data</CardTitle>
          <CardDescription>
            Download CSV snapshots. The download is fetched with your admin
            token attached.
          </CardDescription>
        </CardHeader>
        <CardContent>
          {error && <p className="mb-3 text-sm text-destructive">{error}</p>}
          <div className="flex flex-wrap gap-3">
            <Button
              variant="outline"
              onClick={() =>
                void handleExport(
                  "/admin/export/appointments.csv",
                  "appointments.csv",
                )
              }
            >
              Export appointments (CSV)
            </Button>
            <Button
              variant="outline"
              onClick={() =>
                void handleExport("/admin/export/audit.csv", "audit.csv")
              }
            >
              Export audit log (CSV)
            </Button>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader className="flex flex-row items-center justify-between">
          <div className="space-y-1.5">
            <CardTitle>Audit log</CardTitle>
            <CardDescription>Recent security events.</CardDescription>
          </div>
          <Button variant="outline" size="sm" onClick={() => void loadAudit()}>
            Refresh
          </Button>
        </CardHeader>
        <CardContent>
          {error && <p className="mb-3 text-sm text-destructive">{error}</p>}
          {events.length === 0 ? (
            <p className="text-sm text-muted-foreground">No events.</p>
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Time</TableHead>
                  <TableHead>Event</TableHead>
                  <TableHead>Outcome</TableHead>
                  <TableHead>Subject</TableHead>
                  <TableHead>Detail</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {events.map((ev) => (
                  <TableRow key={ev.id}>
                    <TableCell className="whitespace-nowrap text-xs text-muted-foreground">
                      {ev.timestamp}
                    </TableCell>
                    <TableCell className="font-medium">{ev.event}</TableCell>
                    <TableCell>
                      <Badge variant={outcomeVariant(ev.outcome)}>
                        {ev.outcome}
                      </Badge>
                    </TableCell>
                    <TableCell>{ev.subject}</TableCell>
                    <TableCell className="text-muted-foreground">
                      {ev.detail}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          )}
        </CardContent>
      </Card>
    </div>
  )
}
