import { useState, useCallback } from "react";
import { User, Role, LoginPayload, RegisterPayload } from "../types/grocery";
import * as api from "../api/client";

export interface UseAuthOptions {
  onAuthSuccess?: (user: User) => Promise<void> | void;
  showToast?: (msg: string) => void;
}

export function useAuth(options?: UseAuthOptions) {
  const [currentUser, setCurrentUser] = useState<User | null>(null);
  const [adminViewMode, setAdminViewMode] = useState<"console" | "storefront">("console");
  const [authModalOpen, setAuthModalOpen] = useState(false);
  const [authModalTab, setAuthModalTab] = useState<"login" | "register">("login");
  const [adminUsers, setAdminUsers] = useState<User[]>([]);

  const openAuthModal = useCallback((tab: "login" | "register" = "login") => {
    setAuthModalTab(tab);
    setAuthModalOpen(true);
  }, []);

  const loadAdminUsers = useCallback(async () => {
    try {
      const users = await api.fetchAdminUsers();
      setAdminUsers(users);
    } catch (err: any) {
      console.warn("Could not load admin users:", err.message);
    }
  }, []);

  const initAuth = useCallback(async () => {
    try {
      const user = await api.fetchCurrentUser();
      setCurrentUser(user);
      if (user?.role === "admin") {
        setAdminViewMode("console");
        await loadAdminUsers();
      } else {
        setAdminViewMode("storefront");
      }
      if (user && options?.onAuthSuccess) {
        await options.onAuthSuccess(user);
      }
    } catch {
      setCurrentUser(null);
      setAdminViewMode("storefront");
    }
  }, [loadAdminUsers, options]);

  const handleLogin = useCallback(
    async (payload: LoginPayload) => {
      const auth = await api.loginUser(payload);
      setCurrentUser(auth.user);
      options?.showToast?.(`Signed in as ${auth.user.name} (${auth.user.role.toUpperCase()})`);
      if (options?.onAuthSuccess) {
        await options.onAuthSuccess(auth.user);
      }
      if (auth.user.role === "admin") {
        setAdminViewMode("console");
        await loadAdminUsers();
      } else {
        setAdminViewMode("storefront");
      }
    },
    [loadAdminUsers, options]
  );

  const handleRegister = useCallback(
    async (payload: RegisterPayload) => {
      const auth = await api.registerUser(payload);
      setCurrentUser(auth.user);
      options?.showToast?.(`Account created! Welcome, ${auth.user.name}`);
      if (options?.onAuthSuccess) {
        await options.onAuthSuccess(auth.user);
      }
      if (auth.user.role === "admin") {
        setAdminViewMode("console");
        await loadAdminUsers();
      } else {
        setAdminViewMode("storefront");
      }
    },
    [loadAdminUsers, options]
  );

  const handleLogout = useCallback(async () => {
    try {
      await api.logoutUser();
    } catch {
      api.clearAuthToken();
    }
    setCurrentUser(null);
    setAdminViewMode("storefront");
    options?.showToast?.("Signed out. Switched to guest mode.");
  }, [options]);

  const handleUpdateUserRole = useCallback(
    async (userId: string, newRole: Role) => {
      try {
        const updated = await api.updateUserRole(userId, newRole);
        options?.showToast?.(`Updated ${updated.name}'s role to ${newRole.toUpperCase()}`);
        await loadAdminUsers();
        if (currentUser?.id === userId) {
          setCurrentUser(updated);
          if (updated.role !== "admin") {
            setAdminViewMode("storefront");
          }
        }
      } catch (err: any) {
        options?.showToast?.(`Error: ${err.message}`);
      }
    },
    [currentUser, loadAdminUsers, options]
  );

  const handleDeleteUser = useCallback(
    async (userId: string) => {
      if (!confirm("Are you sure you want to remove this user?")) return;
      try {
        await api.deleteUser(userId);
        options?.showToast?.("User successfully removed.");
        await loadAdminUsers();
      } catch (err: any) {
        options?.showToast?.(`Error: ${err.message}`);
      }
    },
    [loadAdminUsers, options]
  );

  const handleAdminCreateUser = useCallback(
    async (payload: RegisterPayload) => {
      try {
        const res = await api.registerUser(payload);
        options?.showToast?.(`User ${res.user.username} created successfully!`);
        await loadAdminUsers();
      } catch (err: any) {
        options?.showToast?.(`Error: ${err.message}`);
      }
    },
    [loadAdminUsers, options]
  );

  return {
    currentUser,
    setCurrentUser,
    role: currentUser?.role || "shopper",
    adminViewMode,
    setAdminViewMode,
    authModalOpen,
    setAuthModalOpen,
    authModalTab,
    openAuthModal,
    adminUsers,
    loadAdminUsers,
    initAuth,
    handleLogin,
    handleRegister,
    handleLogout,
    handleUpdateUserRole,
    handleDeleteUser,
    handleAdminCreateUser,
  };
}
