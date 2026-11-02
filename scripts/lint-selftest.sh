#!/usr/bin/env bash
# Self test of the bans in clippy.toml. Builds src/lint_probe.rs (cfg
# ocob_lint_probe) with clippy and requires exactly one disallowed-method
# hit on every line of the probe marked `// ban:`, and none elsewhere.
set -euo pipefail
cd "$(dirname "$0")/.."

probe=src/lint_probe.rs
fail() {
    echo "lint selftest: $*" >&2
    exit 1
}

expected=$(grep -nE '^[^/]+// ban:' "$probe" | cut -d: -f1 | sort -n || true)
[ -n "$expected" ] || fail "no // ban: lines in $probe"

log=$(mktemp)
trap 'rm -f "$log"' EXIT
# The log is parsed: no colour codes, whatever CI sets. Clippy exits 0 here:
# the probe hits are warnings, not denied.
if ! CARGO_TERM_COLOR=never cargo clippy --lib --color never --message-format=short \
    -- --cfg ocob_lint_probe >"$log" 2>&1; then
    cat "$log" >&2
    fail "clippy failed on the probe"
fi
actual=$(grep -E "^$probe:[0-9]+:[0-9]+: warning: use of a disallowed method" "$log" \
    | cut -d: -f2 | sort -n || true)

if [ "$expected" != "$actual" ]; then
    echo "--- clippy output on the probe" >&2
    grep -E "^$probe:" "$log" >&2 || cat "$log" >&2
    for line in $expected; do
        grep -qx "$line" <<<"$actual" \
            || echo "missing ban at $probe:$line: $(sed -n "${line}p" "$probe" | sed 's|.*// ban: ||')" >&2
    done
    for line in $actual; do
        grep -qx "$line" <<<"$expected" || echo "unexpected hit at $probe:$line" >&2
    done
    fail "disallowed-method hits differ from the // ban: lines"
fi
echo "lint selftest: $(wc -l <<<"$expected" | tr -d ' ') bans fire"
