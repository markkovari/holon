// API client for the vet-clinic backend. The frontend is served from the same
// origin as the API, so all paths are relative. The token lives in localStorage
// under `vet_token` and is sent as a Bearer header on every guarded call.

export const TOKEN_KEY = "vet_token"

export function getToken(): string | null {
  return localStorage.getItem(TOKEN_KEY)
}

export function setToken(token: string): void {
  localStorage.setItem(TOKEN_KEY, token)
}

export function clearToken(): void {
  localStorage.removeItem(TOKEN_KEY)
}

export class ApiError extends Error {
  status: number
  data: unknown
  constructor(message: string, status: number, data: unknown) {
    super(message)
    this.name = "ApiError"
    this.status = status
    this.data = data
  }
}

type HttpMethod = "GET" | "POST" | "PUT" | "DELETE"

export async function api<T = unknown>(
  method: HttpMethod,
  path: string,
  body?: unknown,
): Promise<T> {
  const headers: Record<string, string> = {}
  if (body !== undefined) headers["content-type"] = "application/json"
  const token = getToken()
  if (token) headers["authorization"] = `Bearer ${token}`

  const res = await fetch(path, {
    method,
    headers,
    body: body !== undefined ? JSON.stringify(body) : undefined,
  })

  const text = await res.text()
  const data = text ? (JSON.parse(text) as unknown) : null

  if (!res.ok) {
    const errMsg =
      (data && typeof data === "object" && "error" in data
        ? String((data as { error: unknown }).error)
        : null) ?? res.statusText
    throw new ApiError(errMsg, res.status, data)
  }
  return data as T
}

// Fetch a path as raw bytes (e.g. a pet photo) with the bearer token attached,
// returning the response Blob. Throws ApiError on a non-2xx status, parsing the
// JSON `{ error }` body when present.
export async function apiBlob(path: string): Promise<Blob> {
  const headers: Record<string, string> = {}
  const token = getToken()
  if (token) headers["authorization"] = `Bearer ${token}`

  const res = await fetch(path, { method: "GET", headers })
  if (!res.ok) {
    let errMsg = res.statusText
    try {
      const data = (await res.clone().json()) as unknown
      if (data && typeof data === "object" && "error" in data) {
        errMsg = String((data as { error: unknown }).error)
      }
    } catch {
      // body wasn't JSON; keep statusText
    }
    throw new ApiError(errMsg, res.status, null)
  }
  return res.blob()
}

// Upload raw bytes (a File/Blob) to a path. Sends the body as-is with the
// blob's content-type and the bearer token. Mirrors api()'s error handling.
export async function apiUpload<T = unknown>(
  path: string,
  file: Blob,
): Promise<T> {
  const headers: Record<string, string> = { "content-type": file.type }
  const token = getToken()
  if (token) headers["authorization"] = `Bearer ${token}`

  const res = await fetch(path, { method: "POST", headers, body: file })

  const text = await res.text()
  const data = text ? (JSON.parse(text) as unknown) : null

  if (!res.ok) {
    const errMsg =
      (data && typeof data === "object" && "error" in data
        ? String((data as { error: unknown }).error)
        : null) ?? res.statusText
    throw new ApiError(errMsg, res.status, data)
  }
  return data as T
}

// ---- API contract types ----

export type Role = "pet-owner" | "doctor" | "admin"

export interface TokenPair {
  accessToken?: string
  // snake_case fallback tolerated
  access_token?: string
  refreshToken?: string
  expiresIn?: number | string
  sessionId?: string
}

export interface Me {
  subject: string
  tenant: string
  roles: string[]
  scopes: string[]
}

export interface RegisterResult {
  subject: string
  tenant: string
  role: string
}

export interface Pet {
  id: string
  name: string
  species: string
  owner: string
  notes?: string
  // when present, the stored content-type of the pet's photo (e.g. "image/png")
  photo?: string
}

export interface PetsResponse {
  pets: Pet[]
  viewer: string
}

export interface Appointment {
  id: string
  pet: string
  owner: string
  doctor: string
  datetime: string
  status: string
}

export interface AppointmentsResponse {
  appointments: Appointment[]
  viewer: string
}

export interface VisitNote {
  id: string
  appointment: string
  author: string
  text: string
  // unix-seconds from the backend; tolerate a string form too
  at: number | string
}

export interface NotesResponse {
  notes: VisitNote[]
}

// Full pet detail: the pet plus its appointments, each with its visit notes
// inlined (returned by GET /pets/:id).
export type PetDetail = Pet & {
  appointments: (Appointment & { notes: VisitNote[] })[]
}

export interface AuditEvent {
  id: string
  timestamp: string
  event: string
  outcome: string
  tenant: string
  subject: string
  detail: string
}

export interface AuditResponse {
  events: AuditEvent[]
}

export interface ValidationField {
  field: string
  code: string
  message: string
}

// Pull a usable access token out of a login response (snake fallback).
export function tokenFromPair(tp: TokenPair): string {
  const t = tp.accessToken ?? tp.access_token
  if (!t) throw new Error("login response had no access token")
  return t
}
