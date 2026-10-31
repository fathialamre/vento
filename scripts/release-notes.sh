#!/bin/sh
# Prints the body of one version's section of CHANGELOG.md (the lines after
# "## [X.Y.Z]" up to the next "## "), for `gh release create --notes-file`.
# Usage: scripts/release-notes.sh <version>
set -eu

if [ "$#" -ne 1 ]; then
	echo "usage: release-notes.sh <version>" >&2
	exit 1
fi

root=$(cd "$(dirname "$0")/.." && pwd)
version=$1

body=$(awk -v header="## [$version]" '
	index($0, header) == 1 { found = 1; next }
	found && /^## / { exit }
	found { print }
	END { if (!found) exit 2 }
' "$root/CHANGELOG.md") || {
	echo "release-notes.sh: no section for $version in CHANGELOG.md" >&2
	exit 1
}

# Trim leading and trailing blank lines.
printf '%s\n' "$body" | awk '
	NF { started = 1 }
	started { lines[++n] = $0 }
	END {
		while (n > 0 && lines[n] !~ /[^[:space:]]/) n--
		for (i = 1; i <= n; i++) print lines[i]
	}
'
