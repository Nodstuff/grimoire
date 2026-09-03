#!/usr/bin/env bash
# Hub slice 2 smoke: three scratch daemons (hub 7570, alice 7571, bob 7572).
#   forward-propose → accept → relay · hub-owned proposal → admin resolve ·
#   transfer offer → accept → ownership flipped both sides → relay without
#   origin_owner · transfer refused (Busy) while a doc is live.
# Scratch dirs under a tempdir; never touches ~/.grimoire or port 7425.
# Usage: cd <repo> && cargo build --release -p grimoire && scripts/smoke-hub-slice2.sh
#   SMOKE_KEEP=1  keep the daemons running on failure (logs under the tempdir)
#   SMOKE_TRACE=1 shell trace
set -euo pipefail
[ -n "${SMOKE_TRACE:-}" ] && set -x

REPO="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$REPO/target/release/grimoire"
export GRIMOIRE_UI_DIST="$REPO/ui/dist"
ROOT="$(mktemp -d /tmp/grimoire-hub-smoke.XXXXXX)"
HUB_PORT=7570; ALICE_PORT=7571; BOB_PORT=7572
PIDS=()
cleanup() { for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done; }
trap cleanup EXIT

step() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
ok()   { printf '   ✓ %s\n' "$*"; }
fail() {
  printf '   ✗ %s\n' "$*"
  if [ -n "${SMOKE_KEEP:-}" ]; then echo "daemons kept running (SMOKE_KEEP); logs under $ROOT"; trap - EXIT; fi
  exit 1
}
# every "cmd && ok" below must not be a silent skip: a failed check is a failure
check() { local msg=$1; shift; if "$@" >/dev/null 2>&1; then ok "$msg"; else fail "$msg"; fi; }
jqe() { jq -e "$@" >/dev/null; }

start() { # name port [extra serve args]
  local name=$1 port=$2; shift 2
  local dir="$ROOT/$name"; mkdir -p "$dir"
  GRIMOIRE_IDENTITY_FILE="$dir/identity.key" \
    "$BIN" --db "$dir/ks.db" --port "$port" serve "$@" >"$dir/log" 2>&1 &
  PIDS+=($!)
  for _ in $(seq 1 100); do
    curl -sf "http://127.0.0.1:$port/api/buildinfo" >/dev/null 2>&1 && [ -f "$dir/admin.token" ] && return
    sleep 0.2
  done
  echo "--- $name log"; tail -20 "$dir/log"; fail "$name did not come up on $port"
}
tok() { cat "$ROOT/$1/admin.token"; }
# api NAME PORT METHOD PATH [json]
api() {
  local name=$1 port=$2 method=$3 path=$4 body=${5:-}
  if [ -n "$body" ]; then
    curl -sf -X "$method" "http://127.0.0.1:$port$path" -H "X-Grimoire-Admin: $(tok "$name")" \
      -H 'Content-Type: application/json' -d "$body"
  else
    curl -sf -X "$method" "http://127.0.0.1:$port$path" -H "X-Grimoire-Admin: $(tok "$name")"
  fi
}
hub()   { api hub   $HUB_PORT   "$@"; }
alice() { api alice $ALICE_PORT "$@"; }
bob()   { api bob   $BOB_PORT   "$@"; }
insert_op() { # text → one paragraph insert op
  jq -cn --arg t "$1" --arg id "$(uuidgen | tr A-Z a-z)" \
    '{kind:{op:"insert",block_id:$id,parent_id:null,order_key:"zz",block_type:"paragraph",content:$t,refers_to:null},source_refs:[]}'
}
# wait_for "what" 'shell expression' — evaluated here every 200ms for 30s; must print non-empty
wait_for() {
  local what=$1 expr=$2 out
  for _ in $(seq 1 150); do
    if out="$(eval "$expr" 2>/dev/null)" && [ -n "$out" ] && [ "$out" != null ] && [ "$out" != false ]; then echo "$out"; return; fi
    sleep 0.2
  done
  fail "timed out waiting for: $what"
}
# helpers used inside wait_for expressions
doc_has()            { api "$1" "$2" GET "/api/doc/$3" | jq -r --arg t "$4" '[.roots[].block.content] | index($t) // empty'; }
hub_transfer_state() { hub GET /admin/hub/transfers | jq -r --arg id "$1" '.[] | select(.id==$id) | .state'; }

