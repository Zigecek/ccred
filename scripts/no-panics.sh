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
# fails. Everything before `#[cfg(test)]` in a file is what ships.
#
# Run it the same way CI does:  sh scripts/no-panics.sh
set -eu

offenders=0
for file in $(find src -name '*.rs' | sort); do
    shipped=$(awk '/^#\[cfg\(test\)\]/ { exit } { print }' "$file")
    found=$(printf '%s\n' "$shipped" |
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
