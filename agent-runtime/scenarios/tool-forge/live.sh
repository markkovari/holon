#!/bin/sh
# run.sh without a script: every turn of the agent goes through the free queue (`holon.llm.ask`,
# ADR-0126), and what it builds, calls and answers is the model's own.
#   QUEUE        the queue's NATS (default nats://malna:4222)
#   WORKER=here  also serve the `llm-queue` component from this machine on QUEUE, for while no host
#                serves it there. It fails over between the free providers (its providers.json);
#                LLM_KEYS is a config file of `<provider>-api-key=...` lines for it.
# Needs what run.sh needs, plus (WORKER=here) the component: cargo xtask build.
S=$(cd "$(dirname "$0")" && pwd); H=$(cd "$S/../../.." && pwd); E=$(mktemp -d -t tool-forge-live); mkdir -p $E/node $E/rt
GOAL=${1:-How many days are there from 2026-03-01 to 2026-11-06, and what weekday is 2026-11-06?}
QUEUE=${QUEUE:-nats://malna:4222}
NATS=nats://127.0.0.1:14222; RT=http://127.0.0.1:18917; AUTH='authorization: Bearer scenario'
nats-server -js -sd $E/js -a 127.0.0.1 -p 14222 > $E/nats.log 2>&1 & echo $! > $E/pids; sleep 1
$H/target/release/holon-host --lattice-nats $NATS --node n1 --lattice dev --addr 127.0.0.1:13911 --state-dir $E/node > $E/host.log 2>&1 & echo $! >> $E/pids
if [ "${WORKER:-}" = here ]; then
  $H/target/release/holon-host --component $H/components/target/wasm32-wasip2/release/llm_queue.wasm \
    --addr 127.0.0.1:13912 --nats-url $QUEUE --subscribe holon.llm.ask=/ask ${LLM_KEYS:+--config-file $LLM_KEYS} \
    --egress api.groq.com:443 --egress api.cerebras.ai:443 --egress generativelanguage.googleapis.com:443 \
    --egress openrouter.ai:443 --egress api.mistral.ai:443 > $E/llm.log 2>&1 & echo $! >> $E/pids
fi
sleep 2
echo scenario > $E/rt/admin-token
HOLON_NATS_URL=$NATS HOLON_FORGE_TEMPLATE=$H/components/text-stats-mbt HOLON_FORGE_ARTIFACTS=$E/node/artifacts \
HOLON_FORGE_LATTICE=dev HOLON_FORGE_NODE=n1 PATH=$HOME/.cargo/bin:$PATH \
  $H/target/release/agent-runtime --state-dir $E/rt --listen 127.0.0.1:18917 > $E/rt.log 2>&1 & echo $! >> $E/pids; sleep 1

jq -n --arg q "$QUEUE" '{spec:{name:"helper",
  description:"You answer questions exactly. You cannot compute dates, counts or arithmetic reliably yourself: when a question needs a calculation and you have no tool for it, build one with tool_build (MoonBit), call it, and answer from its result. Act, do not announce: a reply that is not a tool call ends your task, so never write what you are about to do; send the tool call JSON itself, alone. When tool_build returns a compile error, read it, fix the source and call tool_build again with the whole corrected source.",
  capabilities:[{name:"tool_build"}],auto_approve:["tool_build"],max_steps:8,
  model:{kind:"batch_queue",queue_url:$q,model:"free"}}}' \
  | curl -s -o /dev/null -w "create agent: %{http_code}\n" -X PUT -H "$AUTH" -H 'content-type: application/json' $RT/v1/agents/helper --data-binary @-

echo "\n> $GOAL"; echo "< $(curl -s -G --data-urlencode "q=$GOAL" $RT/agents/helper/run)"
echo "\n-- what happened --"
curl -s -H "$AUTH" $RT/v1/agents/helper/runs | jq -r '.runs[0].steps[] |
  if .kind=="tool" then "TOOL \(.name)(\(.args.name // (.args|tostring))) -> \(if .error then "ERROR " else "" end)\(.result | split("\n")[0:8] | join("\n    "))"
  else "MODEL \(.text | .[0:600])" end'
echo "\ngranted: $(curl -s -H "$AUTH" $RT/v1/agents/helper | jq -c '[(.spec // .).capabilities[].name]')"
ls $E/rt/forge/*/tool/tool.mbt 2>/dev/null | while read f; do echo "\n-- the agent's $(basename $(dirname $(dirname $f))) --"; cat "$f"; done
kill $(cat $E/pids) 2>/dev/null
echo "\n(logs and state in $E)"
