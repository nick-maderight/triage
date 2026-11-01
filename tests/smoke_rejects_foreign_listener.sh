#!/bin/sh
set -eu

tool=${0##*/}
case "$tool" in
  ss)
    printf 'LISTEN 0 128 127.0.0.1:7391 0.0.0.0:* users:(("other",pid=%s,fd=3))\n' "$MOCK_UNRELATED_PID"
    exit 0
    ;;
  curl|sleep)
    exit 0
    ;;
  triage)
    if [ "${1-}" = serve ]; then
      printf '%s\n' "$*" >> "$MOCK_LOG"
      exec /bin/sleep 30
    fi
    printf '%s\n' "$*" >> "$MOCK_LOG"
    exit 97
    ;;
esac

SCRIPT=$(readlink -f "$0")
TEST_DIR=$(mktemp -d "${TMPDIR:-/tmp}/triage-smoke-regression.XXXXXX")
cleanup() {
  rm -rf "$TEST_DIR"
}
trap cleanup EXIT HUP INT TERM
mkdir "$TEST_DIR/bin"
for tool in ss curl sleep triage; do
  ln -s "$SCRIPT" "$TEST_DIR/bin/$tool"
done
MOCK_LOG=$TEST_DIR/triage.log
MOCK_UNRELATED_PID=$$
if TRIAGE_BIN="$TEST_DIR/bin/triage" MOCK_LOG="$MOCK_LOG" MOCK_UNRELATED_PID="$MOCK_UNRELATED_PID" PATH="$TEST_DIR/bin:$PATH" TMPDIR="$TEST_DIR" sh "$(dirname "$SCRIPT")/smoke.sh" >"$TEST_DIR/smoke.out" 2>&1; then
  printf '%s\n' 'smoke accepted an unrelated listener' >&2
  exit 1
fi

calls=0
serve_args=
while IFS= read -r call; do
  calls=$((calls + 1))
  if [ "$calls" -gt 1 ]; then
    printf '%s\n' 'smoke issued CLI commands through an unrelated listener' >&2
    exit 1
  fi
  serve_args=$call
done < "$MOCK_LOG"
if [ "$calls" -ne 1 ]; then
  printf '%s\n' 'smoke did not launch its server' >&2
  exit 1
fi
case "$serve_args" in
  "serve --db $TEST_DIR"/triage-smoke.*/triage.db" --listen 127.0.0.1:7391 --no-notify")
    ;;
  *)
    printf '%s\n' 'smoke server did not use a per-run database' >&2
    exit 1
    ;;
esac
if ! grep -Fq 'server did not become ready' "$TEST_DIR/smoke.out"; then
  printf '%s\n' 'smoke did not reach bounded readiness failure' >&2
  exit 1
fi
