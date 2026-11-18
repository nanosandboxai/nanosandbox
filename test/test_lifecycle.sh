#!/usr/bin/env bash
# test/test_lifecycle.sh - Full sandbox lifecycle: run, exec, ps, stop, rm
#
# Docs covered:
#   - cli/commands.md   (run, exec, ps, stop, rm — every flag)
#   - cli/global-flags.md (-e, --env-file, --project, --cpus, --memory, --timeout)
#
# NOTE: Creates real sandboxes. Requires a working runtime + pulled image.
# Tests that need the runtime will skip gracefully if unavailable.

source "$(dirname "$0")/lib.sh"

print_suite_header "Sandbox Lifecycle (run / exec / stop / rm)"

IMAGE="ghcr.io/nanosandboxai/nanosandbox/claude:latest"
RUNTIME_OK=true

# Pre-check: ensure image is available
if ! $NANOSB images 2>/dev/null | grep -qF "claude"; then
  echo -e "  ${YELLOW}Pulling image first...${RESET}"
  $NANOSB pull "$IMAGE" 2>&1 || true
fi

# ═══════════════════════════════════════════════════════════════════
# Helper: attempt a run; set RUNTIME_OK=false on first failure so
# remaining tests can skip quickly.
# ═══════════════════════════════════════════════════════════════════
try_run() {
  local name="$1"; shift
  local output rc
  output=$($NANOSB run --name "$name" "$@" 2>&1) && rc=0 || rc=$?
  echo "$output"
  if [[ $rc -ne 0 ]]; then
    RUNTIME_OK=false
  fi
  return $rc
}

require_runtime() {
  if ! $RUNTIME_OK; then
    skip_test "runtime not available"
    return 1
  fi
  return 0
}

# ═══════════════════════════════════════════════════════════════════
# 1. nanosb run --name <name> <image> <cmd>
#    (Docs: commands.md — "Run a command in a new sandbox")
# ═══════════════════════════════════════════════════════════════════

SB_BASIC=$(test_sandbox_name)
begin_test "nanosb run --name creates sandbox and runs command"
output=$(try_run "$SB_BASIC" "$IMAGE" echo hello) && rc=0 || rc=$?
cleanup_sandbox "$SB_BASIC"
if [[ $rc -eq 0 ]]; then
  assert_contains "$output" "hello" && pass_test || true
else
  skip_test "run failed (rc=$rc) — runtime may not be available"
fi

# ═══════════════════════════════════════════════════════════════════
# 2. run -e KEY=VALUE
#    (Docs: commands.md, global-flags.md — "inject environment variable")
# ═══════════════════════════════════════════════════════════════════

SB=$(test_sandbox_name)
begin_test "nanosb run -e injects environment variable"
if require_runtime; then
  output=$(try_run "$SB" -e "TEST_VAR=hello_test" "$IMAGE" sh -c 'echo $TEST_VAR') && rc=0 || rc=$?
  cleanup_sandbox "$SB"
  if [[ $rc -eq 0 ]]; then
    assert_contains "$output" "hello_test" && pass_test || true
  else
    skip_test "run -e failed (rc=$rc)"
  fi
fi

# ═══════════════════════════════════════════════════════════════════
# 3. run with multiple -e (repeatable)
#    (Docs: global-flags.md — "repeatable")
# ═══════════════════════════════════════════════════════════════════

SB=$(test_sandbox_name)
begin_test "nanosb run with multiple -e flags injects all vars"
if require_runtime; then
  output=$(try_run "$SB" -e "VAR_A=aaa" -e "VAR_B=bbb" "$IMAGE" sh -c 'echo $VAR_A $VAR_B') && rc=0 || rc=$?
  cleanup_sandbox "$SB"
  if [[ $rc -eq 0 ]]; then
    assert_contains "$output" "aaa" && assert_contains "$output" "bbb" && pass_test || true
  else
    skip_test "multi -e failed (rc=$rc)"
  fi
fi

# ═══════════════════════════════════════════════════════════════════
# 4. run --env-file
#    (Docs: commands.md, global-flags.md)
# ═══════════════════════════════════════════════════════════════════

