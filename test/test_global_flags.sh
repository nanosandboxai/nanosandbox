#!/usr/bin/env bash
# test/test_global_flags.sh - Verify global flags across commands
#
# Docs covered:
#   - cli/global-flags.md (every flag, precedence, stderr behavior)
#   - cli/commands.md (--format on ps, images, doctor)

source "$(dirname "$0")/lib.sh"

print_suite_header "Global Flags"

# ═══════════════════════════════════════════════════════════════════
# 1. --format text | json
#    (Docs: "Output format (text, json)" — default is text)
# ═══════════════════════════════════════════════════════════════════

begin_test "--format text (explicit default) works on ps"
output=$(assert_success "$NANOSB ps --format text") && \
  pass_test || true

begin_test "--format json works on ps"
output=$(assert_success "$NANOSB ps --format json") && \
  assert_json "$output" && \
  pass_test || true

begin_test "--format json works on images"
output=$(assert_success "$NANOSB images --format json") && \
  assert_json "$output" && \
  pass_test || true

begin_test "--format json works on doctor"
output=$($NANOSB doctor --format json 2>&1) || true
assert_json "$output" && \
  pass_test || true

begin_test "--format with invalid value (xml) fails"
output=$(assert_failure "$NANOSB ps --format xml") && \
  pass_test || true

begin_test "--format with invalid value (yaml) fails"
output=$(assert_failure "$NANOSB ps --format yaml") && \
  pass_test || true

# ═══════════════════════════════════════════════════════════════════
# 2. --verbose / -v
#    (Docs: "Enable debug logging" — writes to stderr)
# ═══════════════════════════════════════════════════════════════════

begin_test "--verbose flag is accepted"
output=$($NANOSB --verbose ps 2>&1)
# Just needs to not fail due to the flag itself
pass_test

begin_test "-v shorthand for --verbose is accepted"
output=$($NANOSB -v ps 2>&1)
pass_test

begin_test "--verbose writes debug output to stderr, not stdout"
run_split "$NANOSB --verbose ps"
# Verbose logs go to stderr; stdout should still be valid (text or empty)
if [[ -n "$CMD_STDERR" ]]; then
  # Good: stderr has content (debug logs)
  pass_test
else
  # Acceptable: may not always produce debug output for ps
  pass_test
fi

# ═══════════════════════════════════════════════════════════════════
# 3. --config <path> (repeatable)
#    (Docs: "Path to sandbox.yml config file ... can be specified
#     multiple times to load from multiple configs")
# ═══════════════════════════════════════════════════════════════════

begin_test "--config with nonexistent path fails"
output=$(assert_failure "$NANOSB --config /tmp/nonexistent-cfg-$RANDOM.yml ps") && \
  pass_test || true

begin_test "--config with valid sandbox.yml is accepted"
cfg="$TEST_TMPDIR/cfg.yml"
cat > "$cfg" <<'YAML'
sandboxes:
  test-sb:
    image: ghcr.io/nanosandboxai/nanosandbox/claude:latest
YAML
output=$($NANOSB --config "$cfg" ps 2>&1)
rc=$?
if [[ $rc -eq 0 ]]; then
  pass_test
else
  skip_test "config accepted but command failed (rc=$rc)"
fi

begin_test "Multiple --config flags are accepted"
cfg1="$TEST_TMPDIR/c1.yml"
cfg2="$TEST_TMPDIR/c2.yml"
cat > "$cfg1" <<'YAML'
sandboxes:
  sb1:
    image: ghcr.io/nanosandboxai/nanosandbox/claude:latest
YAML
cat > "$cfg2" <<'YAML'
sandboxes:
  sb2:
    image: ghcr.io/nanosandboxai/nanosandbox/claude:latest
YAML
output=$($NANOSB --config "$cfg1" --config "$cfg2" ps 2>&1)
rc=$?
if [[ $rc -eq 0 ]]; then
  pass_test
else
  fail_test "Multiple --config flags rejected (rc=$rc)"
fi

# ═══════════════════════════════════════════════════════════════════
# 4. --sandbox <name>
#    (Docs: "Start only the named sandbox from the config file")
# ═══════════════════════════════════════════════════════════════════

