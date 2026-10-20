#!/usr/bin/env bash
# Sourced by the cross-machine benches: `ssh -f -n` hides an unreachable box, and a
# smaller fleet still prints a plausible number (ADR-0057).

# An empty key means the ssh agent/config: the load box is a host alias.
need_remote() {
  local host=$1 key=$2 label=${3:-remote}
  local ident=()
  [ -n "$key" ] && ident=(-i "$key" -o IdentitiesOnly=yes)
  if ! ssh -n "${ident[@]}" -o BatchMode=yes -o ConnectTimeout=8 "$host" true 2>/dev/null; then
    echo "preflight: cannot ssh to $label ($host)." >&2
    echo "  Is the box powered on and on this network?${key:+ Is $key the right key?}" >&2
    echo "  This benchmark measures a fleet spanning two machines; running it with" >&2
    echo "  one would print a number for a fleet that is not the one described." >&2
    exit 1
  fi
}

# The remote dials back to this machine for NATS, so a stale MAC= leaves the fleet a machine short.
need_local_addr() {
  local addr=$1
  if ! ifconfig 2>/dev/null | grep -q "inet $addr " && \
     ! ip -4 addr 2>/dev/null | grep -q "inet $addr/"; then
    echo "preflight: $addr is not an address on this machine." >&2
    echo "  The remote node is told to reach NATS at that address, so it would" >&2
    echo "  start, join nothing, and leave the fleet a machine short." >&2
    echo "  Pass the right one: MAC=<this box's LAN address> $0" >&2
    exit 1
  fi
}

need_cmd() {
  for c in "$@"; do
    command -v "$c" >/dev/null 2>&1 || { echo "preflight: $c is not on PATH." >&2; exit 1; }
  done
}
