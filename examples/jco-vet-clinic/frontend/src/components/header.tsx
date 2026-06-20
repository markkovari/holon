import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { useI18n } from "@/hooks/use-i18n"
import type { Me } from "@/lib/api"

interface HeaderProps {
  me: Me | null
  onLogout: () => void
}

export function Header({ me, onLogout }: HeaderProps) {
  const { locale, setLocale, t } = useI18n()

  return (
    <header className="border-b bg-card">
      <div className="mx-auto flex max-w-5xl items-center justify-between gap-4 px-4 py-4">
        <h1 className="flex items-center gap-2 text-lg font-semibold tracking-tight">
          <span aria-hidden>🐾</span> {t("app.title")}
        </h1>
        <div className="flex items-center gap-3">
          <Select value={locale} onValueChange={setLocale}>
            <SelectTrigger className="w-20" size="sm" aria-label="Language">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="en">EN</SelectItem>
              <SelectItem value="es">ES</SelectItem>
            </SelectContent>
          </Select>
          {me && (
            <>
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
                {t("nav.logout")}
              </Button>
            </>
          )}
        </div>
      </div>
    </header>
  )
}
