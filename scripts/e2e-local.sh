#!/bin/bash
# codex-notifier end to end against CCNotifier_Server running as `wrangler dev --local --port 8799`, with curl
# playing the phone. Needs `cargo build` first. CXN_TEST_DIR, CXN_BIN and CXN_SERVER override the defaults.
set -u
S=${CXN_TEST_DIR:-/tmp/codex-notifier-e2e}; mkdir -p $S/codex-home
B=${CXN_BIN:-$(cd "$(dirname "$0")/.." && pwd)/target/debug/codex-notifier}
SERVER=${CXN_SERVER:-http://127.0.0.1:8799}
export CODEX_HOME=$S/codex-home CODEX_NOTIFIER_HOME=$S/cxn-home CODEX_NOTIFIER_SERVER_URL=$SERVER
export CODEX_NOTIFIER_NOTIFY=off CODEX_NOTIFIER_NO_ANALYTICS=1 CODEX_NOTIFIER_CREDENTIAL_STORE=file
DEVICE=$(uuidgen | tr A-Z a-z)
rm -rf $CODEX_NOTIFIER_HOME; rm -f $CODEX_HOME/hooks.json; printf 'model = "gpt-5.4"\nmodel_reasoning_effort = "low"\n' > $CODEX_HOME/config.toml
pass=0; fail=0
ok()   { echo "  ok   $1"; pass=$((pass+1)); }
bad()  { echo "  FAIL $1"; fail=$((fail+1)); }
phone() { curl -s -m 20 -H "X-App: codex" -H "X-Device-ID: $DEVICE" -H "Content-Type: application/json" "$@"; }

echo "== phone shows a code"
CODE=$(phone -X POST $SERVER/api/pairing/request-for-device -d "{\"deviceId\":\"$DEVICE\",\"platform\":\"ios\",\"name\":\"qa-codex-phone\"}" | python3 -c 'import sys,json;print(json.load(sys.stdin)["code"])')
echo "  code $CODE"

echo "== codex-notifier pair \$CODE"
out=$($B pair "$CODE" --name qa-codex-mac </dev/null 2>&1); echo "$out" | sed 's/^/  | /'
echo "$out" | grep -q "Paired with" && ok "paired" || bad "pair"
grep -q '"SessionStart"' $CODEX_HOME/hooks.json && ok "hooks.json written" || bad "hooks.json"
grep -q 'trusted_hash' $CODEX_HOME/config.toml && ok "trust hashes written" || bad "trust"

echo "== server sees codex_cli"
me=$(phone $SERVER/api/pairings)
echo "$me" | grep -q '"clientKind":"codex_cli"' && ok "pairing lists clientKind codex_cli" || bad "clientKind: $me"

hook() { CODEX_NOTIFIER_PRESENCE=${PRESENCE:-present} $B hook; }
latest_pending() {
  phone "$SERVER/api/notifications" | python3 -c '
import sys,json
d=json.load(sys.stdin)
items=[n for m in d.get("macs",[]) for n in m.get("notifications",[])]
items.sort(key=lambda n: n.get("createdAt",0), reverse=True)
import os
seen=set(open(os.environ["SEEN"]).read().split()) if os.path.exists(os.environ["SEEN"]) else set()
for n in items:
    if n.get("status")=="pending" and n.get("supportsRemoteInput") and n["id"] not in seen: print(n["id"]); break'
}
export SEEN=$S/cxn-seen.txt; rm -f $SEEN
answer() { # $1 text: respond to the latest pending notification, retrying until it appears
  for i in $(seq 1 40); do id=$(latest_pending); [ -n "$id" ] && break; sleep 0.5; done
  [ -z "$id" ] && { echo "  (no pending notification)"; return 1; }
  echo "$id" >> $SEEN
  phone -X POST "$SERVER/api/notifications/$id/respond" -d "{\"response\":$(python3 -c 'import json,sys;print(json.dumps(sys.argv[1]))' "$1")}" >/dev/null
}

echo "== settle the welcome card the server pushed at pairing"
answer "hello from the test phone" && ok "welcome answered" || bad "welcome"

echo "== SessionStart"
out=$(echo '{"hook_event_name":"SessionStart","session_id":"s1","cwd":"/tmp/proj","model":"gpt-5.4","permission_mode":"default","source":"startup","transcript_path":null}' | hook)
[ -z "$out" ] && ok "silent" || bad "output: $out"

echo "== UserPromptSubmit"
out=$(echo '{"hook_event_name":"UserPromptSubmit","session_id":"s1","turn_id":"t1","cwd":"/tmp/proj","model":"gpt-5.4","permission_mode":"default","prompt":"add a login page","transcript_path":null}' | hook)
[ -z "$out" ] && ok "silent" || bad "output: $out"

echo "== PermissionRequest, user away, phone answers y"
( sleep 2; answer "y" ) &
out=$(echo '{"hook_event_name":"PermissionRequest","session_id":"s1","turn_id":"t1","cwd":"/tmp/proj","model":"gpt-5.4","permission_mode":"default","tool_name":"Bash","tool_input":{"command":"rm -rf build"},"transcript_path":null}' | PRESENCE=away hook)
wait
echo "  | $out"
echo "$out" | grep -q '"behavior":"allow"' && ok "allow decision returned" || bad "no allow"

echo "== PreToolUse request_user_input, phone picks option 2"
( sleep 2; answer '[[MULTI_ANSWER]]["2"]' ) &
out=$(echo '{"hook_event_name":"PreToolUse","session_id":"s1","turn_id":"t1","cwd":"/tmp/proj","model":"gpt-5.4","permission_mode":"default","tool_name":"request_user_input","tool_use_id":"call1","tool_input":{"questions":[{"id":"fw","header":"Framework","question":"Which framework?","options":[{"label":"React","description":"a"},{"label":"Vue","description":"b"}]}]},"transcript_path":null}' | PRESENCE=away hook)
wait
echo "  | $out"
echo "$out" | grep -q 'Which framework?: Vue' && ok "answer returned as deny reason" || bad "no answer"

echo "== PreToolUse free-text question, phone types"
( sleep 2; answer 'Pinloop' ) &
out=$(echo '{"hook_event_name":"PreToolUse","session_id":"s1","turn_id":"t1","cwd":"/tmp/proj","model":"gpt-5.4","permission_mode":"default","tool_name":"request_user_input","tool_use_id":"call2","tool_input":{"questions":[{"id":"name","header":"Name","question":"What should the app be called?"}]},"transcript_path":null}' | PRESENCE=away hook)
wait
echo "  | $out"
echo "$out" | grep -q 'called?: Pinloop' && ok "free text returned" || bad "no free text"

echo "== PreToolUse apply_patch (fast lane, no output)"
out=$(echo '{"hook_event_name":"PreToolUse","session_id":"s1","turn_id":"t1","cwd":"/tmp/proj","model":"gpt-5.4","permission_mode":"default","tool_name":"apply_patch","tool_use_id":"call3","tool_input":{"patch":"*** Begin Patch\n*** Update File: src/a.ts\n-x\n+y\n*** End Patch"},"transcript_path":null}' | hook)
[ -z "$out" ] && ok "silent" || bad "output: $out"

echo "== Stop, user away, phone sends a follow-up"
( sleep 2; answer 'now add tests' ) &
out=$(echo '{"hook_event_name":"Stop","session_id":"s1","turn_id":"t1","cwd":"/tmp/proj","model":"gpt-5.4","permission_mode":"default","stop_hook_active":false,"last_assistant_message":"Login page added.","transcript_path":null}' | PRESENCE=away hook)
wait
echo "  | $out"
echo "$out" | grep -q '"decision":"block"' && ok "follow-up returned as block" || bad "no block"

echo "== Stop, user present: no hold"
start=$(date +%s)
out=$(echo '{"hook_event_name":"Stop","session_id":"s1","turn_id":"t2","cwd":"/tmp/proj","model":"gpt-5.4","permission_mode":"default","stop_hook_active":false,"last_assistant_message":"Done.","transcript_path":null}' | hook)
took=$(( $(date +%s) - start ))
[ -z "$out" ] && [ $took -lt 5 ] && ok "silent in ${took}s" || bad "output '$out' in ${took}s"

echo "== SessionEnd"
out=$(echo '{"hook_event_name":"SessionEnd","session_id":"s1","cwd":"/tmp/proj","reason":"exit","transcript_path":null}' | hook)
[ -z "$out" ] && ok "silent" || bad "output: $out"
[ ! -e $CODEX_NOTIFIER_HOME/sessions/s1.json ] && ok "session state removed" || bad "state left"

echo "== doctor"
$B doctor 2>&1 | sed 's/^/  | /' | grep -i "trust\|hooks\|registered\|phones"

echo "== uninstall restores Codex's files"
$B uninstall </dev/null >/dev/null 2>&1
[ ! -e $CODEX_HOME/hooks.json ] && ok "hooks.json removed (it held only ours)" || bad "hooks.json left: $(cat $CODEX_HOME/hooks.json)"
! grep -q trusted_hash $CODEX_HOME/config.toml && ok "trust entries removed" || bad "trust left"
echo
echo "passed $pass, failed $fail"
echo "--- log"; tail -25 $CODEX_NOTIFIER_HOME/codex-notifier.log 2>/dev/null
