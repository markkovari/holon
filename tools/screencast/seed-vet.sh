#!/usr/bin/env bash
# Seed the vet-clinic native host: the 3 role→permission maps + demo users.
# The jco example does this in-process at boot (src/seed.ts); the native host
# doesn't, so we bootstrap over the (unguarded) /admin routes. No pets — the
# screencast adds one live.
set -euo pipefail
B="${VET_URL:-http://127.0.0.1:3007}"; T=acme-vet
perms() { curl -s -X POST "$B/admin/role-permissions" -d "$1" -o /dev/null -w "$2: %{http_code}\n"; }
perms "{\"tenant\":\"$T\",\"role\":\"pet-owner\",\"permissions\":[{\"target\":\"pets\",\"action\":\"read\"},{\"target\":\"pets\",\"action\":\"write\"},{\"target\":\"appointments\",\"action\":\"read\"},{\"target\":\"appointments\",\"action\":\"write\"}]}" pet-owner
perms "{\"tenant\":\"$T\",\"role\":\"doctor\",\"permissions\":[{\"target\":\"pets\",\"action\":\"read\"},{\"target\":\"appointments\",\"action\":\"read\"},{\"target\":\"appointments\",\"action\":\"write\"},{\"target\":\"notes\",\"action\":\"write\"}]}" doctor
perms "{\"tenant\":\"$T\",\"role\":\"admin\",\"permissions\":[{\"target\":\"*\",\"action\":\"*\"}]}" admin
for u in "owner@acme-vet.test:ownerpass1:pet-owner" "doctor@acme-vet.test:doctorpass1:doctor" "admin@acme-vet.test:adminpass1:admin"; do
  IFS=: read -r em pw ro <<< "$u"
  curl -s -X POST "$B/auth/register" -d "{\"email\":\"$em\",\"password\":\"$pw\",\"role\":\"$ro\"}" -o /dev/null -w "register $ro: %{http_code}\n"
done
