#!/bin/sh
# An agent with only `tool_build` is given a goal it has no tool for; it writes one, the runtime
# builds/starts/wires it, and the agent calls it — then a second conversation reuses it.
# The agent's words are scripted (mock model); the build, start, wiring and calls are real.
# Needs: cargo build --release -p holon-host -p agent-runtime, nats-server, nats, jq, moon, wasm-tools.
S=$(cd "$(dirname "$0")" && pwd); H=$(cd "$S/../../.." && pwd); E=$(mktemp -d -t tool-forge); mkdir -p $E/node $E/rt
# The first attempt the agent makes: an Int has no `to_json`, so the compiler says no.
sed 's|Json::number((b - a).to_double())|(b - a).to_json()|' $S/days_between.mbt > $E/days_bad.mbt
NATS=nats://127.0.0.1:14222; RT=http://127.0.0.1:18917; AUTH='authorization: Bearer scenario'
nats-server -js -sd $E/js -a 127.0.0.1 -p 14222 > $E/nats.log 2>&1 & echo $! > $E/pids; sleep 1
$H/target/release/holon-host --lattice-nats $NATS --node n1 --lattice dev --addr 127.0.0.1:13911 --state-dir $E/node > $E/host.log 2>&1 & echo $! >> $E/pids; sleep 2
echo scenario > $E/rt/admin-token
HOLON_NATS_URL=$NATS HOLON_FORGE_TEMPLATE=$H/components/text-stats-mbt HOLON_FORGE_ARTIFACTS=$E/node/artifacts \
HOLON_FORGE_LATTICE=dev HOLON_FORGE_NODE=n1 PATH=$HOME/.cargo/bin:$PATH \
  $H/target/release/agent-runtime --state-dir $E/rt --listen 127.0.0.1:18917 > $E/rt.log 2>&1 & echo $! >> $E/pids; sleep 1

build() { jq -n --rawfile src "$1" '{tool:"tool_build",args:{name:"days_between",description:"Days between two dates, and their weekdays.",args:"{\"from\": \"YYYY-MM-DD\", \"to\": \"YYYY-MM-DD\"}",required:["from","to"],source:$src}}|tostring'; }
CALL='{"tool":"days_between","args":{"from":"2026-03-01","to":"2026-11-06"}}'
jq -n --argjson bad "$(build $E/days_bad.mbt)" --argjson good "$(build $S/days_between.mbt)" --arg call "$CALL" \
  '{spec:{name:"helper",description:"You answer questions; when you lack a tool for one, you build it.",
    capabilities:[{name:"tool_build"}],auto_approve:["tool_build"],
    model:{kind:"mock",replies:[$bad,$good,$call,"From 2026-03-01 to 2026-11-06 is 250 days, and 2026-11-06 is a Friday."]}}}' \
  | curl -s -o /dev/null -w "create agent: %{http_code}\n" -X PUT -H "$AUTH" -H 'content-type: application/json' $RT/v1/agents/helper --data-binary @-

GOAL='How many days are there from 2026-03-01 to 2026-11-06, and what weekday is 2026-11-06?'
echo "\n> $GOAL"; echo "< $(curl -s -G --data-urlencode "q=$GOAL" $RT/agents/helper/run)"
echo "\n-- what happened --"
curl -s -H "$AUTH" $RT/v1/agents/helper/runs | jq -r '.runs[0].steps[] | select(.kind=="tool") |
  "\(.name)(\(.args.name // (.args|tostring))) -> \(if .error then "ERROR " else "" end)\(.result | split("\n")[0:6] | join(" | "))"'
echo "\n-- wired in --"
echo "granted: $(curl -s -H "$AUTH" $RT/v1/agents/helper | jq -c '[.spec.capabilities[]?.name // .capabilities[]?.name]')"
echo "connector: $(jq -c .wrpc $E/rt/connectors/days_between.json)"
grep -E "serves" $E/host.log

# A new conversation: the tool is already there.
curl -s -H "$AUTH" $RT/v1/agents/helper | jq --arg c '{"tool":"days_between","args":{"from":"2026-11-06","to":"2026-12-24"}}' \
  '{spec:((.spec // .) | .model.replies += [$c,"48 days to Christmas Eve, a Thursday."])}' \
  | curl -s -o /dev/null -X PUT -H "$AUTH" -H 'content-type: application/json' $RT/v1/agents/helper --data-binary @-
echo "\n> How long until Christmas Eve?"; echo "< $(curl -s -G --data-urlencode "q=How long until Christmas Eve?" $RT/agents/helper/run)"
curl -s -H "$AUTH" $RT/v1/agents/helper/runs | jq -r '.runs[0].steps[] | select(.kind=="tool") | "\(.name)(\(.args|tostring)) -> \(.result)"'
kill $(cat $E/pids) 2>/dev/null
echo "\n(logs and state in $E)"