step "start hub (Team), alice, bob"
start hub   $HUB_PORT --hub --name Team
start alice $ALICE_PORT
start bob   $BOB_PORT
# the install default name is the OS user for all three; contacts are named by these
alice POST /api/profile '{"name":"alice"}' >/dev/null
bob   POST /api/profile '{"name":"bob"}'   >/dev/null
ok "scratch dirs under $ROOT"

step "alice joins (first contact → admin), bob joins (pending) and is approved by alice"
LINK=$(hub POST /admin/hub/invite | jq -r .link)
alice POST /admin/join "$(jq -cn --arg l "$LINK" '{link:$l}')" | jqe '.joined.membership=="active"' && ok "alice active (admin)"
LINK=$(hub POST /admin/hub/invite | jq -r .link)
bob POST /admin/join "$(jq -cn --arg l "$LINK" '{link:$l}')" | jqe '.joined.membership=="pending"' && ok "bob pending"
HUB_ON_ALICE=$(alice GET /admin/hubs | jq -r '.[0].contact_id')
HUB_ON_BOB=$(bob GET /admin/hubs | jq -r '.[0].contact_id')
BOB_KEY=$(bob GET /api/profile | jq -r .node_id)
BOB_ON_HUB=$(alice GET "/admin/hubs/members?hub=$HUB_ON_ALICE" | jq -r --arg k "$BOB_KEY" '.members[] | select(.pubkey==$k) | .contact_id')
alice POST /admin/hubs/approve "$(jq -cn --arg h "$HUB_ON_ALICE" --arg c "$BOB_ON_HUB" '{hub:$h,contact_id:$c}')" | jqe .ok
OFFER=$(wait_for "bob receives the hub's offer" 'bob GET /admin/offers | jq -r ".[0].id // empty"')
bob POST /admin/offers/accept "$(jq -cn --arg id "$OFFER" '{id:$id}')" | jqe '.joined.membership=="active"' && ok "bob accepted the Team folder"
TEAM_ROOT=$(bob GET /admin/hubs | jq -r '.[0].root_doc_id')
[ -n "$TEAM_ROOT" ] && [ "$TEAM_ROOT" != null ] || fail "bob holds no Team root"

step "alice writes Notes and publishes it; bob receives it relayed (owned by alice)"
NOTES=$(alice POST /api/docs '{"title":"Notes"}' | jq -r .id)
alice POST /api/propose "$(jq -cn --arg d "$NOTES" --argjson op "$(insert_op "alice notes")" '{doc_id:$d,base_epoch:0,ops:[$op]}')" >/dev/null
alice POST /admin/shares/offer "$(jq -cn --arg d "$NOTES" --arg c "$HUB_ON_ALICE" '{root_doc:$d,permission:"propose",contact_id:$c}')" | jqe .delivered
wait_for "hub relays Notes" 'hub GET /admin/hub/members | jq -r "[.[].publications[]] | length" | rg -v "^0$"' >/dev/null
wait_for "bob holds Notes, owned by alice" 'bob POST /admin/pull >/dev/null; bob GET /api/doc/$NOTES/federation | jq -r "select(.mirror.origin_owner_name==\"alice\") | \"yes\""' >/dev/null
ok "bob's copy says owned by alice"

