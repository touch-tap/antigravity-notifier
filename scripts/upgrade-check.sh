#!/bin/bash
# Proves a signed self-update end to end, with the real release key, against dry-run releases served
# locally (scripts/release.sh <version> --dry-run makes them):
#   scripts/upgrade-check.sh <old dry-run dir> <newer dry-run dir> [newer version]
# The old build is installed in a scratch folder and upgraded four times: from the real release
# (must succeed), from a SHA256SUMS with one byte changed, from a swapped archive with a SHA256SUMS
# rewritten to match, and from a release with no SHA256SUMS.sig (all three must leave it untouched).
set -u
old=$1; new=$2; next=${3:-0.1.1}; port=${PORT:-8791}; failures=0
W=$(mktemp -d); trap 'kill $srv 2>/dev/null; wait $srv 2>/dev/null; rm -rf $W' EXIT
asset=$(cd "$new" && ls antigravity-notifier-*.tar.gz | head -1)
mkdir -p $W/www/api/cli $W/www/good $W/www/tampered $W/www/swapped $W/www/unsigned
printf '{"version":"%s","tag":"v%s","assets":["%s","SHA256SUMS","SHA256SUMS.sig"]}' $next $next $asset > $W/www/api/cli/latest
cp $new/* $W/www/good/
# Tampered: one byte of SHA256SUMS changed (the archive's own line), signature left as it was
cp $new/* $W/www/tampered/; sed -i '' 's/^./0/' $W/www/tampered/SHA256SUMS; [ "$(head -c1 $new/SHA256SUMS)" = 0 ] && sed -i '' 's/^./1/' $W/www/tampered/SHA256SUMS
# Swapped: a different archive (the old build) with a SHA256SUMS rewritten to match it, old signature kept
cp $new/SHA256SUMS.sig $W/www/swapped/; cp $old/$asset $W/www/swapped/; (cd $W/www/swapped && shasum -a 256 $asset > SHA256SUMS)
# Unsigned: no SHA256SUMS.sig at all
cp $new/$asset $new/SHA256SUMS $W/www/unsigned/
(cd $W/www && exec python3 -m http.server $port >/dev/null 2>&1) & srv=$!
sleep 1
run() { # $1 = folder name; installs the old binary fresh and upgrades against that folder
  rm -rf $W/bin $W/home; mkdir -p $W/bin $W/home/gemini
  tar xzf $old/$asset -C $W/bin
  echo "== $1: installed $($W/bin/antigravity-notifier --version)"
  ANTIGRAVITY_NOTIFIER_HOME=$W/home/cxn ANTIGRAVITY_NOTIFIER_GEMINI_HOME=$W/home/gemini ANTIGRAVITY_NOTIFIER_NO_ANALYTICS=1 \
  ANTIGRAVITY_NOTIFIER_CREDENTIAL_STORE=file ANTIGRAVITY_NOTIFIER_SERVER_URL=http://127.0.0.1:$port \
  ANTIGRAVITY_NOTIFIER_DOWNLOAD_BASE=http://127.0.0.1:$port/$1 $W/bin/antigravity-notifier upgrade 2>&1 | sed 's/^/   | /'
  after=$($W/bin/antigravity-notifier --version)
  echo "   after: $after"
  if [ "$after" = "antigravity-notifier $2" ]; then echo "   ok"; else echo "   FAIL: expected antigravity-notifier $2"; failures=$((failures+1)); fi
}
installed=$(mkdir -p $W/probe; tar xzf $old/$asset -C $W/probe; $W/probe/antigravity-notifier --version | awk '{print $2}')
run good $next; run tampered $installed; run swapped $installed; run unsigned $installed
[ $failures = 0 ] && echo "All four checks passed." || { echo "$failures check(s) failed"; exit 1; }