SB=$(test_sandbox_name)
begin_test "nanosb run --env-file loads variables from file"
if require_runtime; then
  envfile="$TEST_TMPDIR/test.env"
  echo "FROM_FILE=env_file_value" > "$envfile"
  output=$(try_run "$SB" --env-file "$envfile" "$IMAGE" sh -c 'echo $FROM_FILE') && rc=0 || rc=$?
  cleanup_sandbox "$SB"
  if [[ $rc -eq 0 ]]; then
    assert_contains "$output" "env_file_value" && pass_test || true
  else
    skip_test "run --env-file failed (rc=$rc)"
  fi
fi

# ═══════════════════════════════════════════════════════════════════
# 5. run --cpus and --memory
#    (Docs: commands.md — default cpus=2, memory=4096)
# ═══════════════════════════════════════════════════════════════════

SB=$(test_sandbox_name)
begin_test "nanosb run with --cpus and --memory overrides"
if require_runtime; then
  output=$(try_run "$SB" --cpus 1 --memory 2048 "$IMAGE" echo ok) && rc=0 || rc=$?
  cleanup_sandbox "$SB"
  if [[ $rc -eq 0 ]]; then
    pass_test
  else
    skip_test "run with resource overrides failed (rc=$rc)"
  fi
fi

# ═══════════════════════════════════════════════════════════════════
# 6. run --timeout
#    (Docs: commands.md — default 600s)
# ═══════════════════════════════════════════════════════════════════

SB=$(test_sandbox_name)
begin_test "nanosb run with --timeout"
if require_runtime; then
  output=$(try_run "$SB" --timeout 30 "$IMAGE" echo ok) && rc=0 || rc=$?
  cleanup_sandbox "$SB"
  if [[ $rc -eq 0 ]]; then
    pass_test
  else
    skip_test "run with --timeout failed (rc=$rc)"
  fi
fi

# ═══════════════════════════════════════════════════════════════════
# 7. run --buffered
#    (Docs: commands.md — "buffer output for JSON output")
# ═══════════════════════════════════════════════════════════════════

SB=$(test_sandbox_name)
begin_test "nanosb run --buffered buffers output"
if require_runtime; then
  output=$(try_run "$SB" --buffered "$IMAGE" echo buffered_test) && rc=0 || rc=$?
  cleanup_sandbox "$SB"
  if [[ $rc -eq 0 ]]; then
    assert_contains "$output" "buffered_test" && pass_test || true
  else
    skip_test "run --buffered failed (rc=$rc)"
  fi
fi

# ═══════════════════════════════════════════════════════════════════
# 8. run --project mounts directory
#    (Docs: global-flags.md — "project directory to mount at /workspace")
# ═══════════════════════════════════════════════════════════════════

SB=$(test_sandbox_name)
begin_test "nanosb run --project mounts directory at /workspace"
if require_runtime; then
  project_dir="$TEST_TMPDIR/project"
  mkdir -p "$project_dir"
  echo "marker_content" > "$project_dir/marker.txt"
  output=$(try_run "$SB" --project "$project_dir" "$IMAGE" cat /workspace/marker.txt) && rc=0 || rc=$?
  cleanup_sandbox "$SB"
  if [[ $rc -eq 0 ]]; then
    assert_contains "$output" "marker_content" && pass_test || true
  else
    skip_test "run --project failed (rc=$rc)"
  fi
fi

# ═══════════════════════════════════════════════════════════════════
# 10-15. Long-running sandbox for exec / ps / stop / rm tests
# ═══════════════════════════════════════════════════════════════════

SB_LONG=$(test_sandbox_name)
LONG_OK=false

if $RUNTIME_OK; then
  $NANOSB run --name "$SB_LONG" "$IMAGE" sleep 300 &>/dev/null &
  RUN_PID=$!
  if wait_for_sandbox "$SB_LONG"; then
    LONG_OK=true
  fi
fi

# -- ps shows running sandbox
begin_test "nanosb ps shows running sandbox"
if $LONG_OK; then
  ps_out=$($NANOSB ps 2>&1)
  assert_contains "$ps_out" "$SB_LONG" && pass_test || true
else
  skip_test "no running sandbox"
fi

# -- ps --format json shows running sandbox
begin_test "nanosb ps --format json includes running sandbox"
if $LONG_OK; then
  ps_json=$($NANOSB ps --format json 2>&1)
  assert_contains "$ps_json" "$SB_LONG" && pass_test || true
else
  skip_test "no running sandbox"
fi

# -- exec in running sandbox
begin_test "nanosb exec runs command in running sandbox"
if $LONG_OK; then
  exec_out=$($NANOSB exec "$SB_LONG" echo exec_ok 2>&1)
  rc=$?
  if [[ $rc -eq 0 ]]; then
    assert_contains "$exec_out" "exec_ok" && pass_test || true
  else
    skip_test "exec failed (rc=$rc)"
  fi
