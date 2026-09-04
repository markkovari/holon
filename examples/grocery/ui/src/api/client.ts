import {
  Product,
  ScanResult,
  Role,
  User,
  AuthResponse,
  RegisterPayload,
  LoginPayload,
} from "../types/grocery";

let currentToken: string = localStorage.getItem("holon_auth_token") || "";

export function getAuthToken(): string {
  return currentToken;
}

export function setAuthToken(token: string) {
  currentToken = token;
  localStorage.setItem("holon_auth_token", token);
}

export function clearAuthToken() {
  currentToken = "";
  localStorage.removeItem("holon_auth_token");
}

function authHeaders(): Record<string, string> {
  const token = getAuthToken();
  return token ? { Authorization: `Bearer ${token}` } : {};
}

// ----------------- AUTH & USER MANAGEMENT APIS -----------------

export async function registerUser(payload: RegisterPayload): Promise<AuthResponse> {
  const res = await fetch("/api/auth/register", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify(payload),
  });

  if (!res.ok) {
    const err = await res.json().catch(() => ({}));
    throw new Error(err.error || `Registration failed (${res.status})`);
  }

  const data: AuthResponse = await res.json();
  setAuthToken(data.token);
  return data;
}

export async function loginUser(payload: LoginPayload): Promise<AuthResponse> {
  const res = await fetch("/api/auth/login", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify(payload),
  });

  if (!res.ok) {
    const err = await res.json().catch(() => ({}));
    throw new Error(err.error || `Sign in failed (${res.status})`);
  }

  const data: AuthResponse = await res.json();
  setAuthToken(data.token);
  return data;
}

export async function fetchCurrentUser(): Promise<User | null> {
  const token = getAuthToken();
  if (!token) return null;

  const res = await fetch("/api/auth/me", {
    headers: {
      ...authHeaders(),
    },
  });

  if (!res.ok) {
    clearAuthToken();
    return null;
  }

  const data = await res.json();
  return data.user;
}

export async function logoutUser(): Promise<void> {
  await fetch("/api/auth/logout", {
    method: "POST",
    headers: {
      ...authHeaders(),
    },
  }).catch(() => {});
  clearAuthToken();
}

export async function fetchAdminUsers(): Promise<User[]> {
  const res = await fetch("/api/admin/users", {
    headers: {
      ...authHeaders(),
    },
  });

  if (!res.ok) {
    const err = await res.json().catch(() => ({}));
    throw new Error(err.error || `Failed to fetch users: ${res.status}`);
  }

  return res.json();
}

export async function updateUserRole(userId: string, role: Role): Promise<User> {
  const res = await fetch(`/api/admin/users/${encodeURIComponent(userId)}/role`, {
    method: "PATCH",
    headers: {
      "Content-Type": "application/json",
      ...authHeaders(),
    },
    body: JSON.stringify({ role }),
  });

  if (!res.ok) {
    const err = await res.json().catch(() => ({}));
    throw new Error(err.error || `Failed to update user role (${res.status})`);
  }

  return res.json();
}

export async function deleteUser(userId: string): Promise<void> {
  const res = await fetch(`/api/admin/users/${encodeURIComponent(userId)}`, {
    method: "DELETE",
    headers: {
      ...authHeaders(),
    },
  });

  if (!res.ok) {
    const err = await res.json().catch(() => ({}));
    throw new Error(err.error || `Failed to delete user (${res.status})`);
  }
}

// ----------------- GROCERY DOMAIN APIS -----------------

export async function fetchProducts(): Promise<Product[]> {
  const res = await fetch("/api/products", {
    headers: {
      ...authHeaders(),
    },
  });
  if (!res.ok) {
    throw new Error(`Failed to load products: ${res.status}`);
  }
  return res.json();
}

export async function scanBarcodeBytes(
  bytes: ArrayBuffer | Uint8Array,
  _role?: Role
): Promise<ScanResult> {
  const res = await fetch("/api/scan", {
    method: "POST",
    headers: {
      "Content-Type": "image/png",
      ...authHeaders(),
    },
    body: bytes as any,
  });

  if (!res.ok) {
    const errData = await res.json().catch(() => ({}));
    throw new Error(errData.error || `Scan failed with status ${res.status}`);
  }

  return res.json();
}

export async function fetchFixtureBytes(fixtureName: string): Promise<ArrayBuffer> {
  const res = await fetch(`/fixtures/${fixtureName}`);
  if (!res.ok) {
    throw new Error(`Could not load fixture ${fixtureName}`);
  }
  return res.arrayBuffer();
}

export async function checkoutCart(
  items: Array<{ barcode: string; quantity: number }>
): Promise<{ order_id: string }> {
  const res = await fetch("/api/checkout", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      ...authHeaders(),
    },
    body: JSON.stringify({ items }),
  });

  if (!res.ok) {
    const err = await res.json().catch(() => ({}));
    throw new Error(err.error || `Checkout failed (${res.status})`);
  }

  return res.json();
}

export async function adjustProductStock(
  barcode: string,
  delta: number
): Promise<{ stock: number }> {
  const res = await fetch(`/api/products/${encodeURIComponent(barcode)}/stock`, {
    method: "PATCH",
    headers: {
      "Content-Type": "application/json",
      ...authHeaders(),
    },
    body: JSON.stringify({ delta }),
  });

  if (!res.ok) {
    const err = await res.json().catch(() => ({}));
    throw new Error(err.error || `Failed to update stock (${res.status})`);
  }

  return res.json();
}

export async function registerNewProduct(product: {
  barcode: string;
  symbology: string;
  name: string;
  category: string;
  price_cents: number;
  stock: number;
  icon: string;
  description: string;
}): Promise<Product> {
  const res = await fetch("/api/products", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      ...authHeaders(),
    },
    body: JSON.stringify(product),
  });

  if (!res.ok) {
    const err = await res.json().catch(() => ({}));
    throw new Error(err.error || `Failed to register product (${res.status})`);
  }

  return res.json();
}
