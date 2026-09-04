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
| [01](01-fuel-is-money.md) | fuel is money | |
| [02](02-drive-the-queue.md) | drive the queue | |
| [03](03-diversity-beyond-seed.md) | diversity beyond seed | |
| [04](04-the-base-moved.md) | the base moved | |
| [05](05-become-holon.md) | become Holon | |
| [06](06-a-two-part-goal.md) | a two-part goal | ✅ done; its gate is now vacuous |
| [07](07-nothing-criticises-a-gate.md) | nothing criticises a gate | the one the loop is paused on |
| [08](08-a-branch-reads-what-the-swarm-learned.md) | a branch reads what the swarm learned | |
| [09](09-a-collection-that-prices-itself.md) | a collection that prices itself — a Pokémon TCG portfolio | 🟢 three agent-ready goals, 43 red tests |
| [10](10-a-decomposed-goal-with-a-target.md) | a decomposed goal with a target — the field-service dispatch API | 🟢 four red gates; every other decomposed goal is spent |
| [11](11-a-shop-that-scans-things.md) | a shop that scans things — `barcode:read` and a grocery app | ✅ done; capability, app, RBAC & GIF |

Each file carries its own tag and its `writable`/`check` block; the table is only
so the list is visible without an `ls`.

To run one, copy its `writable` and `checks` into a `.comp/goal.toml` and:

```bash
CHECKOUT=$PWD REPO=<owner>/<name> bash goal-demo.sh real   # Holon working on Holon
```
