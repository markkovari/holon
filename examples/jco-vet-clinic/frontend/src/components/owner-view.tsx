import {
  Fragment,
  useCallback,
  useEffect,
  useState,
  type FormEvent,
} from "react"
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
import { DateTimePicker } from "@/components/datetime-picker"
import { PetPhoto } from "@/components/pet-photo"
import { PetDetailView } from "@/components/pet-detail"
import {
  api,
  ApiError,
  type Appointment,
  type AppointmentsResponse,
  type NotesResponse,
  type Pet,
  type PetsResponse,
  type TransitionResponse,
  type VisitNote,
} from "@/lib/api"

function describe(err: unknown): string {
  if (err instanceof ApiError || err instanceof Error) return err.message
  return String(err)
}

// Format a VisitNote.at (unix-seconds, possibly stringified) as a locale
// timestamp; falls back to the raw value when it isn't a usable number.
function formatNoteTime(at: number | string): string {
  const secs = Number(at)
  if (Number.isNaN(secs)) return String(at)
  return new Date(secs * 1000).toLocaleString()
}

export function OwnerView() {
  const [pets, setPets] = useState<Pet[]>([])
  const [appts, setAppts] = useState<Appointment[]>([])
  const [error, setError] = useState("")

  // visit notes, loaded lazily per appointment on expand
  const [notes, setNotes] = useState<Record<string, VisitNote[]>>({})
  const [expanded, setExpanded] = useState<Set<string>>(new Set())

  // lightweight state-based navigation: when set, the detail page replaces
  // the list view instead of using a router
  const [selectedPetId, setSelectedPetId] = useState<string | null>(null)

  // add-pet form
  const [petName, setPetName] = useState("")
  const [petSpecies, setPetSpecies] = useState("")

  // search
  const [query, setQuery] = useState("")

  // book form
  const [apptPet, setApptPet] = useState("")
  const [apptWhen, setApptWhen] = useState("")

  const loadPets = useCallback(async (q?: string) => {
    const path = q ? `/pets?q=${encodeURIComponent(q)}` : "/pets"
    const { pets } = await api<PetsResponse>("GET", path)
    setPets(pets)
    if (pets.length && !pets.some((p) => p.id === apptPet)) {
      setApptPet(pets[0].id)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  const loadAppts = useCallback(async () => {
    const { appointments } = await api<AppointmentsResponse>(
      "GET",
      "/appointments",
    )
    setAppts(appointments)
  }, [])

  useEffect(() => {
    void loadPets().catch((e) => setError(describe(e)))
    void loadAppts().catch((e) => setError(describe(e)))
  }, [loadPets, loadAppts])

  async function handleAddPet(e: FormEvent) {
    e.preventDefault()
    setError("")
    try {
      await api("POST", "/pets", { name: petName, species: petSpecies })
      setPetName("")
      setPetSpecies("")
      await loadPets(query || undefined)
    } catch (err) {
      setError(describe(err))
    }
  }

  async function handleSearch(e: FormEvent) {
    e.preventDefault()
    setError("")
    try {
      await loadPets(query || undefined)
    } catch (err) {
      setError(describe(err))
    }
  }

  async function handleClear() {
    setQuery("")
    setError("")
    try {
      await loadPets()
    } catch (err) {
      setError(describe(err))
    }
  }

  async function handleBook(e: FormEvent) {
    e.preventDefault()
    setError("")
    try {
      await api("POST", "/appointments", { pet: apptPet, datetime: apptWhen })
      setApptWhen("")
      await loadAppts()
    } catch (err) {
      setError(describe(err))
    }
  }

  async function handleDeletePet(petId: string) {
    setError("")
    try {
      await api("DELETE", `/pets/${encodeURIComponent(petId)}`)
      await loadPets(query || undefined)
      await loadAppts()
    } catch (err) {
      setError(describe(err))
    }
  }

  // Cancel an appointment via the lifecycle state machine. An owner may only
  // `cancel` (and only their own); the server returns 409/403 which surfaces
  // through the shared error banner.
  async function handleCancelAppt(apptId: string) {
    setError("")
    try {
      await api<TransitionResponse>(
        "POST",
        `/appointments/${encodeURIComponent(apptId)}/transition`,
        { event: "cancel" },
      )
      await loadAppts()
      await loadPets(query || undefined)
    } catch (err) {
      setError(describe(err))
    }
  }

  // Toggle a row's notes panel, lazily fetching the notes the first time it's
  // opened. A 403 (someone else's appointment) or any failure just shows an
  // empty list — never an error banner.
  function toggleNotes(apptId: string) {
    setExpanded((prev) => {
      const next = new Set(prev)
      if (next.has(apptId)) {
        next.delete(apptId)
      } else {
        next.add(apptId)
        if (!(apptId in notes)) {
          void api<NotesResponse>(
            "GET",
            `/appointments/${encodeURIComponent(apptId)}/notes`,
          )
            .then(({ notes: loaded }) =>
              setNotes((m) => ({ ...m, [apptId]: loaded })),
            )
            .catch(() => setNotes((m) => ({ ...m, [apptId]: [] })))
        }
      }
      return next
    })
  }

  // a pet can be deleted only if it has no active (non-cancelled) bookings
  function petHasActiveBooking(petId: string): boolean {
    return appts.some((a) => a.pet === petId && a.status !== "cancelled")
  }
  // an owner can attempt to cancel any non-terminal appointment; the state
  // machine rejects illegal transitions (409) which we surface as an error
  function apptCancellable(a: Appointment): boolean {
    return a.status !== "completed" && a.status !== "cancelled"
  }

  if (selectedPetId !== null) {
    return (
      <PetDetailView
        petId={selectedPetId}
        onBack={() => setSelectedPetId(null)}
      />
    )
  }

  return (
    <div className="space-y-6">
      {error && <p className="text-sm text-destructive">{error}</p>}

      <Card>
        <CardHeader>
          <CardTitle>My pets</CardTitle>
          <CardDescription>Register a new pet and search your pets.</CardDescription>
        </CardHeader>
        <CardContent className="space-y-6">
          <form onSubmit={handleAddPet} className="flex flex-wrap items-end gap-3">
            <div className="space-y-2">
              <Label htmlFor="pet-name">Name</Label>
              <Input
                id="pet-name"
                required
                value={petName}
                onChange={(e) => setPetName(e.target.value)}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="pet-species">Species</Label>
              <Input
                id="pet-species"
                required
                value={petSpecies}
                onChange={(e) => setPetSpecies(e.target.value)}
              />
            </div>
            <Button type="submit">Add pet</Button>
          </form>

          <form onSubmit={handleSearch} className="flex flex-wrap items-end gap-3">
            <div className="flex-1 space-y-2">
              <Label htmlFor="pet-q">Search</Label>
              <Input
                id="pet-q"
                placeholder="search pets…"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
              />
            </div>
            <Button type="submit" variant="secondary">
              Search
            </Button>
            <Button type="button" variant="ghost" onClick={handleClear}>
              Clear
            </Button>
          </form>

          {pets.length === 0 ? (
            <p className="text-sm text-muted-foreground">No pets yet.</p>
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Photo</TableHead>
                  <TableHead>Name</TableHead>
                  <TableHead>Species</TableHead>
                  <TableHead>Notes</TableHead>
                  <TableHead className="text-right">Actions</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {pets.map((p) => {
                  const blocked = petHasActiveBooking(p.id)
                  return (
                    <TableRow key={p.id}>
                      <TableCell>
                        <PetPhoto
                          pet={p}
                          onUploaded={() => {
                            void loadPets(query || undefined).catch((e) =>
                              setError(describe(e)),
                            )
                          }}
                          onError={setError}
                        />
                      </TableCell>
                      <TableCell className="font-medium">
                        <button
                          type="button"
                          className="text-left font-medium underline-offset-4 hover:underline"
                          onClick={() => setSelectedPetId(p.id)}
                        >
                          {p.name}
                        </button>
                      </TableCell>
                      <TableCell>{p.species}</TableCell>
                      <TableCell className="text-muted-foreground">
                        {p.notes ?? "—"}
                      </TableCell>
                      <TableCell className="text-right">
                        <div className="flex items-center justify-end gap-2">
                          <Button
                            variant="outline"
                            size="sm"
                            onClick={() => setSelectedPetId(p.id)}
                          >
                            View
                          </Button>
                          <Button
                            variant="destructive"
                            size="sm"
                            disabled={blocked}
                            title={blocked ? "Pet has an active booking — cancel it first" : "Delete pet"}
                            onClick={() => handleDeletePet(p.id)}
                          >
                            Delete
                          </Button>
                        </div>
                      </TableCell>
                    </TableRow>
                  )
                })}
              </TableBody>
            </Table>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Book an appointment</CardTitle>
          <CardDescription>
            Pick one of your pets, then choose a date and time.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <form onSubmit={handleBook} className="flex flex-wrap items-end gap-3">
            <div className="space-y-2">
              <Label htmlFor="appt-pet">Pet</Label>
              <Select value={apptPet} onValueChange={setApptPet}>
                <SelectTrigger id="appt-pet" className="w-48">
                  <SelectValue placeholder="Select a pet" />
                </SelectTrigger>
                <SelectContent>
                  {pets.map((p) => (
                    <SelectItem key={p.id} value={p.id}>
                      {p.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <DateTimePicker value={apptWhen} onChange={setApptWhen} />
            <Button type="submit" disabled={!apptPet || !apptWhen}>
              Book
            </Button>
          </form>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>My appointments</CardTitle>
        </CardHeader>
        <CardContent>
          {appts.length === 0 ? (
            <p className="text-sm text-muted-foreground">No appointments yet.</p>
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>ID</TableHead>
                  <TableHead>Pet</TableHead>
                  <TableHead>When</TableHead>
                  <TableHead>Status</TableHead>
                  <TableHead className="text-right">Actions</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {appts.map((a) => {
                  const cancellable = apptCancellable(a)
                  const isOpen = expanded.has(a.id)
                  const loaded = notes[a.id]
                  const count = loaded?.length
                  return (
                    <Fragment key={a.id}>
                      <TableRow>
                        <TableCell className="font-mono text-xs">{a.id}</TableCell>
                        <TableCell>{a.pet}</TableCell>
                        <TableCell>{a.datetime}</TableCell>
                        <TableCell>
                          <Badge variant="secondary">{a.status}</Badge>
                        </TableCell>
                        <TableCell className="text-right">
                          <div className="flex items-center justify-end gap-2">
                            <Button
                              variant="ghost"
                              size="sm"
                              onClick={() => toggleNotes(a.id)}
                              aria-expanded={isOpen}
                            >
                              {count === undefined
                                ? "Notes"
                                : `Notes (${count})`}{" "}
                              {isOpen ? "▾" : "▸"}
                            </Button>
                            {cancellable && (
                              <Button
                                variant="destructive"
                                size="sm"
                                title="Cancel appointment"
                                onClick={() => handleCancelAppt(a.id)}
                              >
                                Cancel
                              </Button>
                            )}
                          </div>
                        </TableCell>
                      </TableRow>
                      {isOpen && (
                        <TableRow className="hover:bg-transparent">
                          <TableCell colSpan={5} className="bg-muted/30">
                            {loaded === undefined ? (
                              <p className="text-sm text-muted-foreground">
                                Loading notes…
                              </p>
                            ) : loaded.length === 0 ? (
                              <p className="text-sm text-muted-foreground">
                                No notes yet.
                              </p>
                            ) : (
                              <ul className="space-y-2">
                                {loaded.map((n) => (
                                  <li key={n.id} className="text-sm">
                                    {n.textHtml !== undefined ? (
                                      // textHtml is SAFE: the server-side
                                      // md:render component renders the markdown
                                      // with raw HTML escaped and link schemes
                                      // sanitized.
                                      <div
                                        className="prose-sm whitespace-pre-wrap"
                                        dangerouslySetInnerHTML={{
                                          __html: n.textHtml,
                                        }}
                                      />
                                    ) : (
                                      <p className="whitespace-pre-wrap">
                                        {n.text}
                                      </p>
                                    )}
                                    <p className="text-xs text-muted-foreground">
                                      {n.author} · {formatNoteTime(n.at)}
                                    </p>
                                  </li>
                                ))}
                              </ul>
                            )}
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
    </div>
  )
}
