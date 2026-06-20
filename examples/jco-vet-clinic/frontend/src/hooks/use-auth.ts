import { useCallback, useEffect, useState } from "react"
import {
  api,
  clearToken,
  getToken,
  setToken,
  tokenFromPair,
  type Me,
  type Role,
  type TokenPair,
} from "@/lib/api"

export interface AuthState {
  me: Me | null
  loading: boolean
  login: (email: string, password: string) => Promise<void>
  register: (email: string, password: string, role: Role) => Promise<void>
  logout: () => Promise<void>
}

export function useAuth(): AuthState {
  const [me, setMe] = useState<Me | null>(null)
  const [loading, setLoading] = useState(true)

  const refreshMe = useCallback(async () => {
    if (!getToken()) {
      setMe(null)
      setLoading(false)
      return
    }
    try {
      const fresh = await api<Me>("GET", "/auth/me")
      setMe(fresh)
    } catch {
      clearToken()
      setMe(null)
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    void refreshMe()
  }, [refreshMe])

  const login = useCallback(
    async (email: string, password: string) => {
      const tp = await api<TokenPair>("POST", "/auth/login", { email, password })
      setToken(tokenFromPair(tp))
      await refreshMe()
    },
    [refreshMe],
  )

  const register = useCallback(
    async (email: string, password: string, role: Role) => {
      await api("POST", "/auth/register", { email, password, role })
      // auto-login after a successful register
      const tp = await api<TokenPair>("POST", "/auth/login", { email, password })
      setToken(tokenFromPair(tp))
      await refreshMe()
    },
    [refreshMe],
  )

  const logout = useCallback(async () => {
    try {
      await api("POST", "/auth/logout")
    } catch {
      // token already gone or call failed — fine, we clear locally regardless
    }
    clearToken()
    setMe(null)
  }, [])

  return { me, loading, login, register, logout }
}
