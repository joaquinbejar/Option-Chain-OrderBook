#!/usr/bin/env bash
# Self test of the bans in clippy.toml. Builds src/lint_probe.rs (cfg
# ocob_lint_probe) with clippy and requires exactly one disallowed-method
# hit on every line of the probe marked `// ban:`, and none elsewhere.
set -euo pipefail
cd "$(dirname "$0")/.."

probe=src/lint_probe.rs
expected=$(grep -nE '^[^/]+// ban:' "$probe" | cut -d: -f1 | sort -n)
[ -n "$expected" ] || { echo "lint selftest: no // ban: lines in $probe" >&2; exit 1; }

log=$(mktemp)
trap 'rm -f "$log"' EXIT
# Clippy exits 0 here: the probe hits are warnings, not denied.
cargo clippy --lib --message-format=short -- --cfg ocob_lint_probe >"$log" 2>&1 || {
    cat "$log" >&2
    echo "lint selftest: clippy failed on the probe" >&2
    exit 1
}
actual=$(grep -E "^$probe:[0-9]+:[0-9]+: warning: use of a disallowed method" "$log" \
    | cut -d: -f2 | sort -n)

if [ "$expected" != "$actual" ]; then
    echo "lint selftest: disallowed-method hits differ from the // ban: lines" >&2
    diff <(echo "$expected") <(echo "$actual") >&2 || true
    exit 1
fi
echo "lint selftest: $(echo "$expected" | wc -l | tr -d ' ') bans fire"