step "1. bob proposes on the relayed doc → forwarded through the hub → alice's queue, as bob"
bob POST /admin/propose_upstream "$(jq -cn --arg d "$NOTES" --argjson op "$(insert_op "bob edit")" '{doc_id:$d,ops:[$op],note:"typo"}')" | jqe '.state=="pending"'
Q=$(alice GET /api/queue)
check "alice's queue: proposer = bob (not the hub)"   jqe '.[0].proposer=="bob"' <<<"$Q"
check "source_refs carry 'via hub: Team'"             jqe '.[0].item.op.source_refs | index("via hub: Team")' <<<"$Q"
check "hub parked nothing itself"                     jqe 'length==0' <<<"$(hub GET /api/queue)"
ANN=$(jq -r '.[0].item.annotation.id' <<<"$Q")
alice POST /api/resolve "$(jq -cn --arg a "$ANN" '{annotation_id:$a,decision:"accept"}')" >/dev/null
hub POST /admin/pull >/dev/null
wait_for "bob's relay shows the accepted edit" 'bob POST /admin/pull >/dev/null; doc_has bob $BOB_PORT $NOTES "bob edit"' >/dev/null
ok "bob sees his edit in the relayed doc"
wait_for "bob's outbound proposal resolves accepted (status asked via the hub)" 'bob GET /admin/proposals | jq -r ".[] | select(.state==\"accepted\") | .id" | head -1' >/dev/null
ok "bob: proposal accepted"

step "3. bob proposes on the hub-owned Team root → hub queue → alice (admin) resolves over the wire"
bob POST /admin/propose_upstream "$(jq -cn --arg d "$TEAM_ROOT" --argjson op "$(insert_op "team rule")" '{doc_id:$d,ops:[$op],note:""}')" | jqe '.state=="pending"'
HQ=$(alice GET "/admin/hubs/queue?hub=$HUB_ON_ALICE")
check "hub queue: 1 proposal waiting, from bob"        jqe '.items | length==1 and .[0].proposer=="bob"' <<<"$HQ"
check "a plain member is refused the hub queue"        jqe '.error' <<<"$(bob GET "/admin/hubs/queue?hub=$HUB_ON_BOB")"
HANN=$(jq -r '.items[0].item.annotation.id' <<<"$HQ")
alice POST /admin/hubs/resolve "$(jq -cn --arg h "$HUB_ON_ALICE" --arg a "$HANN" '{hub:$h,annotation_id:$a,decision:"accept"}')" | jqe .ok
check "hub root has 'team rule'"                       jqe '[.roots[].block.content] | index("team rule")' <<<"$(hub GET "/api/doc/$TEAM_ROOT")"
wait_for "bob's relay of the Team root updates" 'bob POST /admin/pull >/dev/null; doc_has bob $BOB_PORT $TEAM_ROOT "team rule"' >/dev/null
ok "bob sees it"

step "4. alice transfers Notes to Team: offer → admin accept → ownership flips both sides"
alice POST /admin/hubs/transfer "$(jq -cn --arg h "$HUB_ON_ALICE" --arg d "$NOTES" '{hub:$h,root_doc:$d}')" | jqe .ok && ok "offer recorded"
T=$(alice GET "/admin/hubs/transfers?hub=$HUB_ON_ALICE" | jq -r '.transfers[] | select(.state=="offered") | .id')
check "hub lists it: alice offers Notes"               jqe '.transfers[0] | .member=="alice" and .title=="Notes"' <<<"$(alice GET "/admin/hubs/transfers?hub=$HUB_ON_ALICE")"
alice POST /admin/hubs/transfers/accept "$(jq -cn --arg h "$HUB_ON_ALICE" --arg id "$T" '{hub:$h,id:$id}')" | jqe .ok
wait_for "transfer done on the hub" '[ "$(hub_transfer_state $T)" = done ] && echo yes' >/dev/null
ok "hub: transfer done"
check "hub owns Notes (no mirror row)"                 jqe '.mirror==null' <<<"$(hub GET "/api/doc/$NOTES/federation")"
check "hub: publication record gone"                   jqe '[.[].publications[]] | length==0' <<<"$(hub GET /admin/hub/members)"
check "alice: her copy is a mirror of Team, marked transferred from her" \
  jqe '.mirror.owner_petname=="Team" and .mirror.transferred_from_me==true and .mirror.permission=="propose"' <<<"$(alice GET "/api/doc/$NOTES/federation")"
check "alice's hub card: handed to Team; no longer a publication" \
  jqe '.[0].transfers[0].state=="done" and (.[0].publications|length==0)' <<<"$(alice GET /admin/hubs)"
