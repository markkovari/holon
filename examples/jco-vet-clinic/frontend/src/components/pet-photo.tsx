import { useEffect, useRef, useState } from "react"
import { Button } from "@/components/ui/button"
import { apiBlob, apiUpload, ApiError, type Pet } from "@/lib/api"

const ACCEPT = "image/png,image/jpeg,image/webp,image/gif"

function describe(err: unknown): string {
  if (err instanceof ApiError || err instanceof Error) return err.message
  return String(err)
}

interface PetPhotoProps {
  pet: Pet
  // called after a successful upload so the parent can refresh the pet list
  onUploaded: () => void
  // surface upload errors through the parent's existing error display
  onError: (message: string) => void
}

// Thumbnail + upload control for a single pet's photo.
//
// The photo endpoint is bearer-guarded, so we can't point an <img src> at it
// directly — the browser wouldn't attach the Authorization header. Instead we
// fetch the image as a Blob (with the token), wrap it in an object URL, and feed
// that to the <img>. The object URL is revoked on unmount and before being
// replaced so we don't leak blobs.
export function PetPhoto({ pet, onUploaded, onError }: PetPhotoProps) {
  const [objectUrl, setObjectUrl] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const inputRef = useRef<HTMLInputElement>(null)

  const hasPhoto = Boolean(pet.photo)

  useEffect(() => {
    if (!hasPhoto) {
      setObjectUrl(null)
      return
    }

    let url: string | null = null
    let cancelled = false

    void apiBlob(`/pets/${encodeURIComponent(pet.id)}/photo`)
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
    // re-fetch whenever the pet's id or its photo content-type changes
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pet.id, pet.photo])

  async function handleFile(file: File) {
    setBusy(true)
    try {
      await apiUpload(`/pets/${encodeURIComponent(pet.id)}/photo`, file)
      onUploaded()
    } catch (err) {
      onError(describe(err))
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="flex items-center gap-2">
      {objectUrl ? (
        <img
          src={objectUrl}
          alt={`${pet.name} photo`}
          className="h-10 w-10 rounded object-cover"
        />
      ) : (
        <div className="flex h-10 w-10 items-center justify-center rounded bg-muted text-[10px] text-muted-foreground">
          none
        </div>
      )}
      <input
        ref={inputRef}
        type="file"
        accept={ACCEPT}
        className="hidden"
        onChange={(e) => {
          const file = e.target.files?.[0]
          // reset so re-picking the same file fires onChange again
          e.target.value = ""
          if (file) void handleFile(file)
        }}
      />
      <Button
        type="button"
        variant="outline"
        size="sm"
        disabled={busy}
        onClick={() => inputRef.current?.click()}
      >
        {busy ? "…" : hasPhoto ? "Change" : "Upload"}
      </Button>
    </div>
  )
}
