#!/usr/bin/env bash
# A batch of ten tasks through the free queue, half of which need a tool nobody has written. The
# agent answers what it can, builds what it cannot (MoonBit, `tool_build`), calls it, and opens a
# real pull request with each tool's component (`propose_pr`, `components/<tool>-mbt`). Every answer
# is checked against one this script computes itself.
#   QUEUE        the queue's NATS (default nats://malna:4222), served by llm-queue with keys
#   WORKER=here  serve llm-queue from this machine on QUEUE too; LLM_KEYS is its config file of
#                `<provider>-api-key = ...` lines (passed as a file, never read here)
#   PR_BASE      the branch the PRs target (default main); PR_REPO the checkout to push from
#                (default: a fresh clone of this repository, pushing to its origin)
# Needs what live.sh needs, and `gh` logged in.
set -uo pipefail
S=$(cd "$(dirname "$0")" && pwd); H=$(cd "$S/../../.." && pwd); E=$(mktemp -d "${TMPDIR:-/tmp}/tool-forge-batch.XXXXXX")
mkdir -p "$E/node" "$E/rt"
QUEUE=${QUEUE:-nats://malna:4222}; PR_BASE=${PR_BASE:-main}
NATS=nats://127.0.0.1:14222; RT=http://127.0.0.1:18917; AUTH='authorization: Bearer scenario'
trap 'kill $(cat "$E/pids" 2>/dev/null) 2>/dev/null' EXIT

if [ -z "${PR_REPO:-}" ]; then
  PR_REPO=$E/repo
  git clone --quiet --no-local "$H" "$PR_REPO"
  git -C "$PR_REPO" remote set-url origin "$(git -C "$H" remote get-url origin)"
fi

nats-server -js -sd "$E/js" -a 127.0.0.1 -p 14222 > "$E/nats.log" 2>&1 & echo $! > "$E/pids"; sleep 1
"$H/target/release/holon-host" --lattice-nats $NATS --node n1 --lattice dev --addr 127.0.0.1:13911 \
  --state-dir "$E/node" > "$E/host.log" 2>&1 & echo $! >> "$E/pids"
if [ "${WORKER:-}" = here ]; then
  "$H/target/release/holon-host" --component "$H/components/target/wasm32-wasip2/release/llm_queue.wasm" \
    --addr 127.0.0.1:13912 --kv sqlite --nats-url "$QUEUE" --subscribe holon.llm.ask=/ask ${LLM_KEYS:+--config-file "$LLM_KEYS"} \
    --egress api.groq.com:443 --egress api.cerebras.ai:443 --egress generativelanguage.googleapis.com:443 \
    --egress openrouter.ai:443 --egress api.mistral.ai:443 > "$E/llm.log" 2>&1 & echo $! >> "$E/pids"
fi
sleep 2
echo scenario > "$E/rt/admin-token"
HOLON_NATS_URL=$NATS HOLON_FORGE_TEMPLATE="$H/components/text-stats-mbt" HOLON_FORGE_ARTIFACTS="$E/node/artifacts" \
HOLON_FORGE_LATTICE=dev HOLON_FORGE_NODE=n1 PATH="$HOME/.cargo/bin:$HOME/.moon/bin:$PATH" \
  "$H/target/release/agent-runtime" --state-dir "$E/rt" --listen 127.0.0.1:18917 --pr-repo "$PR_REPO" \
  > "$E/rt.log" 2>&1 & echo $! >> "$E/pids"; sleep 1

jq -n --arg q "$QUEUE" --arg base "$PR_BASE" '{spec:{name:"helper",
  description:("You answer questions exactly and briefly. Facts and language you know, answer directly. You cannot compute counts, dates or arithmetic over many steps reliably: for those, if no tool you have fits, write one with tool_build (MoonBit, a `call(args : Json) -> Result[Json, String]` in tool/tool.mbt, as the template shows), call it, and answer with its result. After a tool you built answered, propose it once with propose_pr: {\"branch\": \"tool/<name with - for _>\", \"base\": \"" + $base + "\", \"title\": \"feat(tools): <name> — a tool an agent built\", \"body\": \"<what it computes, and the question it answered>\", \"tool\": \"<name>\"}. End with the answer alone on the last line."),
  capabilities:[{name:"tool_build"},{name:"propose_pr"}],auto_approve:["tool_build","propose_pr"],max_steps:10,
  model:{kind:"batch_queue",queue_url:$q,model:"free"}}}' \
  | curl -s -o /dev/null -w "create agent: %{http_code}\n" -X PUT -H "$AUTH" -H 'content-type: application/json' $RT/v1/agents/helper --data-binary @-

