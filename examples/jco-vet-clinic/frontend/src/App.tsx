import { Toaster } from "@/components/ui/sonner"
import { useAuth } from "@/hooks/use-auth"
import { Header } from "@/components/header"
import { AuthCard } from "@/components/auth-card"
import { OwnerView } from "@/components/owner-view"
import { DoctorView } from "@/components/doctor-view"
import { AdminView } from "@/components/admin-view"

function App() {
  const { me, loading, login, register, logout } = useAuth()

  function renderView() {
    if (!me) return null
    // role-based view selection mirrors the original SPA
    if (me.roles.includes("admin")) return <AdminView />
    if (me.roles.includes("doctor")) return <DoctorView />
    return <OwnerView />
  }

  return (
    <div className="min-h-screen bg-background text-foreground">
      <Header me={me} onLogout={() => void logout()} />
      <main className="mx-auto max-w-5xl px-4 py-8">
        {loading ? (
          <p className="text-sm text-muted-foreground">Loading…</p>
        ) : me ? (
          renderView()
        ) : (
          <AuthCard onLogin={login} onRegister={register} />
        )}
      </main>
      <Toaster />
    </div>
  )
}

export default App
