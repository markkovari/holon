import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react"
import { api, type I18nResponse, type UiMessages } from "@/lib/api"

const LOCALE_KEY = "vet_locale"

export interface I18nState {
  locale: string
  setLocale: (locale: string) => void
  // Translate a message key; returns the key itself when no translation exists.
  t: (key: string) => string
}

const I18nContext = createContext<I18nState | null>(null)

function initialLocale(): string {
  return localStorage.getItem(LOCALE_KEY) ?? "en"
}

// Holds the active locale, fetches its message bundle from GET /i18n/:locale on
// change (no auth), and exposes a t(key) lookup. The choice is persisted to
// localStorage under `vet_locale`. Wrapping the app in <I18nProvider> means a
// switch in the header re-renders every consumer with the new strings.
export function I18nProvider({ children }: { children: ReactNode }) {
  const [locale, setLocaleState] = useState<string>(initialLocale)
  const [messages, setMessages] = useState<UiMessages>({})

  useEffect(() => {
    let cancelled = false
    void (async () => {
      try {
        const res = await api<I18nResponse>("GET", `/i18n/${locale}`)
        if (!cancelled) setMessages(res.messages)
      } catch {
        if (!cancelled) setMessages({})
      }
    })()
    return () => {
      cancelled = true
    }
  }, [locale])

  const setLocale = useCallback((next: string) => {
    localStorage.setItem(LOCALE_KEY, next)
    setLocaleState(next)
  }, [])

  const t = useCallback((key: string) => messages[key] ?? key, [messages])

  const value = useMemo<I18nState>(
    () => ({ locale, setLocale, t }),
    [locale, setLocale, t],
  )

  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>
}

export function useI18n(): I18nState {
  const ctx = useContext(I18nContext)
  if (!ctx) throw new Error("useI18n must be used within an I18nProvider")
  return ctx
}
