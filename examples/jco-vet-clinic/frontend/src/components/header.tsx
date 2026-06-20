import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import type { Me } from "@/lib/api"

interface HeaderProps {
  me: Me | null
  onLogout: () => void
}

export function Header({ me, onLogout }: HeaderProps) {
  return (
    <header className="border-b bg-card">
      <div className="mx-auto flex max-w-5xl items-center justify-between gap-4 px-4 py-4">
        <h1 className="flex items-center gap-2 text-lg font-semibold tracking-tight">
          <span aria-hidden>🐾</span> Acme Vet Clinic
        </h1>
        {me && (
          <div className="flex items-center gap-3">
            <span className="hidden text-sm text-muted-foreground sm:inline">
              {me.subject}
            </span>
            <span className="flex flex-wrap gap-1">
              {me.roles.map((r) => (
                <Badge key={r} variant="secondary">
                  {r}
                </Badge>
              ))}
            </span>
            <Button variant="outline" size="sm" onClick={onLogout}>
              Log out
            </Button>
          </div>
        )}
      </div>
    </header>
  )
}
