#!/usr/bin/env bash
# The examples are processes, so the gate runs them as processes: one owner of these commands,
# called by both CI arms, so an assertion cannot drift apart between them. Why no test target can
# take the place of a row is stated once in docs/AGENTS.md, under "Verification expectations".
#
# Rows run the profile the job already builds. The durations a few of the printed lines carry are
# measurements, and no row asserts on one.
set -euo pipefail

arm="${1:?usage: ci-examples.sh linux|windows}"

echo '::group::the indexer hands four jobs to two worker processes'
out=$(cargo run -q --features os,tokio --example index -- Cargo.toml README.md docs/AGENTS.md absent)
test "$(grep -c $'\tok$' <<<"$out")" = 3
grep -q $'^3\t.*\tabsent' <<<"$out"
echo '::endgroup::'

echo '::group::the compile cache round-trips with a daemon across an exec, and reaps it'
out=$(cargo run -q --features os,tokio --example compile_cache)
test "$(grep -c '^ROUNDTRIP' <<<"$out")" = 9
grep -q '^LIVENESS' <<<"$out"
echo '::endgroup::'

echo '::group::the geometry probe classifies every range it names'
# Five rows are the whole claim on either host. The refusal one of them reports is a host's class
# ceiling, so it is printed on Windows and asserted in `src/tests/mock/talc.rs` on Linux, where the
# ceiling is a number the test can name rather than a token a row would pin.
out=$(cargo run -q --features os,tokio --example compile_cache -- --geometry)
test "$(grep -c '^GEOMETRY' <<<"$out")" = 5
if [ "$arm" = windows ]; then
  grep -q 'refused-class-too-wide-' <<<"$out"
fi
echo '::endgroup::'

echo '::group::the second introduction route reports where it stops'
# 0, 3 and 4 are the three verdicts this example is built to distinguish, and which of them a host
# gives is what the card is about; 1 is its own word for "somewhere the roles do not name", and a
# signal or harness code is not a verdict at all. So the row accepts the measurement and fails on
# the shape nothing recorded.
status=0
out=$(cargo run -q --features os,tokio --example second_introduction) || status=$?
case "$status" in
  0 | 3 | 4 | 6) grep -q $'^SECOND\tlauncher\tverdict:' <<<"$out" ;;
  *) exit "$status" ;;
esac
echo '::endgroup::'
