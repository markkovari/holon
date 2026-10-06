#!/usr/bin/env bash
# The Matrix test: what does an agent look like from Element X?
#
#   ./spike.sh <base-url> <server-name> setup                # create you + a ghost agent (admin command, in the container)
#   ./spike.sh <base-url> <server-name> post                 # one of each: text, mention, file, poll
#   ./spike.sh <base-url> <server-name> loop [secs] [count]  # a text every N seconds, to test push (default 60s x 20)
#
# `setup` prints your password ONCE. Log in with Element X at <base-url> as
# @mark:<server-name>. The ghost agent `rower` DMs you; you accept the invite.
# The real bridge will do this with an application service (no passwords,
# no commands); this is only the quickest honest imitation of its output.
set -euo pipefail
BASE=${1:?base url}; SN=${2:?server name}; CMD=${3:?setup|post|loop}
ME=${ME_USER:-mark}; GHOST=${GHOST_USER:-rower}
CTR=${SYNAPSE_CONTAINER:-$(docker ps --format '{{.Names}}' 2>/dev/null | grep -m1 synapse || true)}
STATE=${SPIKE_STATE:-$(dirname "$0")/.spike}   # holds the ghost's password and room id (gitignored)
mkdir -p "$STATE"; chmod 700 "$STATE"

api() { curl -fsS "$@"; }
login() { # user password -> access token
  api -X POST "$BASE/_matrix/client/v3/login" -H 'content-type: application/json' \
    -d "$(jq -n --arg u "$1" --arg p "$2" '{type:"m.login.password",identifier:{type:"m.id.user",user:$u},password:$p}')" | jq -r .access_token
}
txn() { printf '%s%s' "$(date +%s)" "$RANDOM"; }
send() { # token room type json
  api -X PUT "$BASE/_matrix/client/v3/rooms/$2/send/$3/$(txn)" -H "authorization: Bearer $1" -H 'content-type: application/json' -d "$4" | jq -r .event_id
}

case "$CMD" in
  setup)
    [ -n "$CTR" ] || { echo "no synapse container found; set SYNAPSE_CONTAINER" >&2; exit 1; }
    ME_PW=$(openssl rand -base64 18 | tr -d '/+=' | head -c 20); GH_PW=$(openssl rand -base64 18 | tr -d '/+=' | head -c 20)
    docker exec "$CTR" register_new_matrix_user -c /data/homeserver.yaml --admin -u "$ME" -p "$ME_PW" http://localhost:8008 >/dev/null
    docker exec "$CTR" register_new_matrix_user -c /data/homeserver.yaml --no-admin -u "$GHOST" -p "$GH_PW" http://localhost:8008 >/dev/null
    umask 077; printf '%s' "$GH_PW" > "$STATE/ghost.pw"
    echo "created @$ME:$SN and @$GHOST:$SN"
    echo "YOUR PASSWORD (shown once): $ME_PW"
    ;;
  post|loop)
    TOKEN=$(login "$GHOST" "$(cat "$STATE/ghost.pw")")
    if [ ! -f "$STATE/room" ]; then
      api -X POST "$BASE/_matrix/client/v3/createRoom" -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' \
        -d "$(jq -n --arg me "@$ME:$SN" '{invite:[$me],is_direct:true,preset:"trusted_private_chat",name:"rower (test agent)"}')" | jq -r .room_id > "$STATE/room"
      echo "created the room and invited @$ME:$SN: accept the invite in Element X"
    fi
    ROOM=$(cat "$STATE/room")
    if [ "$CMD" = loop ]; then
      EVERY=${4:-60}; COUNT=${5:-20}
      for i in $(seq 1 "$COUNT"); do
        send "$TOKEN" "$ROOM" m.room.message "$(jq -n --arg b "heartbeat $i/$COUNT at $(date +%H:%M:%S)" '{msgtype:"m.text",body:$b}')" >/dev/null
        echo "sent $i/$COUNT"; [ "$i" -lt "$COUNT" ] && sleep "$EVERY"
      done
      exit 0
    fi
    echo "text:    $(send "$TOKEN" "$ROOM" m.room.message '{"msgtype":"m.text","body":"Latest row: 10/05/26 1:00:00 12,108m 2:28.6"}')"
    echo "mention: $(send "$TOKEN" "$ROOM" m.room.message "$(jq -n --arg me "@$ME:$SN" '{msgtype:"m.text",body:"@mark this one mentions you",format:"org.matrix.custom.html",formatted_body:"<a href=\"https://matrix.to/#/\($me)\">mark</a> this one mentions you","m.mentions":{user_ids:[$me]}}')")"
    printf 'weekly report\ndate,metres\n10/05,12108\n10/04,7321\n' > "$STATE/report.csv"
    URI=$(api -X POST "$BASE/_matrix/media/v3/upload?filename=weekly.csv" -H "authorization: Bearer $TOKEN" -H 'content-type: text/csv' --data-binary @"$STATE/report.csv" | jq -r .content_uri)
    echo "file:    $(send "$TOKEN" "$ROOM" m.room.message "$(jq -n --arg u "$URI" '{msgtype:"m.file",body:"weekly.csv",filename:"weekly.csv",url:$u,info:{mimetype:"text/csv",size:60}}')")"
    echo "poll:    $(send "$TOKEN" "$ROOM" org.matrix.msc3381.poll.start '{"org.matrix.msc1767.text":"rower wants to run write_file. Approve?\n1. Approve\n2. Deny","org.matrix.msc3381.poll.start":{"kind":"org.matrix.msc3381.poll.disclosed","max_selections":1,"question":{"org.matrix.msc1767.text":"rower wants to run write_file. Approve?"},"answers":[{"id":"approve","org.matrix.msc1767.text":"Approve"},{"id":"deny","org.matrix.msc1767.text":"Deny"}]}}')"
    ;;
  *) echo "unknown command $CMD" >&2; exit 2;;
esac
