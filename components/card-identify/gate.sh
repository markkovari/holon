#!/usr/bin/env bash
# The gate for `card-identify`: the whole held-out specification.
#
# Delegates to `components/spec-gate.sh` — one implementation of the log slicing, for
# the reason today's manifest bug demonstrated: two copies of anything drift, and the
# copy that drifts is the one nobody reruns.
exec bash "$(dirname "$0")/../spec-gate.sh" card-identify guess
