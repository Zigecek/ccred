#!/bin/sh
# Refuse a panic site in the code that runs unattended.
#
# A panic in a scheduled refresh is a slot that reports nothing and a profile
# that quietly goes stale -- the failure this program exists to prevent, with
# no message anywhere a person looks. Every reachable `unwrap`, `expect` and
# `panic!` has been rewritten as a contained failure that says what happened;
# this keeps the count at zero.
#
# Tests are exempt: a test that cannot unwrap is a test that says less when it
# fails. What counts as a test is a `#[cfg(test)]` module, tracked by brace
# depth -- the first version of this script stopped reading a file at the
# first `#[cfg(test)]`, so a file with an inline test module halfway through
# had everything after it unscanned, which hid two `expect`s in the module
# this script was written for.
#
# Run it the same way CI does:  sh scripts/no-panics.sh
set -eu

shipped_lines() {
    awk '
        # A test module: skip it and everything nested inside it.
        pending && /^[[:space:]]*(pub([[:space:]]*\([^)]*\))?[[:space:]]+)?mod[[:space:]]+[A-Za-z0-9_]+[[:space:]]*\{/ {
            in_test = 1
            depth = 1
            pending = 0
            next
        }
        in_test {
            opens = gsub(/\{/, "{")
            closes = gsub(/\}/, "}")
            depth += opens - closes
            if (depth <= 0) { in_test = 0 }
            next
        }
        # `#[cfg(test)]` stands on the line before what it annotates, which is
        # a module here and a function elsewhere. Only a module is skipped.
        /^[[:space:]]*#\[cfg\(test\)\]/ { pending = 1; print ""; next }
        { pending = 0; print }
    ' "$1"
}

offenders=0
for file in $(find src -name '*.rs' | sort); do
    found=$(shipped_lines "$file" |
        grep -nE '\.unwrap\(\)|\.expect\(|panic!\(|unreachable!\(|todo!\(' || true)
    if [ -n "$found" ]; then
        echo "$file:"
        printf '%s\n' "$found" | sed 's/^/  /'
        offenders=$((offenders + 1))
    fi
done

if [ "$offenders" -ne 0 ]; then
    echo
    echo "A panic here ends an unattended run with nothing to read afterwards."
    echo "Handle the case and say what happened, or move the line into a test."
    exit 1
fi
echo "no panic sites outside tests"