days() { echo $(( ($(date -j -f %F "$2" +%s) - $(date -j -f %F "$1" +%s)) / 86400 )); }
primes() { awk -v n="$1" 'BEGIN{c=0; for(i=2;i<n;i++) s[i]=1; for(i=2;i*i<n;i++) if(s[i]) for(j=i*i;j<n;j+=i) s[j]=0; for(i=2;i<n;i++) c+=s[i]; print c}'; }
fib() { awk -v n="$1" -v m=1000000007 'BEGIN{a=0;b=1; for(i=0;i<n;i++){t=(a+b)%m;a=b;b=t} print a}'; }
collatz() { awk -v n="$1" 'BEGIN{s=0; while(n!=1){ n=(n%2)?3*n+1:n/2; s++ } print s}'; }
luhn() { awk -v p="$1" 'BEGIN{s=0; d=1; for(i=length(p);i>=1;i--){ x=substr(p,i,1)+0; if(d){x*=2; if(x>9)x-=9} s+=x; d=!d } print (10-s%10)%10}'; }

# kind | question | the answer, as this script computes it
cat > "$E/tasks" <<EOF
new|How many prime numbers are there below 200000?|$(primes 200000)
new|How many days are there from 2026-03-01 to 2026-11-06?|$(days 2026-03-01 2026-11-06)
new|What is the 10000th Fibonacci number modulo 1000000007 (F(0)=0, F(1)=1)?|$(fib 10000)
new|How many steps does the Collatz sequence of 837799 take to reach 1?|$(collatz 837799)
new|What is the Luhn check digit for the number 7992739871?|$(luhn 7992739871)
known|What is the capital of Hungary?|Budapest
known|Which planet is called the Red Planet?|Mars
known|What is the chemical symbol for gold?|Au
known|How do you say "good morning" in Hungarian?|reggelt
known|Who wrote the novel 1984?|Orwell
EOF

pass=0; n=0
while IFS='|' read -r kind q want; do
  n=$((n+1)); t0=$(date +%s)
  got=$(curl -s -m 600 -G --data-urlencode "q=$q" $RT/agents/helper/run)
  last=$(printf '%s\n' "$got" | awk 'NF{l=$0} END{print l}')
  if printf '%s' "$last" | grep -qiF -- "$want"; then ok=PASS; pass=$((pass+1)); else ok=FAIL; fi
  printf '%-4s %-5s %-4ss %s\n     want %s, got: %s\n' "$ok" "$kind" "$(( $(date +%s) - t0 ))" "$q" "$want" "$(printf '%s' "$last" | cut -c1-120)"
done < "$E/tasks"

echo; echo "answers: $pass/$n right"
echo "tools built: $(ls "$E/rt/forge" 2>/dev/null | sed 's/^helper-//' | tr '\n' ' ')"
echo "pull requests:"
curl -s -H "$AUTH" "$RT/v1/agents/helper/runs?limit=50" | jq -r '.runs[].steps[] | select(.kind=="tool" and .name=="propose_pr") |
  if .error then "  refused: \(.result | .[0:160])" else (.result | fromjson | "  \(.url)  \(.branch)  (\(.files | length) files)") end'
echo "(logs and state in $E)"
