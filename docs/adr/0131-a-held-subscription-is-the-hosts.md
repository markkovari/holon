# ADR-0131 — A held subscription is the host's

*A component cannot hold a NATS subscription, so anything that answers messages was native. The
host can hold it and hand each message to the component as a request.*

**Status: accepted.** The message half of [ADR-0130](0130-the-host-is-the-clock.md).

## The problem

ADR-0095 lets a thing be native for "a held subscription". That reason made daemons of things whose
work is otherwise a component's: `holon-llmbatch` answers LLM requests over NATS, and its logic
(provider failover, limits, cooldowns) is pure. Messaging stays off for components on purpose
(`host/src/agent.rs`: a raw subject publish bypasses the host's naming boundary), so the component
cannot subscribe itself either.

## The decision

- **`--subscribe <subject>=<path>`, repeatable.** The host subscribes at `--nats-url`, in a queue
  group named for the subject so replicas share the work, and `POST`s each message's payload to the
  path on its own listener, as `--pump` does. When the message is a request, the response body is
  the reply, whatever its status; when no response came at all, the reply is empty with a
  `Holon-Error` header, so a requester fails at once instead of timing out.
- **The routes are the operator's, never the app's.** They are host flags (self-hosting renders
  them from the app spec, which the operator writes), so a component hears only what it was given
  and still publishes nothing. At most 16 messages are in the app at once.

## What is not built yet

As with the pump: a lattice manifest cannot carry subscriptions, so `--subscribe` refuses to start
beside `--lattice-nats`. When it can, the platform stamps them into the start command the way it
stamps egress (ADR-0008), never the tenant.

## Consequences

- A daemon whose only native need was a subscription becomes a component behind one flag:
  `holon-llmbatch` first.
- Delivery is core NATS: at most once. Anything that needs at-least-once keeps its JetStream
  consumer native until the host can hold one.
