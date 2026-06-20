import { useCallback, useEffect, useState, type FormEvent } from "react"
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
  api,
  ApiError,
  type TwoFactorEnroll,
  type TwoFactorStatus,
} from "@/lib/api"

function describe(err: unknown): string {
  if (err instanceof ApiError || err instanceof Error) return err.message
  return String(err)
}

// Two-factor (TOTP) enrollment card, shared by the doctor and admin views.
//
// Flow:
//   1. on mount, GET /auth/2fa/status → show an "enabled ✓" badge or an
//      "Enable 2FA" button.
//   2. Enable → POST /auth/2fa/enroll → reveal the base32 secret (copyable) and
//      the otpauth:// provisioning URI as text/instructions. We deliberately do
//      NOT pull in a QR-code dependency (no new npm deps; external QR services
//      are blocked by CSP/offline) — the secret and URI are enough to enroll an
//      authenticator app manually.
//   3. Verify a 6-digit code → POST /auth/2fa/verify → 200 flips status to
//      enrolled with a success badge; 401 shows "Invalid code" inline.
export function TwoFactor() {
  const [enrolled, setEnrolled] = useState<boolean | null>(null)
  const [statusError, setStatusError] = useState("")

  // enrollment material, present once the user clicks "Enable 2FA"
  const [enroll, setEnroll] = useState<TwoFactorEnroll | null>(null)
  const [enrolling, setEnrolling] = useState(false)
  const [enrollError, setEnrollError] = useState("")

  // verify form
  const [code, setCode] = useState("")
  const [verifying, setVerifying] = useState(false)
  const [verifyError, setVerifyError] = useState("")
  const [verified, setVerified] = useState(false)

  const [copied, setCopied] = useState(false)

  const loadStatus = useCallback(async () => {
    setStatusError("")
    try {
      const s = await api<TwoFactorStatus>("GET", "/auth/2fa/status")
      setEnrolled(s.enrolled)
    } catch (err) {
      setStatusError(describe(err))
    }
  }, [])

  useEffect(() => {
    void loadStatus()
  }, [loadStatus])

  async function handleEnable() {
    setEnrollError("")
    setEnrolling(true)
    try {
      const res = await api<TwoFactorEnroll>("POST", "/auth/2fa/enroll")
      setEnroll(res)
    } catch (err) {
      setEnrollError(describe(err))
    } finally {
      setEnrolling(false)
    }
  }

  async function handleCopy() {
    if (!enroll) return
    try {
      await navigator.clipboard.writeText(enroll.secret)
      setCopied(true)
      window.setTimeout(() => setCopied(false), 1500)
    } catch {
      // clipboard unavailable (e.g. insecure context) — the secret is still
      // visible for manual copy, so we silently ignore.
    }
  }

  async function handleVerify(e: FormEvent) {
    e.preventDefault()
    setVerifyError("")
    setVerifying(true)
    try {
      await api("POST", "/auth/2fa/verify", { code })
      setVerified(true)
      setEnrolled(true)
      setCode("")
    } catch (err) {
      // the backend returns 401 { error: "bad_code" } on a wrong code
      if (err instanceof ApiError && err.status === 401) {
        setVerifyError("Invalid code")
      } else {
        setVerifyError(describe(err))
      }
    } finally {
      setVerifying(false)
    }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Two-factor authentication</CardTitle>
        <CardDescription>
          Protect your account with a time-based one-time passcode (TOTP).
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        {statusError && (
          <p className="text-sm text-destructive">{statusError}</p>
        )}

        {enrolled === null ? (
          <p className="text-sm text-muted-foreground">Checking status…</p>
        ) : enrolled && !enroll ? (
          <Badge>2FA enabled ✓</Badge>
        ) : (
          <div className="space-y-4">
            {!enroll ? (
              <>
                {enrollError && (
                  <p className="text-sm text-destructive">{enrollError}</p>
                )}
                <Button onClick={() => void handleEnable()} disabled={enrolling}>
                  {enrolling ? "Enabling…" : "Enable 2FA"}
                </Button>
              </>
            ) : (
              <div className="space-y-4">
                <div className="space-y-2">
                  <Label>Secret</Label>
                  <div className="flex flex-wrap items-center gap-2">
                    <code className="rounded bg-muted px-2 py-1 font-mono text-sm break-all">
                      {enroll.secret}
                    </code>
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      onClick={() => void handleCopy()}
                    >
                      {copied ? "Copied ✓" : "Copy"}
                    </Button>
                  </div>
                </div>

                <div className="space-y-2">
                  <Label>Provisioning URI</Label>
                  <p className="text-sm text-muted-foreground">
                    Scan this <code className="font-mono">otpauth://</code> URI
                    with your authenticator app, or enter the secret above
                    manually.
                  </p>
                  <code className="block rounded bg-muted px-2 py-1 font-mono text-xs break-all">
                    {enroll.uri}
                  </code>
                </div>

                {verified ? (
                  <Badge>2FA enabled ✓</Badge>
                ) : (
                  <form
                    onSubmit={handleVerify}
                    className="flex flex-wrap items-end gap-3"
                  >
                    <div className="space-y-2">
                      <Label htmlFor="tfa-code">Verification code</Label>
                      <Input
                        id="tfa-code"
                        inputMode="numeric"
                        autoComplete="one-time-code"
                        pattern="[0-9]*"
                        maxLength={6}
                        placeholder="123456"
                        className="w-32 font-mono tracking-widest"
                        required
                        value={code}
                        onChange={(e) =>
                          setCode(e.target.value.replace(/\D/g, ""))
                        }
                      />
                    </div>
                    <Button type="submit" disabled={verifying || code.length < 6}>
                      {verifying ? "Verifying…" : "Verify"}
                    </Button>
                    {verifyError && (
                      <p className="text-sm text-destructive">{verifyError}</p>
                    )}
                  </form>
                )}
              </div>
            )}
          </div>
        )}
      </CardContent>
    </Card>
  )
}