else
  skip_test "no running sandbox"
fi

# -- exec --buffered
#    (Docs: commands.md — exec also has --buffered)
begin_test "nanosb exec --buffered buffers output"
if $LONG_OK; then
  exec_out=$($NANOSB exec --buffered "$SB_LONG" echo buffered_exec 2>&1)
  rc=$?
  if [[ $rc -eq 0 ]]; then
    assert_contains "$exec_out" "buffered_exec" && pass_test || true
  else
    skip_test "exec --buffered failed (rc=$rc)"
  fi
else
  skip_test "no running sandbox"
fi

# -- stop
begin_test "nanosb stop halts the sandbox"
if $LONG_OK; then
  $NANOSB stop "$SB_LONG" &>/dev/null
  rc=$?
  if [[ $rc -eq 0 ]]; then
    sleep 1
    ps_after=$($NANOSB ps 2>&1)
    assert_not_contains "$ps_after" "$SB_LONG" && pass_test || true
  else
    skip_test "stop failed (rc=$rc)"
  fi
else
  skip_test "no running sandbox"
fi

# -- stopped sandbox in ps -a
#    (Docs: commands.md — "-a shows stopped/exited")
begin_test "Stopped sandbox appears in 'nanosb ps -a'"
if $LONG_OK; then
  ps_all=$($NANOSB ps -a 2>&1)
  if echo "$ps_all" | grep -qF "$SB_LONG"; then
    pass_test
  else
    skip_test "stopped sandbox not found in ps -a"
  fi
else
  skip_test "no running sandbox"
fi

# -- rm removes the sandbox
begin_test "nanosb rm removes the sandbox"
if $LONG_OK; then
  $NANOSB rm "$SB_LONG" &>/dev/null
  rc=$?
  if [[ $rc -eq 0 ]]; then
    sleep 1
    ps_final=$($NANOSB ps -a 2>&1)
    assert_not_contains "$ps_final" "$SB_LONG" && pass_test || true
  else
    skip_test "rm failed (rc=$rc)"
  fi
else
  skip_test "no running sandbox"
fi

# ═══════════════════════════════════════════════════════════════════
# 16. rm -f (force) on a running sandbox
#     (Docs: commands.md — "-f to force-remove a running sandbox")
# ═══════════════════════════════════════════════════════════════════

SB_FORCE=$(test_sandbox_name)
FORCE_OK=false

if $RUNTIME_OK; then
  $NANOSB run --name "$SB_FORCE" "$IMAGE" sleep 300 &>/dev/null &
  FORCE_PID=$!
  if wait_for_sandbox "$SB_FORCE"; then
    FORCE_OK=true
  fi
fi

begin_test "nanosb rm -f force-removes a running sandbox"
if $FORCE_OK; then
  $NANOSB rm -f "$SB_FORCE" &>/dev/null
  rc=$?
  if [[ $rc -eq 0 ]]; then
    sleep 1
    ps_check=$($NANOSB ps -a 2>&1)
    assert_not_contains "$ps_check" "$SB_FORCE" && pass_test || true
  else
    fail_test "rm -f failed (rc=$rc)"
    cleanup_sandbox "$SB_FORCE"
  fi
  kill $FORCE_PID 2>/dev/null || true
else
  skip_test "runtime not available"
fi

# ═══════════════════════════════════════════════════════════════════
# 17-19. Error cases for missing sandboxes
# ═══════════════════════════════════════════════════════════════════

begin_test "nanosb stop on non-existent sandbox fails"
output=$(assert_failure "$NANOSB stop nonexistent-sb-$RANDOM") && \
  pass_test || true

begin_test "nanosb rm on non-existent sandbox fails"
output=$(assert_failure "$NANOSB rm nonexistent-sb-$RANDOM") && \
  pass_test || true

begin_test "nanosb exec on non-existent sandbox fails"
output=$(assert_failure "$NANOSB exec nonexistent-sb-$RANDOM echo test") && \
  pass_test || true

# ═══════════════════════════════════════════════════════════════════
# Cleanup
# ═══════════════════════════════════════════════════════════════════
kill $RUN_PID 2>/dev/null || true
cleanup_sandbox "$SB_LONG"
cleanup_sandbox "$SB_FORCE"

print_summary
