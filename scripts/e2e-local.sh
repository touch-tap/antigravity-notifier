#!/bin/bash
# antigravity-notifier end to end against CCNotifier_Server running as `wrangler dev --local --port 8799`, with curl
# playing the phone. Needs `cargo build` first, and a server that knows clientKind antigravity_cli
# and X-App antigravity (Nov 2026: on a branch, worktree CCNotifier_Server-antigravity).
# AGN_TEST_DIR, AGN_BIN and AGN_SERVER override the defaults. Antigravity itself is not needed:
# the hook input below is what agy 1.5.18 sends. Nothing here touches ~/.gemini.
set -u
S=${AGN_TEST_DIR:-/tmp/antigravity-notifier-e2e}; mkdir -p $S/gemini-home $S/proj
B=${AGN_BIN:-$(cd "$(dirname "$0")/.." && pwd)/target/debug/antigravity-notifier}
SERVER=${AGN_SERVER:-http://127.0.0.1:8799}
export ANTIGRAVITY_NOTIFIER_GEMINI_HOME=$S/gemini-home ANTIGRAVITY_NOTIFIER_HOME=$S/agn-home ANTIGRAVITY_NOTIFIER_SERVER_URL=$SERVER
export ANTIGRAVITY_NOTIFIER_NOTIFY=off ANTIGRAVITY_NOTIFIER_NO_ANALYTICS=1 ANTIGRAVITY_NOTIFIER_CREDENTIAL_STORE=file
export ANTIGRAVITY_NOTIFIER_NO_AUTO_UPDATE=1 ANTIGRAVITY_NOTIFIER_NO_NUDGE=1 ANTIGRAVITY_NOTIFIER_LANG=en
# The hooks are run by this script, not by an interactive agy, so say so
export ANTIGRAVITY_NOTIFIER_HEADLESS=0
HOOKS=$ANTIGRAVITY_NOTIFIER_GEMINI_HOME/config/hooks.json
DEVICE=$(uuidgen | tr A-Z a-z)
rm -rf $ANTIGRAVITY_NOTIFIER_HOME $ANTIGRAVITY_NOTIFIER_GEMINI_HOME/config; mkdir -p $ANTIGRAVITY_NOTIFIER_GEMINI_HOME/config
# Someone else's hook group, which must come through untouched
printf '{\n  "mine": { "Stop": [ { "type": "command", "command": "echo hi", "timeout": 5 } ] }\n}\n' > $HOOKS
pass=0; fail=0
ok()   { echo "  ok   $1"; pass=$((pass+1)); }
bad()  { echo "  FAIL $1"; fail=$((fail+1)); }
phone() { curl -s -m 20 -H "X-App: antigravity" -H "X-Device-ID: $DEVICE" -H "Content-Type: application/json" "$@"; }

echo "== phone shows a code"
CODE=$(phone -X POST $SERVER/api/pairing/request-for-device -d "{\"deviceId\":\"$DEVICE\",\"platform\":\"ios\",\"name\":\"qa-antigravity-phone\"}" | python3 -c 'import sys,json;print(json.load(sys.stdin)["code"])')
echo "  code $CODE"

echo "== antigravity-notifier pair \$CODE"
out=$($B pair "$CODE" --name qa-antigravity-mac </dev/null 2>&1); echo "$out" | sed 's/^/  | /'
echo "$out" | grep -q "Paired with" && ok "paired" || bad "pair"
grep -q '"antigravity-notifier"' $HOOKS && grep -q 'hook PreToolUse' $HOOKS && ok "hook group written" || bad "hooks.json"
grep -q '"mine"' $HOOKS && grep -q 'echo hi' $HOOKS && ok "the other group is still there" || bad "other group lost"

echo "== server sees antigravity_cli"
me=$(phone $SERVER/api/pairings)
echo "$me" | grep -q '"clientKind":"antigravity_cli"' && ok "pairing lists clientKind antigravity_cli" || bad "clientKind: $me"

# hook <Event>: as hooks.json calls it, input on stdin
hook() { ANTIGRAVITY_NOTIFIER_PRESENCE=${PRESENCE:-present} $B hook "$1"; }
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
export SEEN=$S/agn-seen.txt; rm -f $SEEN
answer() { # $1 text: respond to the latest pending notification, retrying until it appears
  for i in $(seq 1 40); do id=$(latest_pending); [ -n "$id" ] && break; sleep 0.5; done
  [ -z "$id" ] && { echo "  (no pending notification)"; return 1; }
  echo "$id" >> $SEEN
  phone -X POST "$SERVER/api/notifications/$id/respond" -d "{\"response\":$(python3 -c 'import json,sys;print(json.dumps(sys.argv[1]))' "$1")}" >/dev/null
}
# The fields every event carries
T=$S/transcript_full.jsonl
base() { printf '"hookEventName":"%s","conversationId":"c1","cwd":"%s","workspacePaths":["%s"],"transcriptPath":"%s","modelName":"gemini-4-hydrogen-high","lastUserInput":"add a login page"' "$1" "$S/proj" "$S/proj" "$T"; }
tool() { # $1 name, $2 args JSON, $3 call id
  printf '{%s,"stepIdx":2,"toolCall":{"name":"%s","args":%s,"id":"%s"}}' "$(base PreToolUse)" "$1" "$2" "$3"
}
stop() { # $1 executionNum
  printf '{%s,"continuationActive":false,"error":"","executionNum":%s,"fullyIdle":true,"terminationReason":"NO_TOOL_CALL"}' "$(base Stop)" "$1"
}
say() { # $1 step, $2 text: Antigravity's final message in the transcript
  printf '{"step_index":%s,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","content":"%s"}\n' "$1" "$2" >> $T
}
rm -f $T

echo "== settle the welcome card the server pushed at pairing"
answer "hello from the test phone" && ok "welcome answered" || bad "welcome"

echo "== SessionStart"
out=$(printf '{%s}' "$(base SessionStart)" | hook SessionStart)
[ "$out" = "{}" ] && ok "nothing to say" || bad "output: $out"

echo "== PreInvocation: the user's prompt"
out=$(printf '{%s,"initialNumSteps":1,"invocationNum":0}' "$(base PreInvocation)" | hook PreInvocation)
[ "$out" = "{}" ] && ok "nothing to say" || bad "output: $out"
grep -q 'add a login page' $ANTIGRAVITY_NOTIFIER_HOME/sessions/c1.json && ok "prompt remembered" || bad "prompt not in the session state"

echo "== PreToolUse run_command, user present: allowed at once, nothing sent"
start=$(date +%s)
out=$(tool run_command '{"CommandLine":"rm -rf build","toolSummary":"Remove the build folder"}' call1 | hook PreToolUse)
took=$(( $(date +%s) - start ))
[ "$out" = '{"decision":"allow"}' ] && [ $took -lt 5 ] && ok "allow in ${took}s" || bad "output '$out' in ${took}s"

echo "== walk-away: allowed with the user present, then they leave while the call is still open"
# A debug build lets the watcher's clock be shortened: look every 0.2 s, alert after 1.5 s away
WATCH=$S/presence; echo present > $WATCH
hasnt() { phone "$SERVER/api/notifications" | grep -c "sleep 300 hasn't finished"; }
out=$(tool run_command '{"CommandLine":"sleep 300"}' callw | ANTIGRAVITY_NOTIFIER_PRESENCE=file:$WATCH ANTIGRAVITY_NOTIFIER_TEST_WATCH_MS=200,1500,60000 $B hook PreToolUse)
[ "$out" = '{"decision":"allow"}' ] && ok "allow" || bad "output: $out"
[ -e $ANTIGRAVITY_NOTIFIER_HOME/watch/c1.json ] && ok "watcher has its note" || bad "no note"
sleep 2; [ "$(hasnt)" = 0 ] && ok "nothing sent while the user is there" || bad "sent too early"
echo away > $WATCH
for i in $(seq 1 20); do [ "$(hasnt)" != 0 ] && break; sleep 0.5; done
[ "$(hasnt)" != 0 ] && ok "one note reached the phone after the grace" || bad "no walk-away note on the server"
sleep 3
n=$(phone "$SERVER/api/notifications" | python3 -c 'import sys,json
d=json.load(sys.stdin)
print(sum(1 for m in d.get("macs",[]) for n in m.get("notifications",[]) if "sleep 300 hasn" in json.dumps(n) and not n.get("supportsRemoteInput")))')
[ "$n" = 1 ] && ok "exactly one, and it can't be answered from the phone" || bad "$n walk-away notes"
[ ! -e $ANTIGRAVITY_NOTIFIER_HOME/watch/c1.json ] && ok "the note is gone" || bad "note left"

echo "== walk-away: the call finishes first, so leaving sends nothing"
echo present > $WATCH
out=$(tool run_command '{"CommandLine":"sleep 301"}' callx | ANTIGRAVITY_NOTIFIER_PRESENCE=file:$WATCH ANTIGRAVITY_NOTIFIER_TEST_WATCH_MS=200,1500,60000 $B hook PreToolUse)
printf '{%s,"stepIdx":2,"error":"","toolCall":{"name":"run_command","args":{"CommandLine":"sleep 301"},"id":"callx"}}' "$(base PostToolUse)" | hook PostToolUse >/dev/null
[ ! -e $ANTIGRAVITY_NOTIFIER_HOME/watch/c1.json ] && ok "PostToolUse ended the watch" || bad "note left"
echo away > $WATCH; sleep 4
[ "$(phone "$SERVER/api/notifications" | grep -c "sleep 301 hasn't finished")" = 0 ] && ok "nothing sent" || bad "sent after the call finished"

echo "== PreToolUse read_file, user away: never held"
out=$(tool read_file "{\"Path\":\"$S/proj/a.ts\"}" call2 | PRESENCE=away hook PreToolUse)
[ "$out" = '{"decision":"allow"}' ] && ok "allow" || bad "output: $out"

echo "== PreToolUse run_command, user away, phone answers y"
( sleep 2; answer "y" ) &
out=$(tool run_command '{"CommandLine":"rm -rf build","toolSummary":"Remove the build folder"}' call3 | PRESENCE=away hook PreToolUse)
wait
echo "  | $out"
[ "$out" = '{"decision":"auto_approve"}' ] && ok "auto_approve returned" || bad "no auto_approve"

echo "== PreToolUse run_command, user away, phone denies with a note"
( sleep 2; answer "use trash instead" ) &
out=$(tool run_command '{"CommandLine":"rm -rf dist"}' call4 | PRESENCE=away hook PreToolUse)
wait
echo "  | $out"
echo "$out" | grep -q '"decision":"deny"' && echo "$out" | grep -q 'use trash instead' && ok "deny with the note as the reason" || bad "no deny"

echo "== PreToolUse write_file, user away: not held by default (phone_approvals commands)"
start=$(date +%s)
out=$(tool write_file "{\"Path\":\"$S/proj/z.ts\",\"Content\":\"x\\n\"}" call4b | PRESENCE=away hook PreToolUse)
took=$(( $(date +%s) - start ))
[ "$out" = '{"decision":"allow"}' ] && [ $took -lt 5 ] && ok "allow in ${took}s" || bad "output '$out' in ${took}s"
$B config set phone_approvals all >/dev/null 2>&1

echo "== PreToolUse write_file, phone_approvals all, user away, phone allows for the session"
( sleep 2; answer "a" ) &
out=$(tool write_file "{\"Path\":\"$S/proj/a.ts\",\"Content\":\"one\\ntwo\\n\",\"Overwrite\":true}" call5 | PRESENCE=away hook PreToolUse)
wait
[ "$out" = '{"decision":"auto_approve"}' ] && ok "auto_approve returned" || bad "output: $out"
out=$(printf '{%s,"stepIdx":3,"error":"","toolCall":{"name":"write_file","args":{"Path":"%s/proj/a.ts","Content":"one\\ntwo\\n","Overwrite":true},"id":"call5"}}' "$(base PostToolUse)" "$S" | hook PostToolUse)
[ "$out" = "{}" ] && ok "PostToolUse quiet" || bad "PostToolUse output: $out"
start=$(date +%s)
out=$(tool write_file "{\"Path\":\"$S/proj/b.ts\",\"Content\":\"x\\n\"}" call6 | PRESENCE=away hook PreToolUse)
took=$(( $(date +%s) - start ))
[ "$out" = '{"decision":"auto_approve"}' ] && [ $took -lt 5 ] && ok "the next write_file needs no card (${took}s)" || bad "output '$out' in ${took}s"

echo "== PreToolUse ask_question, phone picks option 2"
( sleep 2; answer '[[MULTI_ANSWER]]["2"]' ) &
out=$(tool ask_question '{"questions":[{"question":"Which framework?","options":["React","Vue"],"is_multi_select":false}]}' call7 | PRESENCE=away hook PreToolUse)
wait
echo "  | $out"
echo "$out" | grep -q '"decision":"deny"' && echo "$out" | grep -q 'Which framework?: Vue' && ok "answer returned as deny reason" || bad "no answer"

echo "== PreToolUse ask_question, multi-select, phone picks 1 and 3"
( sleep 2; answer '[[MULTI_ANSWER]]["1,3"]' ) &
out=$(tool ask_question '{"questions":[{"question":"Which toppings?","options":["ham","olives","onion"],"is_multi_select":true}]}' call8 | PRESENCE=away hook PreToolUse)
wait
echo "  | $out"
echo "$out" | grep -q 'Which toppings?: ham, onion' && ok "both choices returned" || bad "multi-select answer missing (check what the phone sends for several choices)"

echo "== PreToolUse free-text question, phone types"
( sleep 2; answer 'Pinloop' ) &
out=$(tool ask_question '{"questions":[{"question":"What should the app be called?"}]}' call9 | PRESENCE=away hook PreToolUse)
wait
echo "  | $out"
echo "$out" | grep -q 'called?: Pinloop' && ok "free text returned" || bad "no free text"

echo "== Stop, user away, phone sends a follow-up"
say 5 "Login page added."
( sleep 2; answer 'now add tests' ) &
out=$(stop 0 | PRESENCE=away hook Stop)
wait
echo "  | $out"
echo "$out" | grep -q '"decision":"continue"' && echo "$out" | grep -q 'now add tests' && ok "follow-up returned as continue" || bad "no continue"

echo "== the same Stop again: one reply, one continue"
start=$(date +%s)
out=$(stop 0 | PRESENCE=away hook Stop)
took=$(( $(date +%s) - start ))
[ "$out" = "{}" ] && [ $took -lt 5 ] && ok "no second card or continue (${took}s)" || bad "output '$out' in ${took}s"

echo "== Stop of the continued turn, user present: no hold"
say 9 "Tests added."
start=$(date +%s)
out=$(stop 1 | hook Stop)
took=$(( $(date +%s) - start ))
[ "$out" = "{}" ] && [ $took -lt 5 ] && ok "quiet in ${took}s" || bad "output '$out' in ${took}s"

echo "== input that is not JSON never blocks a tool call"
out=$(echo 'garbage' | hook PreToolUse); rc=$?
[ "$out" = '{"decision":"allow"}' ] && [ $rc = 0 ] && ok "allow, exit 0" || bad "output '$out' exit $rc"

echo "== doctor"
$B doctor 2>&1 | sed 's/^/  | /' | grep -i "hooks\|registered\|phones"

echo "== uninstall leaves the hooks file as it was"
$B uninstall </dev/null >/dev/null 2>&1
! grep -q 'antigravity-notifier' $HOOKS && ok "our group removed" || bad "group left: $(cat $HOOKS)"
grep -q 'echo hi' $HOOKS && ok "the other group is still there" || bad "other group lost"
echo
echo "passed $pass, failed $fail"
echo "--- log"; tail -25 $ANTIGRAVITY_NOTIFIER_HOME/antigravity-notifier.log 2>/dev/null
