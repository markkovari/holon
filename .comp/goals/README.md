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

To run one, copy its `writable` and `checks` into a `.comp/goal.toml` and:

```bash
bash goal-demo.sh real         # against this very repo — Holon working on Holon
```
