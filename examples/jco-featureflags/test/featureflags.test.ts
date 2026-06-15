// E2E for the featureflags:guard component, run in-process via jco (in-memory kv
// shim + config-shim flag definitions). Covers config booleans, percentage
// rollout stickiness, runtime rule creation, tenant-scoped vs global precedence,
// clear/fallback, and listing.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { evaluator as flags } from "../gen/feature_flags.js";

const ctx = (tenant: string, subject: string) => ({ tenant, subject });
const ON = { tag: "enabled" } as const;
const OFF = { tag: "disabled" } as const;

describe("featureflags:guard component", () => {
  it("reads a boolean flag from config", () => {
    assert.equal(flags.isEnabled("new-checkout", ctx("acme", "u1")), true); // config "true"
    assert.equal(flags.isEnabled("dark-mode", ctx("acme", "u1")), false); // config "false"
  });

  it("unknown flag defaults to off", () => {
    assert.equal(flags.isEnabled("does-not-exist", ctx("acme", "u1")), false);
  });

  it("percentage rollout is sticky per subject", () => {
    const a = flags.isEnabled("beta-search", ctx("acme", "steady-user")); // config "25%"
    const b = flags.isEnabled("beta-search", ctx("acme", "steady-user"));
    assert.equal(a, b);
  });

  it("set-rule creates a brand-new flag at runtime", () => {
    assert.equal(flags.isEnabled("runtime-only", ctx("acme", "u1")), false); // unknown
    flags.setRule("runtime-only", "", ON); // global on
    assert.equal(flags.isEnabled("runtime-only", ctx("acme", "u1")), true);
  });

  it("set-rule accepts a runtime percentage", () => {
    flags.setRule("gradual", "", { tag: "percentage", val: 100 });
    assert.equal(flags.isEnabled("gradual", ctx("acme", "anyone")), true);
    flags.setRule("gradual", "", { tag: "percentage", val: 0 });
    assert.equal(flags.isEnabled("gradual", ctx("acme", "anyone")), false);
  });

  it("a tenant rule wins over the global rule", () => {
    flags.setRule("scoped", "", ON); // global on
    flags.setRule("scoped", "acme", OFF); // acme off
    assert.equal(flags.isEnabled("scoped", ctx("acme", "u1")), false, "acme overridden off");
    assert.equal(flags.isEnabled("scoped", ctx("other", "u1")), true, "other inherits global on");
  });

  it("a runtime rule wins over config, clear falls back", () => {
    flags.setRule("dark-mode", "", ON); // config-false -> force on
    assert.equal(flags.isEnabled("dark-mode", ctx("acme", "u1")), true);
    flags.clearRule("dark-mode", "");
    assert.equal(flags.isEnabled("dark-mode", ctx("acme", "u1")), false); // back to config
  });

  it("list-flags merges config + runtime rules with correct source", () => {
    flags.setRule("listed-global", "", ON);
    flags.setRule("listed-tenant", "acme", OFF);
    const states = flags.listFlags("acme");
    const byName = new Map(states.map((s) => [s.name, s]));

    // config-defined flags appear with source "config".
    assert.equal(byName.get("new-checkout")?.source, "config");
    // runtime global + tenant rules appear with their source.
    assert.equal(byName.get("listed-global")?.source, "global-override");
    assert.equal(byName.get("listed-tenant")?.source, "tenant-override");
    assert.deepEqual(byName.get("listed-tenant")?.rule, OFF);
  });
});
