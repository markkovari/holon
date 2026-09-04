import { useState, useEffect } from "react";
import { X, LogIn, UserPlus, ShieldCheck, ShoppingBag, Sparkles, AlertCircle } from "lucide-react";
import { Role, RegisterPayload, LoginPayload } from "../../types/grocery";

interface AuthModalProps {
  isOpen: boolean;
  initialTab?: "login" | "register";
  onClose: () => void;
  onLogin: (payload: LoginPayload) => Promise<void>;
  onRegister: (payload: RegisterPayload) => Promise<void>;
}

export const AuthModal: React.FC<AuthModalProps> = ({
  isOpen,
  initialTab = "login",
  onClose,
  onLogin,
  onRegister,
}) => {
  const [tab, setTab] = useState<"login" | "register">(initialTab);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (isOpen && initialTab) {
      setTab(initialTab);
      setError(null);
    }
  }, [isOpen, initialTab]);

  // Login form state
  const [loginUsername, setLoginUsername] = useState("");
  const [loginPassword, setLoginPassword] = useState("");

  // Register form state
  const [regRole, setRegRole] = useState<Role>("shopper");
  const [regUsername, setRegUsername] = useState("");
  const [regName, setRegName] = useState("");
  const [regEmail, setRegEmail] = useState("");
  const [regPassword, setRegPassword] = useState("");

  if (!isOpen) return null;

  const handleLoginSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!loginUsername || !loginPassword) {
      setError("Please enter both username and password.");
      return;
    }
    setError(null);
    setLoading(true);
    try {
      await onLogin({ username: loginUsername, password: loginPassword });
      onClose();
    } catch (err: any) {
      setError(err.message || "Failed to sign in.");
    } finally {
      setLoading(false);
    }
  };

  const handleRegisterSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!regUsername || !regPassword) {
      setError("Username and password are required.");
      return;
    }
    setError(null);
    setLoading(true);
    try {
      await onRegister({
        username: regUsername,
        name: regName || regUsername,
        email: regEmail || `${regUsername}@grocery.local`,
        password: regPassword,
        role: regRole,
      });
      onClose();
    } catch (err: any) {
      setError(err.message || "Failed to register.");
    } finally {
      setLoading(false);
    }
  };

  const handleFillDemo = (user: string, pass: string) => {
    setLoginUsername(user);
    setLoginPassword(pass);
    setError(null);
  };

  return (
    <div className="mobile-modal-overlay" onClick={onClose}>
      <div
        className="mobile-bottom-sheet modal-content"
        style={{ maxWidth: "480px" }}
        onClick={(e) => e.stopPropagation()}
      >
        <div
          style={{
            display: "flex",
            justifyContent: "space-between",
            alignItems: "center",
            marginBottom: "1rem",
          }}
        >
          <div style={{ display: "flex", alignItems: "center", gap: "0.5rem" }}>
            <ShieldCheck size={22} style={{ color: "var(--accent)" }} />
            <h3 style={{ fontSize: "1.2rem", fontWeight: 700 }}>Store Identity & RBAC</h3>
          </div>
          <button
            onClick={onClose}
            style={{
              background: "transparent",
              border: "none",
              color: "var(--text-muted)",
              cursor: "pointer",
            }}
          >
            <X size={20} />
          </button>
        </div>

        {/* Tab Switcher */}
        <div
          style={{
            display: "grid",
            gridTemplateColumns: "1fr 1fr",
            background: "rgba(15, 23, 42, 0.6)",
            borderRadius: "10px",
            padding: "3px",
            marginBottom: "1.2rem",
            border: "1px solid var(--border)",
          }}
        >
          <button
            id="auth-tab-login"
            className={`role-tab ${tab === "login" ? "active" : ""}`}
            style={{
              padding: "0.5rem",
              borderRadius: "8px",
              display: "flex",
              justifyContent: "center",
              alignItems: "center",
              gap: "0.4rem",
              fontSize: "0.85rem",
              fontWeight: 600,
            }}
            onClick={() => {
              setTab("login");
              setError(null);
            }}
          >
            <LogIn size={15} />
            <span>Sign In</span>
          </button>
          <button
            id="auth-tab-register"
            className={`role-tab ${tab === "register" ? "active" : ""}`}
            style={{
              padding: "0.5rem",
              borderRadius: "8px",
              display: "flex",
              justifyContent: "center",
              alignItems: "center",
              gap: "0.4rem",
              fontSize: "0.85rem",
              fontWeight: 600,
            }}
            onClick={() => {
              setTab("register");
              setError(null);
            }}
          >
            <UserPlus size={15} />
            <span>Create Account</span>
          </button>
        </div>

        {/* Error Alert */}
        {error && (
          <div
            style={{
              background: "rgba(239, 68, 68, 0.12)",
              border: "1px solid rgba(239, 68, 68, 0.3)",
              borderRadius: "8px",
              padding: "0.65rem 0.85rem",
              marginBottom: "1rem",
              color: "#f87171",
              fontSize: "0.82rem",
              display: "flex",
              alignItems: "center",
              gap: "0.5rem",
            }}
          >
            <AlertCircle size={16} />
            <span>{error}</span>
          </div>
        )}

        {/* SIGN IN FORM */}
        {tab === "login" ? (
          <div>
            <form onSubmit={handleLoginSubmit} style={{ display: "flex", flexDirection: "column", gap: "0.75rem" }}>
              <div>
                <label style={{ fontSize: "0.75rem", color: "var(--text-secondary)", marginBottom: "0.25rem", display: "block" }}>
                  Username
                </label>
                <input
                  id="login-username"
                  type="text"
                  placeholder="e.g. shopper or admin"
                  value={loginUsername}
                  onChange={(e) => setLoginUsername(e.target.value)}
                  className="mobile-input"
                  autoFocus
                />
              </div>

              <div>
                <label style={{ fontSize: "0.75rem", color: "var(--text-secondary)", marginBottom: "0.25rem", display: "block" }}>
                  Password
                </label>
                <input
                  id="login-password"
                  type="password"
                  placeholder="e.g. shopper123 or admin123"
                  value={loginPassword}
                  onChange={(e) => setLoginPassword(e.target.value)}
                  className="mobile-input"
                />
              </div>

              <button
                id="submit-login-btn"
                type="submit"
                className="btn-mobile btn-mobile-primary"
                style={{ marginTop: "0.5rem", padding: "0.75rem" }}
                disabled={loading}
              >
                <LogIn size={16} />
                <span>{loading ? "Authenticating..." : "Sign In"}</span>
              </button>
            </form>

            {/* Quick Demo Credentials */}
            <div style={{ marginTop: "1.25rem", borderTop: "1px solid var(--border)", paddingTop: "1rem" }}>
              <div style={{ fontSize: "0.72rem", color: "var(--text-muted)", fontWeight: 600, textTransform: "uppercase", letterSpacing: "0.04em", marginBottom: "0.6rem" }}>
                Quick-Fill Demo Credentials:
              </div>
              <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: "0.5rem" }}>
                <button
                  id="demo-shopper-login-btn"
                  type="button"
                  className="chip-btn"
                  style={{ padding: "0.55rem 0.75rem", display: "flex", alignItems: "center", justifyContent: "center", gap: "0.4rem" }}
                  onClick={() => handleFillDemo("shopper", "shopper123")}
                >
                  <ShoppingBag size={14} style={{ color: "#34d399" }} />
                  <span>Fill Shopper</span>
                </button>
                <button
                  id="demo-admin-login-btn"
                  type="button"
                  className="chip-btn"
                  style={{ padding: "0.55rem 0.75rem", display: "flex", alignItems: "center", justifyContent: "center", gap: "0.4rem" }}
                  onClick={() => handleFillDemo("admin", "admin123")}
                >
                  <ShieldCheck size={14} style={{ color: "#818cf8" }} />
                  <span>Fill Admin</span>
                </button>
              </div>
            </div>
          </div>
        ) : (
          /* CREATE ACCOUNT FORM */
          <form onSubmit={handleRegisterSubmit} style={{ display: "flex", flexDirection: "column", gap: "0.75rem" }}>
            <div>
              <label style={{ fontSize: "0.75rem", color: "var(--text-secondary)", marginBottom: "0.35rem", display: "block" }}>
                Select User Group / Role
              </label>
              <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: "0.5rem" }}>
                <button
                  id="reg-role-shopper"
                  type="button"
                  className={`chip-btn ${regRole === "shopper" ? "active" : ""}`}
                  style={{ padding: "0.6rem 0.5rem", textAlign: "center", display: "flex", flexDirection: "column", alignItems: "center", gap: "0.2rem" }}
                  onClick={() => setRegRole("shopper")}
                >
                  <ShoppingBag size={16} style={{ color: "#34d399" }} />
                  <span style={{ fontWeight: 700, fontSize: "0.8rem" }}>Customer Shopper</span>
                  <span style={{ fontSize: "0.65rem", color: "var(--text-muted)" }}>Scan & Checkout</span>
                </button>
                <button
                  id="reg-role-admin"
                  type="button"
                  className={`chip-btn ${regRole === "admin" ? "active" : ""}`}
                  style={{ padding: "0.6rem 0.5rem", textAlign: "center", display: "flex", flexDirection: "column", alignItems: "center", gap: "0.2rem" }}
                  onClick={() => setRegRole("admin")}
                >
                  <ShieldCheck size={16} style={{ color: "#818cf8" }} />
                  <span style={{ fontWeight: 700, fontSize: "0.8rem" }}>Store Admin</span>
                  <span style={{ fontSize: "0.65rem", color: "var(--text-muted)" }}>Inventory & Users</span>
                </button>
              </div>
            </div>

            <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: "0.5rem" }}>
              <div>
                <label style={{ fontSize: "0.72rem", color: "var(--text-secondary)" }}>Username</label>
                <input
                  id="register-username"
                  type="text"
                  placeholder="e.g. maria_rodriguez"
                  value={regUsername}
                  onChange={(e) => setRegUsername(e.target.value)}
                  className="mobile-input"
                  required
                />
              </div>
              <div>
                <label style={{ fontSize: "0.72rem", color: "var(--text-secondary)" }}>Full Name</label>
                <input
                  id="register-name"
                  type="text"
                  placeholder="e.g. Maria Rodriguez"
                  value={regName}
                  onChange={(e) => setRegName(e.target.value)}
                  className="mobile-input"
                />
              </div>
            </div>

            <div>
              <label style={{ fontSize: "0.72rem", color: "var(--text-secondary)" }}>Email Address</label>
              <input
                id="register-email"
                type="email"
                placeholder="e.g. maria@store.local"
                value={regEmail}
                onChange={(e) => setRegEmail(e.target.value)}
                className="mobile-input"
              />
            </div>

            <div>
              <label style={{ fontSize: "0.72rem", color: "var(--text-secondary)" }}>Password</label>
              <input
                id="register-password"
                type="password"
                placeholder="Create password"
                value={regPassword}
                onChange={(e) => setRegPassword(e.target.value)}
                className="mobile-input"
                required
              />
            </div>

            <button
              id="submit-register-btn"
              type="submit"
              className="btn-mobile btn-mobile-primary"
              style={{ marginTop: "0.4rem", padding: "0.75rem" }}
              disabled={loading}
            >
              <Sparkles size={16} />
              <span>{loading ? "Registering..." : `Register as ${regRole === "admin" ? "Store Admin" : "Shopper"}`}</span>
            </button>
          </form>
        )}
      </div>
    </div>
  );
};
