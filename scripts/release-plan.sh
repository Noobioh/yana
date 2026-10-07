#!/bin/sh
# Decides the next version from Conventional Commits since the last v* tag and
# writes release notes. Prints `version=X.Y.Z` (empty when nothing warrants a
# release) and `notes=<file>`, ready to append to $GITHUB_OUTPUT.
#   feat!: / BREAKING CHANGE -> major, feat -> minor, fix/perf -> patch, else none
set -eu
cd "$(dirname "$0")/.."
NOTES=${1:-dist/release-notes.md}

last=$(git describe --tags --abbrev=0 --match 'v[0-9]*' 2>/dev/null || true)
if [ -n "$last" ]; then
    base=${last#v}
    range="$last..HEAD"
else
    base=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
    range=HEAD
fi
BREAKING='^[a-z]+(\([^)]*\))?!:'
subjects=$(git log --no-merges --format=%s "$range")
IFS=. read -r major minor patch <<EOF
$base
EOF

if printf '%s\n' "$subjects" | grep -qE '^[a-z]+(\([^)]*\))?!:' ||
    git log --no-merges --format=%b "$range" | grep -q '^BREAKING[ -]CHANGE'; then
    version="$((major + 1)).0.0"
elif printf '%s\n' "$subjects" | grep -qE '^feat(\([^)]*\))?:'; then
    version="$major.$((minor + 1)).0"
elif printf '%s\n' "$subjects" | grep -qE '^(fix|perf)(\([^)]*\))?:'; then
    version="$major.$minor.$((patch + 1))"
else
    version=
fi

section() {
    [ -n "$subjects" ] || return 0
    items=$(printf "%s\n" "$subjects" | grep $2 "$3" | sed -E "s/^[a-z]+(\(([^)]*)\))?!?: */\2: /; s/^: //; s/^/- /") || true
    [ -n "$items" ] && printf "## %s\n\n%s\n\n" "$1" "$items"
    return 0
}

mkdir -p "$(dirname "$NOTES")"
{
    section Features -E "$BREAKING|^feat(\([^)]*\))?:"
    section Fixes -E "^(fix|perf)(\([^)]*\))?:"
    section Other -vE "$BREAKING|^(feat|fix|perf)(\([^)]*\))?:"
    if [ -n "$last" ] && [ -n "$version" ] && [ -n "${GITHUB_REPOSITORY:-}" ]; then
        echo "**Full diff**: https://github.com/$GITHUB_REPOSITORY/compare/$last...v$version"
    fi
} > "$NOTES"

echo "version=$version"
echo "notes=$NOTES"
