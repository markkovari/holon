import {
  Fragment,
  useCallback,
  useEffect,
  useState,
  type FormEvent,
} from "react"
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
  type Invoice,
  type InvoiceItem,
  type NotesResponse,
  type TransitionResponse,
  type VisitNote,
} from "@/lib/api"

function describe(err: unknown): string {
  if (err instanceof ApiError || err instanceof Error) return err.message
  return String(err)
}

// Format a VisitNote.at (unix-seconds, possibly stringified) as a locale string.
function formatNoteTime(at: number | string): string {
  const secs = Number(at)
  if (Number.isNaN(secs)) return String(at)
  return new Date(secs * 1000).toLocaleString()
}

// Events legal from each status before the server tells us otherwise. We seed
// the buttons from the appointment's status and refine from the transition
// response's `allowed` list once an action runs.
const ALLOWED_BY_STATUS: Record<string, string[]> = {
  booked: ["confirm", "cancel"],
  confirmed: ["complete", "cancel"],
  completed: [],
  cancelled: [],
}

const TRANSITION_LABEL: Record<string, string> = {
  confirm: "Confirm",
  complete: "Complete",
  cancel: "Cancel",
}

export function DoctorView() {
  const [appts, setAppts] = useState<Appointment[]>([])
  const [error, setError] = useState("")
  const [noteAppt, setNoteAppt] = useState("")
  const [noteText, setNoteText] = useState("")

  // events allowed per appointment, refined from transition responses
  const [allowed, setAllowed] = useState<Record<string, string[]>>({})

  // visit notes loaded lazily per appointment on expand
  const [notes, setNotes] = useState<Record<string, VisitNote[]>>({})
  const [expanded, setExpanded] = useState<Set<string>>(new Set())

  // invoices: pending line-item drafts and the saved totals, keyed by appt id
  const [invoiceDrafts, setInvoiceDrafts] = useState<
    Record<string, { description: string; dollars: string }[]>
  >({})
  const [invoices, setInvoices] = useState<Record<string, Invoice>>({})

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

  function allowedFor(a: Appointment): string[] {
    return allowed[a.id] ?? ALLOWED_BY_STATUS[a.status] ?? []
  }

  async function handleTransition(apptId: string, event: string) {
    setError("")
    try {
      const res = await api<TransitionResponse>(
        "POST",
        `/appointments/${encodeURIComponent(apptId)}/transition`,
        { event },
      )
      setAllowed((m) => ({ ...m, [apptId]: res.allowed }))
      toast.success(`Appointment ${res.status}`)
      await loadAppts()
    } catch (err) {
      toast.error(`Failed: ${describe(err)}`)
      setError(describe(err))
    }
  }

  async function handleNote(e: FormEvent) {
    e.preventDefault()
    try {
      await api("POST", `/appointments/${encodeURIComponent(noteAppt)}/notes`, {
        text: noteText,
      })
      setNoteText("")
      toast.success("Note saved")
      // refresh the notes panel if it's open
      if (expanded.has(noteAppt)) await refreshNotes(noteAppt)
    } catch (err) {
      toast.error(`Failed: ${describe(err)}`)
    }
  }

  const refreshNotes = useCallback(async (apptId: string) => {
    try {
      const { notes: loaded } = await api<NotesResponse>(
        "GET",
        `/appointments/${encodeURIComponent(apptId)}/notes`,
      )
      setNotes((m) => ({ ...m, [apptId]: loaded }))
    } catch {
      setNotes((m) => ({ ...m, [apptId]: [] }))
    }
  }, [])

  function toggleNotes(apptId: string) {
    setExpanded((prev) => {
      const next = new Set(prev)
      if (next.has(apptId)) {
        next.delete(apptId)
      } else {
        next.add(apptId)
        if (!(apptId in notes)) void refreshNotes(apptId)
      }
      return next
    })
  }

  function draftFor(apptId: string): { description: string; dollars: string }[] {
    return invoiceDrafts[apptId] ?? [{ description: "", dollars: "" }]
  }

  function setDraft(
    apptId: string,
    lines: { description: string; dollars: string }[],
  ) {
    setInvoiceDrafts((m) => ({ ...m, [apptId]: lines }))
  }

  function addLine(apptId: string) {
    setDraft(apptId, [...draftFor(apptId), { description: "", dollars: "" }])
  }

  function updateLine(
    apptId: string,
    idx: number,
    patch: Partial<{ description: string; dollars: string }>,
  ) {
    const lines = draftFor(apptId).map((l, i) =>
      i === idx ? { ...l, ...patch } : l,
    )
    setDraft(apptId, lines)
  }

  async function handleSaveInvoice(apptId: string) {
    setError("")
    // dollars → integer cents; skip blank rows
    const items: InvoiceItem[] = draftFor(apptId)
      .filter((l) => l.description.trim() !== "" || l.dollars.trim() !== "")
      .map((l) => ({
        description: l.description.trim(),
        cents: Math.round(Number(l.dollars || "0") * 100),
      }))
    if (items.length === 0) {
      toast.error("Add at least one line item")
      return
    }
    try {
      const invoice = await api<Invoice>(
        "PUT",
        `/appointments/${encodeURIComponent(apptId)}/invoice`,
        { items },
      )
      setInvoices((m) => ({ ...m, [apptId]: invoice }))
      toast.success(`Invoice saved · ${invoice.totalFormatted}`)
    } catch (err) {
      toast.error(`Failed: ${describe(err)}`)
      setError(describe(err))
    }
  }

  return (
    <div className="space-y-6">
      {error && <p className="text-sm text-destructive">{error}</p>}

      <Card>
        <CardHeader>
          <CardTitle>Appointments</CardTitle>
          <CardDescription>
            Move appointments through their lifecycle, write notes, and invoice.
          </CardDescription>
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
                  <TableHead className="text-right">Actions</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {appts.map((a) => {
                  const events = allowedFor(a)
                  const isOpen = expanded.has(a.id)
                  const saved = invoices[a.id]
                  return (
                    <Fragment key={a.id}>
                      <TableRow>
                        <TableCell className="font-mono text-xs">
                          {a.id}
                        </TableCell>
                        <TableCell>{a.pet}</TableCell>
                        <TableCell>{a.owner}</TableCell>
                        <TableCell>{a.datetime}</TableCell>
                        <TableCell>
                          <Badge variant="secondary">{a.status}</Badge>
                        </TableCell>
                        <TableCell className="text-right">
                          <div className="flex flex-wrap items-center justify-end gap-2">
                            {events.map((ev) => (
                              <Button
                                key={ev}
                                variant={
                                  ev === "cancel" ? "destructive" : "default"
                                }
                                size="sm"
                                onClick={() => handleTransition(a.id, ev)}
                              >
                                {TRANSITION_LABEL[ev] ?? ev}
                              </Button>
                            ))}
                            <Button
                              variant="ghost"
                              size="sm"
                              onClick={() => toggleNotes(a.id)}
                              aria-expanded={isOpen}
                            >
                              Notes / invoice {isOpen ? "▾" : "▸"}
                            </Button>
                          </div>
                        </TableCell>
                      </TableRow>
                      {isOpen && (
                        <TableRow className="hover:bg-transparent">
                          <TableCell colSpan={6} className="bg-muted/30">
                            <div className="space-y-4">
                              <NotesPanel notes={notes[a.id]} />
                              <InvoicePanel
                                lines={draftFor(a.id)}
                                saved={saved}
                                onAddLine={() => addLine(a.id)}
                                onUpdate={(i, p) => updateLine(a.id, i, p)}
                                onSave={() => handleSaveInvoice(a.id)}
                              />
                            </div>
                          </TableCell>
                        </TableRow>
                      )}
                    </Fragment>
                  )
                })}
              </TableBody>
            </Table>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Write a visit note</CardTitle>
          <CardDescription>
            Attach a note to an appointment by its ID. Markdown supported.
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
              <Label htmlFor="note-text">Note (Markdown supported)</Label>
              <Input
                id="note-text"
                placeholder="**visit note** in markdown…"
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

function NotesPanel({ notes }: { notes: VisitNote[] | undefined }) {
  if (notes === undefined) {
    return <p className="text-sm text-muted-foreground">Loading notes…</p>
  }
  if (notes.length === 0) {
    return <p className="text-sm text-muted-foreground">No notes yet.</p>
  }
  return (
    <ul className="space-y-2">
      {notes.map((n) => (
        <li key={n.id} className="text-sm">
          {n.textHtml !== undefined ? (
            // textHtml is SAFE: the server-side md:render component renders the
            // markdown with raw HTML escaped and link schemes sanitized.
            <div
              className="prose-sm whitespace-pre-wrap"
              dangerouslySetInnerHTML={{ __html: n.textHtml }}
            />
          ) : (
            <p className="whitespace-pre-wrap">{n.text}</p>
          )}
          <p className="text-xs text-muted-foreground">
            {n.author} · {formatNoteTime(n.at)}
          </p>
        </li>
      ))}
    </ul>
  )
}

function InvoicePanel({
  lines,
  saved,
  onAddLine,
  onUpdate,
  onSave,
}: {
  lines: { description: string; dollars: string }[]
  saved: Invoice | undefined
  onAddLine: () => void
  onUpdate: (
    idx: number,
    patch: Partial<{ description: string; dollars: string }>,
  ) => void
  onSave: () => void
}) {
  return (
    <div className="space-y-2 border-t pt-3">
      <div className="flex items-center justify-between">
        <p className="text-sm font-medium">Invoice</p>
        {saved && (
          <Badge>
            {saved.totalFormatted} {saved.currency}
          </Badge>
        )}
      </div>
      <div className="space-y-2">
        {lines.map((l, i) => (
          <div key={i} className="flex flex-wrap items-center gap-2">
            <Input
              className="w-56"
              placeholder="description"
              value={l.description}
              onChange={(e) => onUpdate(i, { description: e.target.value })}
            />
            <Input
              className="w-28"
              type="number"
              min="0"
              step="0.01"
              placeholder="0.00"
              value={l.dollars}
              onChange={(e) => onUpdate(i, { dollars: e.target.value })}
            />
          </div>
        ))}
      </div>
      <div className="flex items-center gap-2">
        <Button type="button" variant="outline" size="sm" onClick={onAddLine}>
          Add line
        </Button>
        <Button type="button" size="sm" onClick={onSave}>
          Save invoice
        </Button>
      </div>
    </div>
  )
}
