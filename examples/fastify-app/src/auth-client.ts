// Thin client for the auth:identity HTTP surface (the accounts-app component
// deployed on wasmCloud). The contract is reached purely over HTTP — no wasm
// in this Node process — so any framework can consume it the same way.

export interface Principal {
  subject: string;
  tenant: string;
  roles: string[];
  scopes: string[];
}

export interface TokenPair {
  access_token: string;
  refresh_token: string | null;
  expires_in: number;
  session_id: string | null;
}

export interface Permission {
  target: string;
  action: string;
}

/** Error carrying the auth service's HTTP status + machine-readable code. */
export class AuthError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
  ) {
    super(`auth ${status}: ${code}`);
  }
}

export class AuthClient {
  constructor(private readonly baseUrl: string) {}

  private async call(
    path: string,
    init: RequestInit & { token?: string } = {},
  ): Promise<Response> {
    const headers = new Headers(init.headers);
    if (init.token) headers.set("authorization", `Bearer ${init.token}`);
    if (init.body) headers.set("content-type", "application/json");
    return fetch(`${this.baseUrl}${path}`, { ...init, headers });
  }

  private async json<T>(res: Response): Promise<T> {
    if (!res.ok) {
      const body = (await res.json().catch(() => ({}))) as { error?: string };
      throw new AuthError(res.status, body.error ?? "unknown");
    }
    return res.json() as Promise<T>;
  }

  /** Create a local account. */
  async register(
    email: string,
    password: string,
    tenant = "",
  ): Promise<Principal> {
    return this.json(
      await this.call("/register", {
        method: "POST",
        body: JSON.stringify({ email, password, tenant }),
      }),
    );
  }

  /** Authenticate and receive a session token pair. */
  async login(
    email: string,
    password: string,
    tenant = "",
  ): Promise<TokenPair> {
    return this.json(
      await this.call("/login", {
        method: "POST",
        body: JSON.stringify({ email, password, tenant }),
      }),
    );
  }

  /** Resolve a token to its principal (no permission check). */
  async me(token: string): Promise<Principal> {
    return this.json(await this.call("/me", { token }));
  }

  /** Revoke the session behind a token. Idempotent. */
  async logout(token: string): Promise<void> {
    const res = await this.call("/logout", { method: "POST", token });
    if (!res.ok && res.status !== 204) {
      throw new AuthError(res.status, "logout_failed");
    }
  }

  /**
   * Verify a token AND require a permission. Returns the principal if allowed;
   * throws AuthError (401 invalid/expired, 403 insufficient_scope) otherwise.
   * This is the call that guards arbitrary routes.
   */
  async verify(token: string, perm: Permission): Promise<Principal> {
    return this.json(
      await this.call("/verify", {
        method: "POST",
        token,
        body: JSON.stringify(perm),
      }),
    );
  }
}
