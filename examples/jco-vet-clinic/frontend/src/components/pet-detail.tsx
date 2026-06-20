import { useEffect, useState } from "react"
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import {
  apiBlob,
  api,
  ApiError,
  type PetDetail,
  type VisitNote,
} from "@/lib/api"

function describe(err: unknown): string {
  if (err instanceof ApiError || err instanceof Error) return err.message
  return String(err)
}

// Format a unix-seconds timestamp (possibly stringified) as a locale string;
// falls back to the raw value when it isn't a usable number.
function formatTime(at: number | string): string {
  const secs = Number(at)
  if (Number.isNaN(secs)) return String(at)
  return new Date(secs * 1000).toLocaleString()
}

interface PetDetailViewProps {
  petId: string
  onBack: () => void
}

// The big photo for the detail page. Same bearer-guarded blob → object URL
// pattern as <PetPhoto>: fetch as a Blob with the token, wrap in an object URL,
// revoke on unmount / before replacement so we don't leak blobs.
function DetailPhoto({
  petId,
  hasPhoto,
  name,
  onError,
}: {
  petId: string
  hasPhoto: boolean
  name: string
  onError: (message: string) => void
}) {
  const [objectUrl, setObjectUrl] = useState<string | null>(null)

  useEffect(() => {
    if (!hasPhoto) {
      setObjectUrl(null)
      return
    }

    let url: string | null = null
    let cancelled = false

    void apiBlob(`/pets/${encodeURIComponent(petId)}/photo`)
      .then((blob) => {
        if (cancelled) return
        url = URL.createObjectURL(blob)
        setObjectUrl(url)
      })
      .catch((err) => {
        if (!cancelled) onError(describe(err))
      })

    return () => {
      cancelled = true
      if (url) URL.revokeObjectURL(url)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [petId, hasPhoto])

  if (objectUrl) {
    return (
      <img
        src={objectUrl}
        alt={`${name} photo`}
        className="max-h-64 w-full rounded-md object-contain"
      />
    )
  }
  return (
    <div className="flex h-64 w-full items-center justify-center rounded-md bg-muted text-sm text-muted-foreground">
      No photo
    </div>
  )
}

// Owner-facing detail view for a single pet: a larger photo, the pet's data,
// and its appointments with each visit's notes inlined.
export function PetDetailView({ petId, onBack }: PetDetailViewProps) {
  const [detail, setDetail] = useState<PetDetail | null>(null)
  const [error, setError] = useState("")

  useEffect(() => {
    let cancelled = false
    setError("")
    setDetail(null)
    void api<PetDetail>("GET", `/pets/${encodeURIComponent(petId)}`)
      .then((d) => {
        if (!cancelled) setDetail(d)
      })
      .catch((err) => {
        if (!cancelled) setError(describe(err))
      })
    return () => {
      cancelled = true
    }
  }, [petId])

  return (
    <div className="space-y-6">
      <Button variant="ghost" onClick={onBack}>
        ← Back
      </Button>

      {error && <p className="text-sm text-destructive">{error}</p>}

      {detail === null ? (
        !error && (
          <p className="text-sm text-muted-foreground">Loading pet…</p>
        )
      ) : (
        <>
          <Card>
            <CardHeader>
              <CardTitle>{detail.name}</CardTitle>
            </CardHeader>
            <CardContent className="grid gap-6 md:grid-cols-[16rem_1fr]">
              <DetailPhoto
                petId={detail.id}
                hasPhoto={Boolean(detail.photo)}
                name={detail.name}
                onError={setError}
              />
              <dl className="grid grid-cols-[6rem_1fr] gap-x-4 gap-y-2 text-sm">
                <dt className="text-muted-foreground">Name</dt>
                <dd className="font-medium">{detail.name}</dd>
                <dt className="text-muted-foreground">Species</dt>
                <dd>{detail.species}</dd>
                <dt className="text-muted-foreground">Owner</dt>
                <dd className="font-mono text-xs">{detail.owner}</dd>
                <dt className="text-muted-foreground">Notes</dt>
                <dd className="whitespace-pre-wrap">{detail.notes ?? "—"}</dd>
              </dl>
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle>Appointments</CardTitle>
            </CardHeader>
            <CardContent className="space-y-4">
              {detail.appointments.length === 0 ? (
                <p className="text-sm text-muted-foreground">
                  No appointments yet.
                </p>
              ) : (
                detail.appointments.map((a) => (
                  <AppointmentCard key={a.id} appointment={a} />
                ))
              )}
            </CardContent>
          </Card>
        </>
      )}
    </div>
  )
}

function AppointmentCard({
  appointment,
}: {
  appointment: PetDetail["appointments"][number]
}) {
  return (
    <div className="rounded-md border p-4">
      <div className="flex flex-wrap items-center gap-x-4 gap-y-1">
        <span className="font-medium">{appointment.datetime}</span>
        <Badge variant="secondary">{appointment.status}</Badge>
        <span className="text-sm text-muted-foreground">
          Doctor: {appointment.doctor || "—"}
        </span>
      </div>
      <NoteList notes={appointment.notes} />
    </div>
  )
}

function NoteList({ notes }: { notes: VisitNote[] }) {
  if (notes.length === 0) {
    return (
      <p className="mt-3 text-sm text-muted-foreground">No visit notes.</p>
    )
  }
  return (
    <ul className="mt-3 space-y-2">
      {notes.map((n) => (
        <li key={n.id} className="text-sm">
          <p className="whitespace-pre-wrap">{n.text}</p>
          <p className="text-xs text-muted-foreground">
            {n.author} · {formatTime(n.at)}
          </p>
        </li>
      ))}
    </ul>
  )
}