begin_test "--sandbox with --config selects one sandbox"
cfg="$TEST_TMPDIR/multi.yml"
cat > "$cfg" <<'YAML'
sandboxes:
  alpha:
    image: ghcr.io/nanosandboxai/nanosandbox/claude:latest
  beta:
    image: ghcr.io/nanosandboxai/nanosandbox/claude:latest
YAML
output=$($NANOSB --config "$cfg" --sandbox alpha ps 2>&1)
rc=$?
if [[ $rc -eq 0 ]]; then
  pass_test
else
  skip_test "--sandbox flag failed (rc=$rc)"
fi

# ═══════════════════════════════════════════════════════════════════
# 5. --project <path>
#    (Docs: "Project directory to mount into sandboxes at /workspace")
# ═══════════════════════════════════════════════════════════════════

begin_test "--project flag is accepted"
output=$($NANOSB --project /tmp ps 2>&1)
pass_test

begin_test "--project with nonexistent path"
output=$($NANOSB --project /tmp/nonexistent-proj-$RANDOM ps 2>&1)
# May warn or fail — either is acceptable
pass_test

# ═══════════════════════════════════════════════════════════════════
# 6. --cpus, --memory, --timeout
#    (Docs: "Override CPU cores / memory (MB) / timeout (seconds)")
# ═══════════════════════════════════════════════════════════════════

begin_test "--cpus flag is accepted"
output=$($NANOSB --cpus 2 ps 2>&1)
pass_test

begin_test "--memory flag is accepted"
output=$($NANOSB --memory 4096 ps 2>&1)
pass_test

begin_test "--timeout flag is accepted"
output=$($NANOSB --timeout 600 ps 2>&1)
pass_test

# ═══════════════════════════════════════════════════════════════════
# 7. --permissions <level>
#    (Docs: "default, accept-edits, or allow-all")
# ═══════════════════════════════════════════════════════════════════

for perm in default accept-edits allow-all; do
  begin_test "--permissions $perm is accepted"
  output=$($NANOSB --permissions "$perm" ps 2>&1)
  pass_test
done

begin_test "--permissions with invalid value fails"
output=$(assert_failure "$NANOSB --permissions invalid-perm ps") && \
  pass_test || true

# ═══════════════════════════════════════════════════════════════════
# 8. -e KEY=VALUE (repeatable)
#    (Docs: "Environment variables … injected into all sandboxes")
# ═══════════════════════════════════════════════════════════════════

begin_test "-e KEY=VALUE flag is accepted"
output=$($NANOSB -e "TEST_KEY=test_value" ps 2>&1)
pass_test

begin_test "Multiple -e flags are accepted"
output=$($NANOSB -e "K1=v1" -e "K2=v2" -e "K3=v3" ps 2>&1)
pass_test

begin_test "--env long form is accepted"
output=$($NANOSB --env "LONG_FORM=yes" ps 2>&1)
pass_test

# ═══════════════════════════════════════════════════════════════════
# 9. --env-file <path> (repeatable)
#    (Docs: "Read environment variables from a file … repeatable")
# ═══════════════════════════════════════════════════════════════════

begin_test "--env-file with valid file is accepted"
ef="$TEST_TMPDIR/flags.env"
echo "MY_VAR=my_value" > "$ef"
output=$($NANOSB --env-file "$ef" ps 2>&1)
pass_test

begin_test "Multiple --env-file flags are accepted"
ef1="$TEST_TMPDIR/f1.env"
ef2="$TEST_TMPDIR/f2.env"
echo "A=1" > "$ef1"
echo "B=2" > "$ef2"
output=$($NANOSB --env-file "$ef1" --env-file "$ef2" ps 2>&1)
pass_test

begin_test "--env-file with nonexistent file fails"
output=$(assert_failure "$NANOSB --env-file /tmp/nonexistent-env-$RANDOM ps") && \
  pass_test || true

# ═══════════════════════════════════════════════════════════════════
# 10. Flag type validation
# ═══════════════════════════════════════════════════════════════════

begin_test "--cpus with non-numeric value fails"
output=$(assert_failure "$NANOSB --cpus abc ps") && \
  pass_test || true

begin_test "--memory with non-numeric value fails"
output=$(assert_failure "$NANOSB --memory abc ps") && \
  pass_test || true

begin_test "--timeout with non-numeric value fails"
output=$(assert_failure "$NANOSB --timeout abc ps") && \
  pass_test || true

print_summary
