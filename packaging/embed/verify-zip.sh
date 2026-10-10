#!/usr/bin/env bash
# Check a built embed zip the way a host would:  packaging/embed/verify-zip.sh <zip>
#
# - the zip matches its sibling <zip>.sha256;
# - SHA256SUMS inside it matches every file it lists, and lists every file in the zip (but itself);
# - the files the embed API promises are there (docs/embed.md).
# Needs: unzip and sha256sum (or shasum). Writes nothing outside a temporary folder.
set -euo pipefail

zip="${1:?usage: verify-zip.sh <wordcraft-embed-version.zip>}"
[ -f "$zip" ] || { echo "error: $zip is not a file" >&2; exit 1; }
[ -f "$zip.sha256" ] || { echo "error: $zip.sha256 is missing" >&2; exit 1; }

check_sums() {
  if command -v sha256sum >/dev/null; then sha256sum -c "$@"; else shasum -a 256 -c "$@"; fi
}

(cd "$(dirname "$zip")" && check_sums "$(basename "$zip").sha256")

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
unzip -q "$zip" -d "$work"
[ -f "$work/SHA256SUMS" ] || { echo "error: the zip has no SHA256SUMS" >&2; exit 1; }
(cd "$work" && check_sums --quiet SHA256SUMS)

# Every file in the zip but SHA256SUMS is listed, and nothing else is: a file added to the zip
# after the sums were written would otherwise ship unchecked.
in_zip="$(unzip -Z1 "$zip" | LC_ALL=C sort | grep -vx SHA256SUMS)"
in_sums="$(awk '{ print $2 }' "$work/SHA256SUMS" | LC_ALL=C sort)"
if [ "$in_zip" != "$in_sums" ]; then
  echo "error: SHA256SUMS and the zip's files differ:" >&2
  diff <(printf '%s\n' "$in_sums") <(printf '%s\n' "$in_zip") >&2 || true
  exit 1
fi

for want in wordcraft_embed.js wordcraft_embed_bg.wasm wordcraft_embed.d.ts LICENSE-MIT LICENSE-APACHE NOTICE ATTRIBUTION.md THIRD-PARTY-LICENSES.txt; do
  [ -f "$work/$want" ] || { echo "error: the zip lacks $want" >&2; exit 1; }
done
echo "ok: $(basename "$zip") matches its checksums"
