#!/usr/bin/env bash
# Self-test for verify-zip.sh:  packaging/embed/test-verify-zip.sh [<real zip>]
#
# Builds crafted zips in a temporary folder and asserts that verify-zip.sh refuses each one, with
# the reason it should give, and accepts a well-formed one (and the real zip, when it is given).
# Needs: python3 (writes the zips; Python's zipfile can store names that `zip` would not), unzip.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VERIFY="$HERE/verify-zip.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

hash_file() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

# make_zip <mutation>  ->  $WORK/<mutation>/wordcraft-embed-0.0.0.zip (+ .sha256 as a well-formed one)
make_zip() {
  local mutation="$1" dir="$WORK/$1"
  mkdir -p "$dir"
  python3 -I - "$dir/wordcraft-embed-0.0.0.zip" "$mutation" <<'PY'
import hashlib, sys, zipfile

out, mutation = sys.argv[1], sys.argv[2]
names = [
    "wordcraft_embed.js", "wordcraft_embed_bg.wasm", "wordcraft_embed.d.ts",
    "LICENSE-MIT", "LICENSE-APACHE", "NOTICE", "ATTRIBUTION.md", "THIRD-PARTY-LICENSES.txt",
    "licenses/CC0-Compact-ICC-Profiles.txt", "licenses/LICENSE-Hack.txt",
    "licenses/LICENSE-emoji-icon-font.txt", "licenses/OFL-Inter.txt",
    "licenses/OFL-JetBrainsMono.txt", "licenses/OFL-NotoEmoji.txt", "licenses/OFL-SourceSans3.txt",
    "licenses/OFL-SourceSerif4.txt", "licenses/UFL-Ubuntu.txt",
]
files = {name: ("content of " + name + "\n").encode() for name in names}  # entry name -> bytes
listed = dict(files)  # what SHA256SUMS lists
sums_extra = ""
order = list(files)

if mutation == "traversal":
    # The attack: a name that climbs out of the extraction folder, listed with the hash of the
    # real file it would reach.
    files["../../etc/hosts"] = listed["../../etc/hosts"] = b"127.0.0.1 localhost\n"
elif mutation == "absolute":
    files["/etc/hosts"] = listed["/etc/hosts"] = b"127.0.0.1 localhost\n"
elif mutation == "dotdot-inside":
    files["licenses/../NOTICE2"] = listed["licenses/../NOTICE2"] = b"x\n"
elif mutation == "backslash":
    files["licenses\\evil.txt"] = listed["licenses\\evil.txt"] = b"x\n"
elif mutation == "folder-entry":
    files["licenses/"] = b""
elif mutation == "extra-listed":
    files["extra.txt"] = listed["extra.txt"] = b"extra\n"
elif mutation == "extra-unlisted":
    files["extra.txt"] = b"extra\n"
elif mutation == "name-with-space":
    files["a b.txt"] = listed["a b.txt"] = b"x\n"
elif mutation == "missing-file":
    del files["licenses/OFL-Inter.txt"], listed["licenses/OFL-Inter.txt"]
    order.remove("licenses/OFL-Inter.txt")
elif mutation == "listed-but-absent":
    del files["NOTICE"]
    order.remove("NOTICE")
elif mutation == "bad-entry-hash":
    files["wordcraft_embed_bg.wasm"] = b"tampered after the sums were written\n"
elif mutation == "unlisted-file":
    del listed["NOTICE"]
elif mutation == "sums-twice":
    sums_extra = hashlib.sha256(listed["NOTICE"]).hexdigest() + "  NOTICE\n"
elif mutation == "no-sums":
    pass
elif mutation == "sums-malformed":
    sums_extra = "not a checksum line\n"
elif mutation == "duplicate-entry":
    pass  # added below
elif mutation == "craft-fonts-ok":
    files["licenses/OFL-Some-Family.txt"] = listed["licenses/OFL-Some-Family.txt"] = b"family\n"
elif mutation == "binary-marker-ok":
    pass  # SHA256SUMS written below with ' *' markers
elif mutation != "ok":
    sys.exit("unknown mutation " + mutation)

lines = []
for name in sorted(listed):
    marker = " *" if mutation == "binary-marker-ok" else "  "
    lines.append(hashlib.sha256(listed[name]).hexdigest() + marker + name + "\n")
sums = "".join(lines) + sums_extra

with zipfile.ZipFile(out, "w") as archive:
    for name in sorted(files):
        info = zipfile.ZipInfo(name, (1980, 1, 1, 0, 0, 0))
        archive.writestr(info, files[name])
    if mutation != "no-sums":
        archive.writestr(zipfile.ZipInfo("SHA256SUMS", (1980, 1, 1, 0, 0, 0)), sums)
    if mutation == "duplicate-entry":
        import warnings
        with warnings.catch_warnings():
            warnings.simplefilter("ignore")
            archive.writestr(zipfile.ZipInfo("NOTICE", (1980, 1, 1, 0, 0, 0)), files["NOTICE"])
PY
  write_sha256 "$dir/wordcraft-embed-0.0.0.zip"
}

