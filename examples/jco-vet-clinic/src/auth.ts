// Thin wrappers over the composed auth-guard component (auth + rate-limiter +
// audit-log, wac-plugged into one wasm) and notify-dispatch. All RBAC, account,
// session and authorization logic is the real component — this file only adapts
// names and centralises the tenant.

// Composed auth-guard, transpiled to JS. Exports the full auth:identity surface.
import { accounts, authorizer, rbac, session } from "../gen/auth/auth_guard.composed.js";
// notify-dispatch, transpiled.
import { dispatcher as notify } from "../gen/notify/notify_dispatch.js";
import { get as cfgGet } from "./shims/config.js";

export const TENANT = (cfgGet("default-tenant") as string) ?? "acme-vet";

export interface Principal {
  subject: string;
  tenant: string;
  roles: string[];
  scopes: string[];
}
export interface TokenPair {
  accessToken: string;
  refreshToken?: string;
  expiresIn: bigint | number;
  sessionId?: string;
}
export interface Permission {
  target: string;
  action: string;
}

// ---- accounts + sessions -------------------------------------------------

export function register(email: string, password: string): Principal {
  return accounts.register(email, password, TENANT) as Principal;
}
export function login(email: string, password: string): TokenPair {
  return accounts.login(email, password, TENANT) as TokenPair;
}
export function introspect(token: string): Principal {
  return authorizer.introspect(token) as Principal;
}
export function logout(token: string): void {
  session.revoke(token);
}

// ---- authorization -------------------------------------------------------

/** Verify the token AND require the permission; throws an auth-error variant. */
export function authorize(token: string, perm: Permission): Principal {
  return authorizer.authorize(token, perm) as Principal;
}

// ---- RBAC seeding --------------------------------------------------------

export function setRolePermissions(role: string, permissions: Permission[]): void {
  rbac.setRolePermissions(TENANT, role, permissions);
}
export function assignRole(subject: string, role: string): void {
  rbac.assignRole(TENANT, subject, role);
}
export function rolesFor(subject: string): string[] {
  return rbac.rolesFor(TENANT, subject) as string[];
}

// ---- notifications -------------------------------------------------------

/**
 * Fire a confirmation notification. Returns true if the dispatcher accepted it.
 * The configured gateway is a local sink, so a network failure is treated as
 * "queued" — the example demonstrates the wiring, not a live email vendor.
 */
export function notifyEmail(to: string, subject: string, body: string): boolean {
  try {
    notify.send({ channel: "email", target: to, subject, body });
    return true;
  } catch {
    return false; // sink unreachable — fine for the demo
  }
}
