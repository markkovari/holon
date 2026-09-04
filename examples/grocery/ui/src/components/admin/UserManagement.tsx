import { useState } from "react";
import { Users, UserCheck, Shield, Trash2, ArrowUpRight, ArrowDownRight, UserPlus, RefreshCw } from "lucide-react";
import { User, Role, RegisterPayload } from "../../types/grocery";

interface UserManagementProps {
  users: User[];
  currentUserId?: string;
  onUpdateRole: (userId: string, newRole: Role) => Promise<void>;
  onDeleteUser: (userId: string) => Promise<void>;
  onCreateUser: (payload: RegisterPayload) => Promise<void>;
  onRefresh: () => Promise<void>;
}

export const UserManagement: React.FC<UserManagementProps> = ({
  users,
  currentUserId,
  onUpdateRole,
  onDeleteUser,
  onCreateUser,
  onRefresh,
}) => {
  const [showAddForm, setShowAddForm] = useState(false);
  const [newUsername, setNewUsername] = useState("");
  const [newName, setNewName] = useState("");
  const [newEmail, setNewEmail] = useState("");
  const [newPassword, setNewPassword] = useState("");
  const [newRole, setNewRole] = useState<Role>("shopper");
  const [loading, setLoading] = useState(false);

  const handleCreateSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!newUsername || !newPassword) {
      alert("Username and password are required.");
      return;
    }
    setLoading(true);
    try {
      await onCreateUser({
        username: newUsername,
        name: newName || newUsername,
        email: newEmail || `${newUsername}@grocery.local`,
        password: newPassword,
        role: newRole,
      });
      setNewUsername("");
      setNewName("");
      setNewEmail("");
      setNewPassword("");
      setShowAddForm(false);
    } catch (err: any) {
      alert(err.message || "Failed to create user.");
    } finally {
      setLoading(false);
    }
  };

  const shoppers = users.filter((u) => u.role === "shopper");
  const admins = users.filter((u) => u.role === "admin");

  return (
    <div className="mobile-card" id="admin-user-management">
      <div className="card-title-row">
        <h3 className="card-heading">
          <Users size={18} style={{ color: "var(--admin-accent)" }} />
          <span>User Management & RBAC</span>
        </h3>
        <div style={{ display: "flex", gap: "0.4rem" }}>
          <button
            className="btn-mobile btn-mobile-outline"
            style={{ padding: "3px 7px", fontSize: "0.72rem" }}
            onClick={onRefresh}
            title="Refresh Users"
          >
            <RefreshCw size={12} />
          </button>
          <button
            id="admin-add-user-btn"
            className="btn-mobile btn-mobile-admin"
            style={{ padding: "3px 8px", fontSize: "0.72rem" }}
            onClick={() => setShowAddForm((v) => !v)}
          >
            <UserPlus size={13} />
            <span>{showAddForm ? "Cancel" : "Add User"}</span>
          </button>
        </div>
      </div>

      {/* User Group Summary Chips */}
      <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: "0.5rem", marginBottom: "0.85rem" }}>
        <div
          style={{
            background: "rgba(16, 185, 129, 0.08)",
            border: "1px solid rgba(16, 185, 129, 0.25)",
            borderRadius: "8px",
            padding: "0.5rem 0.75rem",
          }}
        >
          <div style={{ fontSize: "0.7rem", color: "var(--text-muted)" }}>Shopper Customers</div>
          <div style={{ fontSize: "1.1rem", fontWeight: 700, color: "#34d399" }}>
            {shoppers.length} active
          </div>
        </div>
        <div
          style={{
            background: "rgba(99, 102, 241, 0.08)",
            border: "1px solid rgba(99, 102, 241, 0.25)",
            borderRadius: "8px",
            padding: "0.5rem 0.75rem",
          }}
        >
          <div style={{ fontSize: "0.7rem", color: "var(--text-muted)" }}>Store Administrators</div>
          <div style={{ fontSize: "1.1rem", fontWeight: 700, color: "#818cf8" }}>
            {admins.length} active
          </div>
        </div>
      </div>

      {/* Provision New User Form */}
      {showAddForm && (
        <form
          onSubmit={handleCreateSubmit}
          style={{
            background: "rgba(15, 23, 42, 0.9)",
            border: "1px solid var(--border)",
            borderRadius: "10px",
            padding: "0.85rem",
            marginBottom: "1rem",
            display: "flex",
            flexDirection: "column",
            gap: "0.6rem",
          }}
        >
          <div style={{ fontWeight: 600, fontSize: "0.85rem", color: "#818cf8" }}>
            Provision New System Account
          </div>
          <div style={{ display: "grid", gridTemplateColumns: "1.5fr 1fr", gap: "0.5rem" }}>
            <input
              type="text"
              placeholder="Username"
              value={newUsername}
              onChange={(e) => setNewUsername(e.target.value)}
              className="mobile-input"
              required
            />
            <select
              value={newRole}
              onChange={(e) => setNewRole(e.target.value as Role)}
              className="mobile-input"
            >
              <option value="shopper">Shopper</option>
              <option value="admin">Store Admin</option>
            </select>
          </div>
          <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: "0.5rem" }}>
            <input
              type="text"
              placeholder="Display Name"
              value={newName}
              onChange={(e) => setNewName(e.target.value)}
              className="mobile-input"
            />
            <input
              type="password"
              placeholder="Password"
              value={newPassword}
              onChange={(e) => setNewPassword(e.target.value)}
              className="mobile-input"
              required
            />
          </div>
          <input
            type="email"
            placeholder="Email (e.g. user@store.local)"
            value={newEmail}
            onChange={(e) => setNewEmail(e.target.value)}
            className="mobile-input"
          />
          <button
            type="submit"
            className="btn-mobile btn-mobile-admin"
            style={{ padding: "0.55rem" }}
            disabled={loading}
          >
            {loading ? "Creating..." : "Save New Account"}
          </button>
        </form>
      )}

      {/* Users List */}
      <div style={{ display: "flex", flexDirection: "column", gap: "0.6rem" }}>
        {users.map((u) => {
          const isSelf = u.id === currentUserId;
          const isAdmin = u.role === "admin";

          return (
            <div
              key={u.id}
              style={{
                display: "flex",
                justifyContent: "space-between",
                alignItems: "center",
                padding: "0.75rem 0.85rem",
                background: "rgba(30, 41, 59, 0.45)",
                borderRadius: "10px",
                border: isSelf ? "1px solid rgba(99, 102, 241, 0.4)" : "1px solid var(--border)",
              }}
            >
              <div style={{ display: "flex", alignItems: "center", gap: "0.65rem" }}>
                <div
                  style={{
                    width: "36px",
                    height: "36px",
                    borderRadius: "50%",
                    background: isAdmin ? "rgba(99, 102, 241, 0.2)" : "rgba(16, 185, 129, 0.2)",
                    color: isAdmin ? "#a5b4fc" : "#34d399",
                    display: "flex",
                    alignItems: "center",
                    justifyContent: "center",
                    fontWeight: 700,
                    fontSize: "0.95rem",
                  }}
                >
                  {isAdmin ? <Shield size={18} /> : <UserCheck size={18} />}
                </div>

                <div>
                  <div style={{ display: "flex", alignItems: "center", gap: "0.4rem" }}>
                    <span style={{ fontWeight: 600, fontSize: "0.88rem" }}>{u.name}</span>
                    {isSelf && (
                      <span className="badge-mini badge-purple" style={{ fontSize: "0.58rem" }}>
                        You
                      </span>
                    )}
                  </div>
                  <div style={{ fontSize: "0.72rem", color: "var(--text-muted)" }}>
                    <span className="mono">@{u.username}</span> · {u.email}
                  </div>
                </div>
              </div>

              <div style={{ display: "flex", alignItems: "center", gap: "0.5rem" }}>
                <span className={`badge-mini ${isAdmin ? "badge-purple" : "badge-green"}`}>
                  {isAdmin ? "Admin" : "Shopper"}
                </span>

                {/* Role Switcher Action */}
                <button
                  id={`toggle-role-${u.id}`}
                  className="btn-mobile btn-mobile-outline"
                  style={{ padding: "3px 6px", fontSize: "0.68rem" }}
                  onClick={() => onUpdateRole(u.id, isAdmin ? "shopper" : "admin")}
                  title={isAdmin ? "Demote to Shopper" : "Promote to Admin"}
                >
                  {isAdmin ? (
                    <>
                      <ArrowDownRight size={11} />
                      <span>Shopper</span>
                    </>
                  ) : (
                    <>
                      <ArrowUpRight size={11} />
                      <span>Admin</span>
                    </>
                  )}
                </button>

                {/* Delete user */}
                {!isSelf && (
                  <button
                    id={`delete-user-${u.id}`}
                    className="btn-mobile btn-mobile-outline"
                    style={{ padding: "3px 6px", color: "#f87171" }}
                    onClick={() => onDeleteUser(u.id)}
                    title="Delete User"
                  >
                    <Trash2 size={12} />
                  </button>
                )}
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
};