# The .sha256 a release writes: one line, the zip's hash and its own file name.
write_sha256() {
  printf '%s  %s\n' "$(hash_file "$1")" "$(basename "$1")" >"$1.sha256"
}

FAILURES=0
expect_ok() {
  local label="$1" zip="$2" output
  if output="$(bash "$VERIFY" "$zip" 2>&1)"; then
    echo "pass: $label"
  else
    echo "FAIL: $label should be accepted, but: $output" >&2
    FAILURES=$((FAILURES + 1))
  fi
}
expect_refused() {
  local label="$1" zip="$2" reason="$3" output
  if output="$(bash "$VERIFY" "$zip" 2>&1)"; then
    echo "FAIL: $label was accepted" >&2
    FAILURES=$((FAILURES + 1))
  elif [[ "$output" != *"$reason"* ]]; then
    echo "FAIL: $label was refused, but not for '$reason': $output" >&2
    FAILURES=$((FAILURES + 1))
  else
    echo "pass: $label refused ($reason)"
  fi
}

zip_of() { echo "$WORK/$1/wordcraft-embed-0.0.0.zip"; }

for mutation in ok craft-fonts-ok binary-marker-ok; do
  make_zip "$mutation"
  expect_ok "$mutation" "$(zip_of "$mutation")"
done

refusal() { # <mutation> <reason>
  make_zip "$1"
  expect_refused "$1" "$(zip_of "$1")" "$2"
}
refusal traversal "has a '.', '..' or empty path component"
refusal dotdot-inside "has a '.', '..' or empty path component"
refusal absolute "is an absolute path"
refusal backslash "holds a backslash"
refusal folder-entry "is a folder entry"
refusal name-with-space "has characters outside"
refusal extra-listed "not part of the embed build"
refusal extra-unlisted "not part of the embed build"
refusal missing-file "the zip lacks: licenses/OFL-Inter.txt"
refusal listed-but-absent "the zip lacks: NOTICE"
refusal bad-entry-hash "wordcraft_embed_bg.wasm does not match SHA256SUMS"
refusal unlisted-file "SHA256SUMS and the zip's files differ"
refusal sums-twice "SHA256SUMS lists a file twice"
refusal sums-malformed "SHA256SUMS has a line that is not"
refusal no-sums "the zip lacks: SHA256SUMS"
refusal duplicate-entry "lists an entry twice"

# The .sha256 file itself: it must name this zip, once, with this zip's hash.
make_zip ok
good_zip="$(zip_of ok)"
sha_dir="$WORK/sha"
mkdir -p "$sha_dir"
cp "$good_zip" "$sha_dir/wordcraft-embed-0.0.0.zip"
cp "$good_zip" "$sha_dir/other.zip"
bad_zip="$sha_dir/wordcraft-embed-0.0.0.zip"

# Names another file of the folder, with that file's real hash: sha256sum -c would pass it.
printf '%s  %s\n' "$(hash_file "$sha_dir/other.zip")" other.zip >"$bad_zip.sha256"
expect_refused ".sha256 naming another file" "$bad_zip" "names 'other.zip', not wordcraft-embed-0.0.0.zip"

printf '%s  %s\n%s  %s\n' "$(hash_file "$bad_zip")" wordcraft-embed-0.0.0.zip "$(hash_file "$bad_zip")" other.zip >"$bad_zip.sha256"
expect_refused ".sha256 with two lines" "$bad_zip" "has 2 lines"

: >"$bad_zip.sha256"
expect_refused "empty .sha256" "$bad_zip" "has 0 lines"

printf '%064d  %s\n' 0 wordcraft-embed-0.0.0.zip >"$bad_zip.sha256"
expect_refused ".sha256 with another hash" "$bad_zip" "does not match its .sha256"

printf 'garbage\n' >"$bad_zip.sha256"
expect_refused "malformed .sha256" "$bad_zip" "is not '<sha256>  <file name>'"

rm "$bad_zip.sha256"
expect_refused "missing .sha256" "$bad_zip" "wordcraft-embed-0.0.0.zip.sha256 is missing"

if [ -n "${1:-}" ]; then
  expect_ok "the real zip $(basename "$1")" "$1"
fi

if [ "$FAILURES" -ne 0 ]; then
  echo "$FAILURES check(s) failed" >&2
  exit 1
fi
echo "ok: verify-zip.sh refuses every crafted zip and accepts the good ones"
