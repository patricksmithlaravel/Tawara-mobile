#!/usr/bin/env bash
# The build number of the phone builds (Tawara-wallet's docs/RELEASE.md section
# 5.2). It becomes iOS's CFBundleVersion and Android's versionCode.
#
# Both stores refuse a build number they have seen. So every upload to either
# store takes the next number: the counter in platform/build-number is raised by
# one for each upload, and a rebuilt upload takes a new number too. The counter
# holds the number of the most recent upload, 0 before the first. It is
# committed, so that a release commit and its tag say which number their builds
# carry. docs/RELEASE.md names the integer but not the file that holds it, so
# where the counter lives is a choice awaiting the owner.
#
# Debug and CI builds take the fixed development number, BN_DEV, as they always
# have. A store build sets TAWARA_BUILD_NUMBER to the committed counter, so the
# release step raises the counter first (`bump`), commits it, tags that commit,
# and only then builds from the tag.
#
#   platform/build-number.sh counter        the committed counter
#   platform/build-number.sh next           the number the next upload takes
#   platform/build-number.sh check NEW OLD  exit 0 only if NEW is OLD plus one
#   platform/build-number.sh bump NEW       record NEW as the counter, if check accepts it
#   platform/build-number.sh number         the number a build takes now
#
# platform/ios/tawara.sh and platform/android/tawara.sh source this file and take
# their number from `number`. It is bash 3.2 code, as those scripts are: they run
# on the owner's Mac, where /bin/bash is that version.

BN_DEV=1
# The highest versionCode Google Play accepts. Both platforms stop at it here.
BN_MAX=2100000000
BN_DIR=$(unset CDPATH; cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
BN_FILE=$BN_DIR/build-number

# bn_integer TEXT: TEXT is a plain decimal integer, digits only with no sign and
# no leading zero, of at most ten digits. The digits are listed rather than given
# as a range, so that no locale can admit another character.
bn_integer() {
  case ${1:-} in
    '' | *[!0123456789]* | 0?*) return 1 ;;
  esac
  [ "${#1}" -le 10 ]
}

# bn_counter: prints the committed counter, a plain integer from 0 to BN_MAX on
# the only line of the file.
bn_counter() {
  local value lines
  if [ ! -f "$BN_FILE" ]; then
    echo "::error::$BN_FILE is missing" >&2
    return 1
  fi
  lines=$(wc -l <"$BN_FILE" | tr -d ' ')
  value=$(sed -n '1p' "$BN_FILE")
  if [ "$lines" != 1 ] || ! bn_integer "$value" || [ "$value" -gt "$BN_MAX" ]; then
    echo "::error::$BN_FILE must hold one line, a build number from 0 to $BN_MAX" >&2
    return 1
  fi
  printf '%s\n' "$value"
}

# bn_check NEW OLD: exit 0 only if NEW is OLD plus one, both plain integers in
# range. So the counter rises by one for each upload, and no number is repeated,
# lowered or skipped.
bn_check() {
  local new=${1:-} old=${2:-}
  if ! bn_integer "$old" || [ "$old" -gt "$BN_MAX" ]; then
    echo "::error::'$old' is not a counter value" >&2
    return 1
  fi
  if ! bn_integer "$new" || [ "$new" -lt 1 ] || [ "$new" -gt "$BN_MAX" ]; then
    echo "::error::'$new' is not a build number from 1 to $BN_MAX" >&2
    return 1
  fi
  if [ "$new" -ne $((old + 1)) ]; then
    echo "::error::build number $new does not follow $old: each upload takes the number after the last" >&2
    return 1
  fi
}

# bn_next: prints the number the next upload takes.
bn_next() {
  local counter
  counter=$(bn_counter) || return 1
  if [ "$counter" -ge "$BN_MAX" ]; then
    echo "::error::the counter is at $BN_MAX, the highest build number allowed" >&2
    return 1
  fi
  printf '%s\n' $((counter + 1))
}

# bn_bump NEW: records NEW as the counter, when bn_check accepts it against the
# counter as committed. The release step calls it, then commits the file.
bn_bump() {
  local counter tmp
  counter=$(bn_counter) || return 1
  bn_check "${1:-}" "$counter" || return 1
  tmp=$BN_FILE.new
  printf '%s\n' "$1" >"$tmp" && mv "$tmp" "$BN_FILE"
}

# bn_number: prints the number this build takes. With TAWARA_BUILD_NUMBER unset,
# that is the development number. Otherwise it must be TAWARA_BUILD_NUMBER, and
# that must be the committed counter: a store build is made only after its number
# is raised and committed.
bn_number() {
  local counter
  if [ -z "${TAWARA_BUILD_NUMBER:-}" ]; then
    printf '%s\n' "$BN_DEV"
    return 0
  fi
  counter=$(bn_counter) || return 1
  if ! bn_integer "$TAWARA_BUILD_NUMBER" || [ "$TAWARA_BUILD_NUMBER" -lt 1 ] \
    || [ "$TAWARA_BUILD_NUMBER" != "$counter" ]; then
    echo "::error::TAWARA_BUILD_NUMBER is '$TAWARA_BUILD_NUMBER' but $BN_FILE holds '$counter': a store build takes the committed counter, so raise it with 'bump' and commit it first" >&2
    return 1
  fi
  printf '%s\n' "$TAWARA_BUILD_NUMBER"
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  case ${1:-} in
    counter) bn_counter ;;
    next) bn_next ;;
    check) bn_check "${2:-}" "${3:-}" ;;
    bump) bn_bump "${2:-}" ;;
    number) bn_number ;;
    *)
      echo "usage: $0 counter|next|check NEW OLD|bump NEW|number" >&2
      exit 2
      ;;
  esac
fi