check "alice's local edit is refused (mirror)" \
  jqe '.error' <<<"$(alice POST /api/propose "$(jq -cn --arg d "$NOTES" --argjson op "$(insert_op "x")" '{doc_id:$d,base_epoch:2,ops:[$op]}')")"
wait_for "bob's relay of Notes has no origin_owner" 'bob POST /admin/pull >/dev/null; bob GET /api/doc/$NOTES/federation | jq -r "select(.mirror.origin_owner==null and .mirror.owner_petname==\"Team\") | \"yes\""' >/dev/null
ok "bob: Notes is owned by Team now (no origin owner)"
alice POST /admin/pull >/dev/null
check "alice's content untouched"                      jqe '[.roots[].block.content] | index("alice notes")' <<<"$(alice GET "/api/doc/$NOTES")"
check "alice's copy now files under Team/alice" \
  jqe '.title=="alice"' <<<"$(alice GET "/api/doc/$(alice GET "/api/doc/$NOTES" | jq -r .doc.parent_id)" | jq .doc)"
check "accepting a done transfer again is a no-op" \
  jqe .ok <<<"$(alice POST /admin/hubs/transfers/accept "$(jq -cn --arg h "$HUB_ON_ALICE" --arg id "$T" '{hub:$h,id:$id}')")"
alice POST /admin/propose_upstream "$(jq -cn --arg d "$NOTES" --argjson op "$(insert_op "alice again")" '{doc_id:$d,ops:[$op],note:""}')" | jqe '.state=="pending"'
check "alice's edit to her former doc waits in the hub queue" \
  jqe '.items[0].proposer=="alice" and .items[0].doc_title=="Notes"' <<<"$(alice GET "/admin/hubs/queue?hub=$HUB_ON_ALICE")"

step "5. transfer refused with Busy while a doc is live; goes through once idle"
PLAN=$(alice POST /api/docs '{"title":"Plan"}' | jq -r .id)
alice POST /api/propose "$(jq -cn --arg d "$PLAN" --argjson op "$(insert_op "the plan")" '{doc_id:$d,base_epoch:0,ops:[$op]}')" >/dev/null
alice POST /admin/hubs/transfer "$(jq -cn --arg h "$HUB_ON_ALICE" --arg d "$PLAN" '{hub:$h,root_doc:$d}')" | jqe .ok
T2=$(alice GET "/admin/hubs/transfers?hub=$HUB_ON_ALICE" | jq -r --arg d "$PLAN" '.transfers[] | select(.root_doc==$d and .state=="offered") | .id')
alice POST "/api/doc/$PLAN/hot/start" '{}' >/dev/null && ok "Plan is live on alice"
alice POST /admin/hubs/transfers/accept "$(jq -cn --arg h "$HUB_ON_ALICE" --arg id "$T2" '{hub:$h,id:$id}')" | jqe .ok
wait_for "hub logs the Busy refusal" 'cat $ROOT/hub/ksd.*.log | rg -q "is in a live session" && echo yes' >/dev/null
ok "hub log: refused — “Plan” is in a live session"
wait_for "transfer bounced back to offered" '[ "$(hub_transfer_state $T2)" = offered ] && echo yes' >/dev/null
ok "hub: transfer back to offered (admin can retry)"
check "alice still owns Plan"                          jqe '.mirror==null' <<<"$(alice GET "/api/doc/$PLAN/federation")"
alice POST "/api/doc/$PLAN/hot/end" >/dev/null
alice POST /admin/hubs/transfers/accept "$(jq -cn --arg h "$HUB_ON_ALICE" --arg id "$T2" '{hub:$h,id:$id}')" | jqe .ok
wait_for "Plan transfer done" '[ "$(hub_transfer_state $T2)" = done ] && echo yes' >/dev/null
ok "hub: Plan taken over once idle"
check "alice: Plan is a mirror of Team"                jqe '.mirror.owner_petname=="Team"' <<<"$(alice GET "/api/doc/$PLAN/federation")"

step "all green"
echo "logs: $ROOT/{hub,alice,bob}/ksd.*.log (dir is kept)"
trap - EXIT; cleanup
