#!/usr/bin/env bash
# Makes the homeserver reachable from your devices — and only your devices — by
# putting `tailscale serve` (HTTPS, a real certificate for the *.ts.net name) in
# front of the loopback port. Optionally turns the machine into an exit node.
#
#   ./tailscale.sh Malna serve        # HTTPS in front of Synapse, tailnet only
#   ./tailscale.sh Malna exit-node    # advertise the machine as an exit node
#   ./tailscale.sh Malna status
#
# Needs: HTTPS certificates enabled for the tailnet (admin console -> DNS), and
# passwordless sudo on the target for `serve`/`exit-node` (or run the printed
# commands there by hand). Funnel is deliberately never used: nothing here is
# exposed to the internet.
set -euo pipefail
TARGET=${1:?usage: tailscale.sh <ssh host> serve|exit-node|status}
ACTION=${2:?}
PORT=${SYNAPSE_PORT:-8008}
# Seamless access: if ~/.ssh/holon_<host> exists (a dedicated deploy key; see README),
# use it, unless SSH_OPTS says otherwise.
if [ -z "${SSH_OPTS:-}" ]; then
  _k="$HOME/.ssh/holon_$(printf %s "${TARGET:-}" | tr 'A-Z' 'a-z')"
  [ -f "$_k" ] && SSH_OPTS="-i $_k -o IdentitiesOnly=yes"
fi
# Commands are piped to `bash -s`: the remote login shell may not be bash (malna's is
# fish). SSH_OPTS is for things like a shared ControlPath; none are required.
on() { if [ "$TARGET" = local ]; then bash -c "$1"; else printf '%s\n' "$1" | ssh ${SSH_OPTS:-} -o BatchMode=yes "$TARGET" bash -s; fi; }

case "$ACTION" in
  serve)
    on "sudo -n tailscale serve --bg --https=443 http://127.0.0.1:$PORT"
    on "tailscale serve status"
    NAME=$(on "tailscale status --json | python3 -c 'import json,sys; print(json.load(sys.stdin)[\"Self\"][\"DNSName\"].rstrip(\".\"))'")
    echo "reachable (tailnet only) at: https://$NAME"
    echo "undo with: ssh $TARGET 'sudo tailscale serve reset'"
    ;;
  exit-node)
    # An exit node forwards IP packets, so the kernel must allow it.
    on "echo 'net.ipv4.ip_forward = 1' | sudo -n tee /etc/sysctl.d/99-tailscale.conf >/dev/null && echo 'net.ipv6.conf.all.forwarding = 1' | sudo -n tee -a /etc/sysctl.d/99-tailscale.conf >/dev/null && sudo -n sysctl -p /etc/sysctl.d/99-tailscale.conf"
    on "sudo -n tailscale set --advertise-exit-node"
    echo "advertised. APPROVE it once in the admin console (Machines -> $TARGET -> Edit route settings -> Use as exit node),"
    echo "then on a device:  tailscale set --exit-node=$TARGET   (and --exit-node= to stop)"
    echo "note: an exit node routes a device's INTERNET traffic through $TARGET. Reaching $TARGET itself never needs it."
    ;;
  status) on "tailscale serve status; tailscale debug prefs | grep -iE 'AdvertiseRoutes|ExitNode'";;
  *) echo "unknown action $ACTION" >&2; exit 2;;
esac
