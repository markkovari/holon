# The worklist

Holon works a goal the way this repository was built: a person writes what is
wanted and the checks that decide it, and the machine explores in parallel and
lands the winner as a pull request. **A person writes these files. Nothing here
runs until a person starts it** (ADR-0082).

Each goal below carries a readiness tag, because not everything is the same shape
of work:

| tag | meaning |
|---|---|
| 🟢 **agent-ready** | one writable surface, a deterministic gate that already passes-or-fails; the loop can run it as it stands |
| 🟡 **needs a gate** | the change is scoped but the check that would prove it does not exist yet — write the failing test first, then it is agent-ready |
| 🔴 **human-led** | an architectural change that spans files and decisions; the agent is the wrong tool, but the goal is still the goal |

The honest state of the engine is in [`docs/CURRENT.md`](../../docs/CURRENT.md)
and [`docs/SCENARIOS.md`](../../docs/SCENARIOS.md); every goal here traces to a
line in the "honestly missing" column of one of them.

## The goals

| # | goal | readiness |
|---|---|---|
| [01](01-fuel-is-money.md) | fuel is money | 🟡 half built — `cost_cents` and `spent_cents` exist; no `max-cents` bound yet |
| [02](02-drive-the-queue.md) | drive the queue | ✅ built — `comp-goald` |
| [03](03-diversity-beyond-seed.md) | diversity beyond seed | 🔴 human-led |
| [04](04-the-base-moved.md) | the base moved | 🟡 needs a gate |
| [05](05-become-holon.md) | become Holon | 🔴 human-led — the CLI is `holon`; the rest stops at boundaries, on purpose |
| [06](06-a-two-part-goal.md) | a two-part goal | ✅ done; its gate is now vacuous |
| [07](07-nothing-criticises-a-gate.md) | nothing criticises a gate | ✅ built — `compose::criticise` |
| [08](08-a-branch-reads-what-the-swarm-learned.md) | a branch reads what the swarm learned | ✅ done |
| [09](09-a-collection-that-prices-itself.md) | a collection that prices itself — a Pokémon TCG portfolio | ✅ all three landed, written by the loop |
| [10](10-a-decomposed-goal-with-a-target.md) | a decomposed goal with a target — the field-service dispatch API | ✅ done (#220); its gates are green on `main` |
| [11](11-a-shop-that-scans-things.md) | a shop that scans things — `barcode:read` and a grocery app | ✅ done; capability, app, RBAC & GIF |

Each file carries its own tag and its `writable`/`check` block; the table is only
so the list is visible without an `ls`.

To run one, copy its `writable` and `checks` into a `.comp/goal.toml` and:

```bash
CHECKOUT=$PWD REPO=<owner>/<name> bash goal-demo.sh real   # Holon working on Holon
```

## The goal specs

The `.toml` files are the specs a run reads — copied into `.comp/goal.toml`, or
picked up by `comp-goald` from the queue. A spec whose work has merged is renamed
`X.toml.archived`: kept as the worked example of how it was specified, and out of
the way of anything that lists what is left to run. To rerun one, stub its
implementation first — otherwise goal 07's base pre-check refuses it, correctly.

**Not archived:** `bytes-codec`, `case-conv`, `cicd-orchestrator`,
`clinic-telehealth`, `crypto-exchange`, `deck-build`, `ecommerce-fulfillment`,
`events-crud`, `freight-tracker`, `hr-payroll`, `interactive-elearning`,
`iot-smarthome`, `luhn-checksum`, `real-estate-auction`, `social-feed`,
`treasury-ledger`.

**Archived, merged:**

| spec | landed in |
|---|---|
| `clinic-goal` | archived in #86 |
| `dispatch` | goal 10, #220 |
| `surreal-kv` | 69051b7 |
| `card-identify`, `price-history`, `portfolio-value` | goal 09 |
| `triage`, `triage-assist`, `moderation-queue`, `support-desk`, `invoice-copilot`, `doc-search-agent` | the decomposed goals goal 10 lists as spent |
| `book-lending`, `complaint-escalation`, `flag-console`, `incident-response`, `invoice-approval`, `oncall-schedule`, `referral-tracker`, `room-booking`, `ticket-triage` | #254 |
| `volunteer-shift` | #255 |
| `expense-report`, `parking-domain`, `parking-lot-visualization`, `survey-domain`, `goalexit-codes` | archived in #271 |
| `goalbatch` | #275 |
| `inspection-domain` | #279 |
