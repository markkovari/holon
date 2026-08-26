#!/usr/bin/env bash
# The `bytes:codec` specification, run against the ARTIFACT.
#
# `tests/codec.rs` calls the Rust crate directly. That is a fine unit test and it
# cannot judge a component built in another language, or one fetched by digest and
# never built here at all. This drives the same cases through `codec-probe` over
# HTTP, so what is being judged is whatever satisfies the contract.
#
# Bytes cross as HEX in both directions: a base64 test whose transport is base64
# cannot tell a bug from a round trip.
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1

HOST="${COMP_HOST:-host/target/release/comp-host}"
PLUG="${COMP_PLUG:-reconciler/target/release/comp-plug}"
[ -x "$HOST" ] || { echo "no comp-host at '$HOST' — cargo build --release in host/"; exit 1; }
[ -x "$PLUG" ] || { echo "no comp-plug at '$PLUG' — cargo build --release in reconciler/"; exit 1; }

ART=$("$PLUG" codec-probe) || { echo "could not compose codec-probe"; exit 1; }
PORT="${CODEC_PORT:-3219}"
"$HOST" --app codecprobe --component "$ART" --addr "127.0.0.1:$PORT" >/tmp/codec-gate.log 2>&1 &
HOSTPID=$!
trap 'kill $HOSTPID 2>/dev/null' EXIT

for _ in $(seq 1 60); do
  curl -sf "http://127.0.0.1:$PORT/health" >/dev/null 2>&1 && break
  sleep 0.25
done

fails=0
# get <route> <query> ; prints the JSON body
get() { curl -sf "http://127.0.0.1:$PORT/$1?$2"; }
# expect <what> <expected> <actual>
#
# The expected JSON is written with keys in the order the component emits them —
# `serde_json` sorts them — because comparing strings is what makes this gate
# runnable from a shell against any implementation.
expect() {
  if [ "$2" = "$3" ]; then return 0; fi
  echo "  FAIL $1"
  echo "       expected $2"
  echo "       got      $3"
  fails=$((fails + 1))
}
# to hex, without assuming the component under test can do it
tohex() { printf '%s' "$1" | od -An -tx1 | tr -d ' \n'; }

echo "RFC 4648 vectors"
for pair in "f:Zg==" "fo:Zm8=" "foo:Zm9v" "foob:Zm9vYg==" "fooba:Zm9vYmE=" "foobar:Zm9vYmFy"; do
  raw="${pair%%:*}"; want="${pair##*:}"
  expect "encode $raw" "{\"text\":\"$want\"}" "$(get encode "bytes=$(tohex "$raw")&alphabet=standard")"
  expect "decode $want" "{\"bytes\":\"$(tohex "$raw")\"}" "$(get decode "text=$(printf '%s' "$want" | sed 's/=/%3D/g')&alphabet=standard")"
done

echo "the two alphabets are not interchangeable"
expect "encode fbff standard" '{"text":"+/8="}' "$(get encode 'bytes=fbff&alphabet=standard')"
expect "encode fbff url-safe" '{"text":"-_8"}'  "$(get encode 'bytes=fbff&alphabet=url-safe')"
expect "standard text refused by url-safe" '{"at":0,"error":"not-in-alphabet","found":"+"}' \
  "$(get decode 'text=%2B%2F8%3D&alphabet=url-safe')"
expect "url-safe text refused by standard" '{"at":0,"error":"not-in-alphabet","found":"-"}' \
  "$(get decode 'text=-_8&alphabet=standard')"

echo "padding"
expect "url-safe does not pad" '{"text":"Zg"}' "$(get encode "bytes=$(tohex f)&alphabet=url-safe")"
expect "padded url-safe decodes" "{\"bytes\":\"$(tohex f)\"}" "$(get decode 'text=Zg%3D%3D&alphabet=url-safe')"
expect "unpadded standard decodes" "{\"bytes\":\"$(tohex f)\"}" "$(get decode 'text=Zg&alphabet=standard')"
expect "padding in the middle" '{"at":2,"error":"misplaced-padding"}' "$(get decode 'text=Zg%3D%3DZg%3D%3D&alphabet=standard')"

echo "refusals"
expect "a character in neither" '{"at":4,"error":"not-in-alphabet","found":"!"}' \
  "$(get decode 'text=Zm9v%21mFy&alphabet=standard')"
expect "an orphan character" '{"error":"truncated-group","length":9}' \
  "$(get decode 'text=Zm9vYmFyZ&alphabet=standard')"

echo "hex"
expect "to-hex" '{"text":"deadbeef"}' "$(get to-hex 'bytes=deadbeef')"
expect "from-hex, either case" '{"bytes":"deadbeef"}' "$(get from-hex 'text=DeAdBeEf')"
expect "from-hex refuses" '{"at":4,"error":"not-in-alphabet","found":" "}' "$(get from-hex 'text=dead%20beef')"
expect "an odd number of digits" '{"error":"truncated-group","length":3}' "$(get from-hex 'text=abc')"

echo "every byte survives a round trip"
all=$(python3 -c "print(''.join(f'{i:02x}' for i in range(256)))")
for a in standard url-safe; do
  text=$(get encode "bytes=$all&alphabet=$a" | sed 's/.*"text":"//; s/"}//')
  esc=$(printf '%s' "$text" | sed 's/+/%2B/g; s|/|%2F|g; s/=/%3D/g')
  expect "round trip $a" "{\"bytes\":\"$all\"}" "$(get decode "text=$esc&alphabet=$a")"
done

if [ "$fails" -gt 0 ]; then
  echo
  echo "$fails case(s) failed against the artifact"
  exit 1
fi
echo
echo "the specification passes against the composed artifact"
