// Bootstrap the three vet-clinic roles + their permissions, and (optionally) a
// demo user per role. Idempotent — safe to call on every boot.

import * as auth from "./auth.js";

export const ROLES: Record<string, auth.Permission[]> = {
  // Pet owners manage their own pets and book appointments.
  "pet-owner": [
    { target: "pets", action: "read" },
    { target: "pets", action: "write" },
    { target: "appointments", action: "read" },
    { target: "appointments", action: "write" },
  ],
  // Doctors see patients + appointments and write visit notes.
  doctor: [
    { target: "pets", action: "read" },
    { target: "appointments", action: "read" },
    { target: "appointments", action: "write" },
    { target: "notes", action: "write" },
  ],
  // Admin: everything (wildcard).
  admin: [{ target: "*", action: "*" }],
};

/** Define every role -> permission mapping. */
export function seedRoles(): void {
  for (const [role, perms] of Object.entries(ROLES)) {
    auth.setRolePermissions(role, perms);
  }
}

export interface DemoUser {
  email: string;
  password: string;
  role: string;
  subject: string;
}

/**
 * Register one user per role (ignoring already-exists) and assign the role.
 * Returns the demo users so the server can print credentials.
 */
export function seedDemoUsers(): DemoUser[] {
  const demos = [
    { email: "owner@acme-vet.test", password: "ownerpass1", role: "pet-owner" },
    { email: "doctor@acme-vet.test", password: "doctorpass1", role: "doctor" },
    { email: "admin@acme-vet.test", password: "adminpass1", role: "admin" },
  ];
  const out: DemoUser[] = [];
  for (const d of demos) {
    let subject = "";
    try {
      const p = auth.register(d.email, d.password);
      subject = p.subject;
    } catch (e) {
      // already-exists -> recover the subject by logging in + introspecting.
      const tp = auth.login(d.email, d.password);
      subject = auth.introspect(tp.accessToken).subject;
    }
    auth.assignRole(subject, d.role);
    out.push({ ...d, subject });
  }
  return out;
}
