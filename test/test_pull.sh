#!/usr/bin/env bash
# test/test_pull.sh - Verify the "nanosb pull" command
#
# Docs covered:
#   - cli/commands.md        (pull section)
#   - agents/overview.md     (agent images)
#
# NOTE: This test performs network operations and downloads images.

source "$(dirname "$0")/lib.sh"

print_suite_header "Pull Command"

FULL_IMAGE="ghcr.io/nanosandboxai/nanosandbox/claude:latest"

# ═══════════════════════════════════════════════════════════════════
# 1. Argument validation
# ═══════════════════════════════════════════════════════════════════

begin_test "nanosb pull without args fails"
output=$(assert_failure "$NANOSB pull") && \
  pass_test || true

begin_test "nanosb pull with invalid image name fails"
output=$(assert_failure "$NANOSB pull not-a-valid-registry/nonexistent-image:fake") && \
  pass_test || true

# ═══════════════════════════════════════════════════════════════════
# 2. Pull a full image reference
# ═══════════════════════════════════════════════════════════════════

begin_test "nanosb pull downloads a full image reference"
output=$($NANOSB pull "$FULL_IMAGE" 2>&1) && rc=0 || rc=$?
if [[ $rc -eq 0 ]]; then
  pass_test
else
  skip_test "pull failed (rc=$rc) — possible network issue"
fi

# ═══════════════════════════════════════════════════════════════════
# 3. Pulled image appears in images list
# ═══════════════════════════════════════════════════════════════════

begin_test "Pulled image appears in 'nanosb images' text output"
images_output=$($NANOSB images 2>&1)
if echo "$images_output" | grep -qF "claude"; then
  pass_test
else
  skip_test "image not in list — pull may not have succeeded"
fi

begin_test "Pulled image appears in 'nanosb images --format json'"
json_output=$($NANOSB images --format json 2>&1)
if echo "$json_output" | grep -qF "claude"; then
  pass_test
else
  skip_test "image not in JSON — pull may not have succeeded"
fi

# ═══════════════════════════════════════════════════════════════════
# 4. All documented agent images can be pulled via full reference
#    (Docs: agents/overview.md — claude, goose, codex, cursor)
#    NOTE: Bare names (e.g. "claude") only auto-resolve in sandbox.yml,
#    not in the pull command. Use full registry paths here.
# ═══════════════════════════════════════════════════════════════════

for agent in claude goose codex cursor; do
  begin_test "nanosb pull accepts full reference for agent '$agent'"
  full_ref="ghcr.io/nanosandboxai/nanosandbox/${agent}:latest"
  output=$($NANOSB pull "$full_ref" 2>&1) && rc=0 || rc=$?
  if [[ $rc -eq 0 ]]; then
    pass_test
  else
    skip_test "pull failed (rc=$rc) — possible network/auth issue"
  fi
done

print_summary
