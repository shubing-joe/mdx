#!/usr/bin/env bash
#
# Fail when a resolved dependency carries a license this project cannot ship.
#
# `cargo metadata` is the source of truth: it resolves the graph the build
# actually uses, including target-specific and optional dependencies. The denied
# families are GPL, AGPL, SSPL and EPL.
#
# Two deliberate choices, both conservative:
#
#   * LGPL is *not* matched by the GPL rule — it is a linking question, not a
#     distribution blocker — but it is printed like every other license, so a
#     reviewer sees it.
#   * a dual-licensed expression such as `MIT OR GPL-3.0` *is* reported, because
#     picking the permissive half of an OR is a decision for a human, not for
#     this script. The SPDX expression is printed verbatim.
#
# Usage: scripts/license-audit.sh [MANIFEST_DIR]
#   MANIFEST_DIR  directory holding the Cargo.toml to audit
#                 (default: the parent of this script)
#
# Exit status: 0 clean, 1 a denied license was found, 2 the audit could not run.

set -euo pipefail

root="${1:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)}"

if [ ! -f "$root/Cargo.toml" ]; then
    printf 'license-audit: no Cargo.toml in %s\n' "$root" >&2
    exit 2
fi

for tool in cargo jq; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf 'license-audit: %s is required but was not found on PATH\n' "$tool" >&2
        exit 2
    fi
done

printf 'license-audit: resolving the dependency graph of %s\n' "$root"
metadata="$(cargo metadata --manifest-path "$root/Cargo.toml" --format-version 1)"

# One tab separated `<name> <version> <license> <verdict>` line per package,
# deduplicated and sorted so the report is stable between runs.
report="$(
    printf '%s' "$metadata" | jq -r '
        def denied: test("(agpl|sspl|epl|(^|[^a-z])gpl)"; "i");
        .packages
        | map({
            name,
            version,
            license: (if (.license // "") == "" then null else .license end)
          })
        | unique_by(.name, .version)
        | sort_by(.name, .version)
        | .[]
        | [
            .name,
            .version,
            (if .license == null then "-" else .license end),
            (if .license == null then "unknown"
             elif (.license | denied) then "denied"
             else "ok" end)
          ]
        | @tsv
    '
)"

total=0
denied_count=0
unknown_count=0

while IFS=$'\t' read -r name version license verdict; do
    [ -n "$name" ] || continue
    total=$((total + 1))
    case "$verdict" in
        denied)
            denied_count=$((denied_count + 1))
            printf 'DENIED   %s %s  %s\n' "$name" "$version" "$license"
            ;;
        unknown)
            unknown_count=$((unknown_count + 1))
            printf 'UNKNOWN  %s %s  (no SPDX license in the package manifest)\n' "$name" "$version"
            ;;
        *)
            printf 'ok       %s %s  %s\n' "$name" "$version" "$license"
            ;;
    esac
done <<< "$report"

printf '\nlicense-audit: %d packages, %d denied, %d without an SPDX license\n' \
    "$total" "$denied_count" "$unknown_count"

if [ "$unknown_count" -gt 0 ]; then
    printf 'license-audit: the packages marked UNKNOWN need a manual look\n' >&2
fi

if [ "$denied_count" -gt 0 ]; then
    printf 'license-audit: FAILED — GPL, AGPL, SSPL or EPL dependencies are not shippable here\n' >&2
    exit 1
fi

printf 'license-audit: OK\n'
