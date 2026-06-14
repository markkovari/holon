#!/usr/bin/env bash
# Seed an OIDC IdP (zitadel or ory) for the auth-guard `oidc` path, and print
# the keyvalue config to load into the running auth-guard.
#
# Usage:  ./seed-idp.sh zitadel   |   ./seed-idp.sh ory
#
# This brings up the chosen compose profile, waits for the issuer to be healthy,
# and prints (a) the OIDC issuer URL and (b) the `nats kv put` commands to seed
# `oidc:issuer` / `oidc:client-id` / `oidc:client-secret` into auth-guard's
# bucket. Client registration differs per IdP — the manual step is spelled out
# because both IdPs require an authenticated admin call that depends on local
# credentials we will not bake into a script.
set -euo pipefail

IDP="${1:-}"
COMPOSE="docker compose -f $(dirname "$0")/../compose.yaml"

case "$IDP" in
  zitadel)
    ISSUER="http://localhost:8080"
    $COMPOSE --profile zitadel up -d
    echo "waiting for Zitadel issuer at $ISSUER ..."
    for _ in $(seq 1 60); do
      curl -sf "$ISSUER/.well-known/openid-configuration" >/dev/null 2>&1 && break
      sleep 2
    done
    cat <<EOF

Zitadel up. Issuer: $ISSUER

Register an OIDC app:
  1. open $ISSUER (login with the bootstrap admin from compose env)
  2. create a Project -> Application (type: Web / API), note Client ID + Secret
  3. set redirect URI to your app's callback

Then seed auth-guard config (NATS kv bucket "comp-auth"):
  nats kv put comp-auth oidc:issuer        "$ISSUER"
  nats kv put comp-auth oidc:client-id     "<client-id>"
  nats kv put comp-auth oidc:client-secret "<client-secret>"
EOF
    ;;
  ory)
    ISSUER="http://localhost:4444"
    ADMIN="http://localhost:4445"
    $COMPOSE --profile ory up -d
    echo "waiting for Ory Hydra issuer at $ISSUER ..."
    for _ in $(seq 1 60); do
      curl -sf "$ISSUER/.well-known/openid-configuration" >/dev/null 2>&1 && break
      sleep 2
    done
    echo "registering an OAuth2 client via Hydra admin ($ADMIN) ..."
    RESP=$(curl -sf -X POST "$ADMIN/admin/clients" \
      -H 'content-type: application/json' \
      -d '{"grant_types":["authorization_code","refresh_token"],"response_types":["code"],"scope":"openid offline","redirect_uris":["http://localhost:3000/callback"],"token_endpoint_auth_method":"client_secret_post"}' \
      || true)
    CID=$(printf '%s' "$RESP"  | sed -E 's/.*"client_id":"([^"]+)".*/\1/')
    CSEC=$(printf '%s' "$RESP" | sed -E 's/.*"client_secret":"([^"]+)".*/\1/')
    cat <<EOF

Ory Hydra up. Issuer: $ISSUER
Registered client: id=$CID secret=$CSEC

Seed auth-guard config (NATS kv bucket "comp-auth"):
  nats kv put comp-auth oidc:issuer        "$ISSUER"
  nats kv put comp-auth oidc:client-id     "$CID"
  nats kv put comp-auth oidc:client-secret "$CSEC"
EOF
    ;;
  *)
    echo "usage: $0 zitadel|ory" >&2
    exit 2
    ;;
esac
