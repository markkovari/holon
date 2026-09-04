import type React from "react";
import { Store, ShoppingBag, ShieldCheck, RefreshCw, UserCheck, LogIn, UserPlus, LogOut } from "lucide-react";
import { User as UserType } from "../../types/grocery";

interface HeaderProps {
  currentUser: UserType | null;
  adminViewMode: "console" | "storefront";
  onSetAdminViewMode: (mode: "console" | "storefront") => void;
  onSync: () => void;
  cartItemCount: number;
  cartTotalCents: number;
  onOpenCart: () => void;
  onOpenAuth: (tab?: "login" | "register") => void;
  onLogout: () => void;
  loading: boolean;
}

export const Header: React.FC<HeaderProps> = ({
  currentUser,
  adminViewMode,
  onSetAdminViewMode,
  onSync,
  cartItemCount,
  cartTotalCents,
  onOpenCart,
  onOpenAuth,
  onLogout,
  loading,
}) => {
  const isAdmin = currentUser?.role === "admin";

  return (
    <header className="app-header">
      <div className="header-inner">
        <div className="header-top">
          <div className="brand-group">
            <div className="brand-badge">
              <Store size={18} />
            </div>
            <div>
              <div className="brand-title">Holon Grocery</div>
              <div className="brand-sub">WASI 0.2 Component · Pure WASM RBAC</div>
            </div>
          </div>

          <div style={{ display: "flex", alignItems: "center", gap: "0.5rem" }}>
            {/* Real User Authentication Status */}
            {currentUser ? (
              <div
                id="user-session-pill"
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: "0.45rem",
                  background: "rgba(30, 41, 59, 0.6)",
                  border: "1px solid var(--border)",
                  borderRadius: "20px",
                  padding: "3px 8px 3px 4px",
                  fontSize: "0.75rem",
                }}
              >
                <div
                  style={{
                    width: "22px",
                    height: "22px",
                    borderRadius: "50%",
                    background: isAdmin
                      ? "rgba(99, 102, 241, 0.3)"
                      : "rgba(16, 185, 129, 0.3)",
                    color: isAdmin ? "#a5b4fc" : "#34d399",
                    display: "flex",
                    alignItems: "center",
                    justifyContent: "center",
                    fontSize: "0.7rem",
                    fontWeight: 700,
                  }}
                >
                  {isAdmin ? <ShieldCheck size={13} /> : <UserCheck size={13} />}
                </div>
                <span style={{ fontWeight: 600, color: "var(--text-primary)" }}>
                  {currentUser.name}
                </span>
                <span
                  className={`badge-mini ${isAdmin ? "badge-purple" : "badge-green"}`}
                  style={{ fontSize: "0.62rem" }}
                >
                  {currentUser.role.toUpperCase()}
                </span>
                <button
                  id="header-logout-btn"
                  onClick={onLogout}
                  className="btn-mobile btn-mobile-outline"
                  style={{ padding: "2px 6px", fontSize: "0.68rem", marginLeft: "2px" }}
                  title="Sign Out"
                >
                  <LogOut size={11} />
                  <span>Sign Out</span>
                </button>
              </div>
            ) : (
              <div style={{ display: "flex", alignItems: "center", gap: "0.4rem" }}>
                <button
                  id="header-signin-btn"
                  onClick={() => onOpenAuth("login")}
                  className="btn-mobile btn-mobile-primary"
                  style={{ padding: "4px 10px", fontSize: "0.75rem" }}
                >
                  <LogIn size={13} />
                  <span>Sign In</span>
                </button>
                <button
                  id="header-register-btn"
                  onClick={() => onOpenAuth("register")}
                  className="btn-mobile btn-mobile-outline"
                  style={{ padding: "4px 10px", fontSize: "0.75rem" }}
                >
                  <UserPlus size={13} />
                  <span>Register</span>
                </button>
              </div>
            )}

            <button
              id="header-sync-btn"
              onClick={onSync}
              className="btn-mobile btn-mobile-outline"
              style={{ padding: "4px 8px", fontSize: "0.75rem" }}
              title="Reload Catalog"
              disabled={loading}
            >
              <RefreshCw size={13} />
              <span>Sync</span>
            </button>
          </div>
        </div>

        {/* Authenticated Admin Management Navigation (ONLY visible when signed in as Admin) */}
        {isAdmin && (
          <div style={{ display: "flex", alignItems: "center", gap: "0.5rem" }}>
            <button
              id="admin-view-console-btn"
              className={`chip-btn ${adminViewMode === "console" ? "active" : ""}`}
              onClick={() => onSetAdminViewMode("console")}
              style={{ padding: "0.4rem 0.8rem", fontSize: "0.8rem" }}
            >
              <ShieldCheck size={14} />
              <span>Admin Console</span>
            </button>
            <button
              id="admin-view-store-btn"
              className={`chip-btn ${adminViewMode === "storefront" ? "active" : ""}`}
              onClick={() => onSetAdminViewMode("storefront")}
              style={{ padding: "0.4rem 0.8rem", fontSize: "0.8rem" }}
            >
              <ShoppingBag size={14} />
              <span>Storefront Preview</span>
            </button>
          </div>
        )}

        {/* Desktop Quick Header Basket Trigger */}
        <div className="desktop-header-actions">
          {(!isAdmin || adminViewMode === "storefront") && (
            <button
              id="header-open-cart-btn"
              className="btn-mobile btn-mobile-primary"
              onClick={onOpenCart}
              style={{ padding: "0.45rem 1rem", fontSize: "0.85rem" }}
            >
              <ShoppingBag size={16} />
              <span>Basket ({cartItemCount})</span>
              {cartItemCount > 0 && (
                <span style={{ fontWeight: 800, marginLeft: "4px" }}>
                  ${(cartTotalCents / 100).toFixed(2)}
                </span>
              )}
            </button>
          )}
        </div>
      </div>
    </header>
  );
};
