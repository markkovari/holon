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
  type Appointment,
  type AppointmentsResponse,
} from "@/lib/api"

function describe(err: unknown): string {
  if (err instanceof ApiError || err instanceof Error) return err.message
  return String(err)
}

export function DoctorView() {
  const [appts, setAppts] = useState<Appointment[]>([])
  const [error, setError] = useState("")
  const [noteAppt, setNoteAppt] = useState("")
  const [noteText, setNoteText] = useState("")

  const loadAppts = useCallback(async () => {
    const { appointments } = await api<AppointmentsResponse>(
      "GET",
      "/appointments",
    )
    setAppts(appointments)
  }, [])

  useEffect(() => {
    void loadAppts().catch((e) => setError(describe(e)))
  }, [loadAppts])

  async function handleNote(e: FormEvent) {
    e.preventDefault()
    try {
      await api("POST", `/appointments/${encodeURIComponent(noteAppt)}/notes`, {
        text: noteText,
      })
      setNoteText("")
      toast.success("Note saved")
    } catch (err) {
      toast.error(`Failed: ${describe(err)}`)
    }
  }

  return (
    <div className="space-y-6">
      {error && <p className="text-sm text-destructive">{error}</p>}

      <Card>
        <CardHeader>
          <CardTitle>Appointments</CardTitle>
          <CardDescription>Appointments assigned to you.</CardDescription>
        </CardHeader>
        <CardContent>
          {appts.length === 0 ? (
            <p className="text-sm text-muted-foreground">None assigned.</p>
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>ID</TableHead>
                  <TableHead>Pet</TableHead>
                  <TableHead>Owner</TableHead>
                  <TableHead>When</TableHead>
                  <TableHead>Status</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {appts.map((a) => (
                  <TableRow key={a.id}>
                    <TableCell className="font-mono text-xs">{a.id}</TableCell>
                    <TableCell>{a.pet}</TableCell>
                    <TableCell>{a.owner}</TableCell>
                    <TableCell>{a.datetime}</TableCell>
                    <TableCell>{a.status}</TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Write a visit note</CardTitle>
          <CardDescription>
            Attach a note to an appointment by its ID.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <form onSubmit={handleNote} className="flex flex-wrap items-end gap-3">
            <div className="space-y-2">
              <Label htmlFor="note-appt">Appointment ID</Label>
              <Input
                id="note-appt"
                required
                value={noteAppt}
                onChange={(e) => setNoteAppt(e.target.value)}
              />
            </div>
            <div className="flex-1 space-y-2">
              <Label htmlFor="note-text">Note</Label>
              <Input
                id="note-text"
                placeholder="visit note…"
                required
                value={noteText}
                onChange={(e) => setNoteText(e.target.value)}
              />
            </div>
            <Button type="submit">Save note</Button>
          </form>
        </CardContent>
      </Card>
    </div>
  )
}
