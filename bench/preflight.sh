#!/usr/bin/env bash
# Preconditions for the cross-machine benchmarks. Sourced, not run.
#
# The scripts that need another box all start it with `ssh -f -n`, which backgrounds
# immediately and reports almost nothing. Under `set -uo pipefail` (no `-e`) an
# unreachable Pi therefore does not stop anything: the run continues, the fleet is
# quietly smaller than the one being described, and 90 seconds later a number comes
# out that looks exactly like a good one.
#
# That is the failure ADR-0057 is about — a plausible number measuring something
# other than what its label says. These checks make it loud and immediate instead.

# Is the remote up, and does the key work? Both, because "the Pi is on" and "this
# machine can log into it" fail separately and are fixed differently.
# An empty key means "use the agent / ssh config", which is how the load box is
# reached — it is a host alias, not an address with a dedicated key.
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

# The remote dials back to THIS machine for NATS, so a hardcoded MAC= that no longer
# names an address on this box means the remote node joins nothing — and the fleet is
# silently short a machine again, this time with ssh succeeding.
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

# A binary the script shells out to. Cheaper to say so now than to discover it in
# the middle of a timed run.
need_cmd() {
  for c in "$@"; do
    command -v "$c" >/dev/null 2>&1 || { echo "preflight: $c is not on PATH." >&2; exit 1; }
  done
}
