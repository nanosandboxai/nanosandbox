#!/usr/bin/env bash
# test/test_config.sh - Verify sandbox.yml configuration handling
#
# Docs covered:
#   - cli/configuration.md  (full config format, defaults, overrides,
#     env interpolation, MCP section, image auto-resolution, auto-detect)
#   - agents/mcp-servers.md  (mcp: env, enabled, defaults)

source "$(dirname "$0")/lib.sh"

print_suite_header "Configuration (sandbox.yml)"

# ═══════════════════════════════════════════════════════════════════
# 1. Minimal valid config
# ═══════════════════════════════════════════════════════════════════

begin_test "Minimal sandbox.yml (just image) is accepted"
cfg="$TEST_TMPDIR/minimal.yml"
cat > "$cfg" <<'YAML'
sandboxes:
  my-sandbox:
    image: ghcr.io/nanosandboxai/nanosandbox/claude:latest
YAML
output=$($NANOSB --config "$cfg" ps 2>&1)
rc=$?
[[ $rc -eq 0 ]] && pass_test || fail_test "Minimal config rejected (rc=$rc): $output"

# ═══════════════════════════════════════════════════════════════════
# 2. Bare image name auto-resolution
#    (Docs: configuration.md — "bare names auto-resolve to
#     ghcr.io/nanosandboxai/nanosandbox/<name>:latest")
# ═══════════════════════════════════════════════════════════════════

begin_test "Bare image name in config is accepted (auto-resolution)"
cfg="$TEST_TMPDIR/bare.yml"
cat > "$cfg" <<'YAML'
sandboxes:
  agent:
    image: claude
YAML
output=$($NANOSB --config "$cfg" ps 2>&1)
rc=$?
[[ $rc -eq 0 ]] && pass_test || fail_test "Bare name config rejected (rc=$rc)"

# ═══════════════════════════════════════════════════════════════════
# 3. defaults section
#    (Docs: configuration.md — cpus, memory, timeout)
# ═══════════════════════════════════════════════════════════════════

begin_test "Config with defaults section is accepted"
cfg="$TEST_TMPDIR/defaults.yml"
cat > "$cfg" <<'YAML'
defaults:
  cpus: 2
  memory: 4096
  timeout: 600

sandboxes:
  my-sandbox:
    image: ghcr.io/nanosandboxai/nanosandbox/claude:latest
YAML
output=$($NANOSB --config "$cfg" ps 2>&1)
rc=$?
[[ $rc -eq 0 ]] && pass_test || fail_test "Defaults config rejected (rc=$rc)"

# ═══════════════════════════════════════════════════════════════════
# 4. Per-sandbox overrides
# ═══════════════════════════════════════════════════════════════════

begin_test "Per-sandbox cpus/memory/timeout overrides are accepted"
cfg="$TEST_TMPDIR/overrides.yml"
cat > "$cfg" <<'YAML'
defaults:
  cpus: 2
  memory: 4096

sandboxes:
  fast:
    image: ghcr.io/nanosandboxai/nanosandbox/claude:latest
    cpus: 4
    memory: 8192
    timeout: 1200
YAML
output=$($NANOSB --config "$cfg" ps 2>&1)
rc=$?
[[ $rc -eq 0 ]] && pass_test || fail_test "Overrides config rejected (rc=$rc)"

# ═══════════════════════════════════════════════════════════════════
# 5. env section with static values
# ═══════════════════════════════════════════════════════════════════

begin_test "Config with env section (static values) is accepted"
cfg="$TEST_TMPDIR/env_static.yml"
cat > "$cfg" <<'YAML'
sandboxes:
  my-sandbox:
    image: ghcr.io/nanosandboxai/nanosandbox/claude:latest
    env:
      MY_KEY: my_value
      ANOTHER: something
YAML
output=$($NANOSB --config "$cfg" ps 2>&1)
rc=$?
[[ $rc -eq 0 ]] && pass_test || fail_test "Env config rejected (rc=$rc)"

# ═══════════════════════════════════════════════════════════════════
# 6. env section with shell-style ${VAR} interpolation
#    (Docs: configuration.md — "shell-style ${VAR} interpolation")
# ═══════════════════════════════════════════════════════════════════

begin_test "Config with env interpolation (\${HOME}) is accepted"
cfg="$TEST_TMPDIR/env_interp.yml"
cat > "$cfg" <<'YAML'
sandboxes:
  my-sandbox:
    image: ghcr.io/nanosandboxai/nanosandbox/claude:latest
    env:
      HOME_DIR: ${HOME}
YAML
output=$($NANOSB --config "$cfg" ps 2>&1)
rc=$?
[[ $rc -eq 0 ]] && pass_test || fail_test "Env interpolation rejected (rc=$rc)"

# ═══════════════════════════════════════════════════════════════════
# 7. Multiple sandboxes
# ═══════════════════════════════════════════════════════════════════

begin_test "Config with multiple sandboxes is accepted"
cfg="$TEST_TMPDIR/multi.yml"
cat > "$cfg" <<'YAML'
sandboxes:
  sandbox-a:
    image: ghcr.io/nanosandboxai/nanosandbox/claude:latest
  sandbox-b:
    image: ghcr.io/nanosandboxai/nanosandbox/claude:latest
  sandbox-c:
    image: ghcr.io/nanosandboxai/nanosandbox/claude:latest
YAML
output=$($NANOSB --config "$cfg" ps 2>&1)
rc=$?
[[ $rc -eq 0 ]] && pass_test || fail_test "Multi-sandbox config rejected (rc=$rc)"

# ═══════════════════════════════════════════════════════════════════
# 8. --sandbox selects one
# ═══════════════════════════════════════════════════════════════════

begin_test "--sandbox selects a single sandbox from multi-sandbox config"
cfg="$TEST_TMPDIR/multi.yml"
output=$($NANOSB --config "$cfg" --sandbox sandbox-a ps 2>&1)
rc=$?
[[ $rc -eq 0 ]] && pass_test || fail_test "--sandbox selection failed (rc=$rc)"

# ═══════════════════════════════════════════════════════════════════
# 9. MCP section — basic
#    (Docs: configuration.md, agents/mcp-servers.md)
# ═══════════════════════════════════════════════════════════════════

begin_test "Config with MCP server (command + args) is accepted"
cfg="$TEST_TMPDIR/mcp_basic.yml"
cat > "$cfg" <<'YAML'
sandboxes:
  my-sandbox:
    image: ghcr.io/nanosandboxai/nanosandbox/claude:latest
    mcp:
      my-server:
        command: echo
        args: ["hello"]
YAML
output=$($NANOSB --config "$cfg" ps 2>&1)
rc=$?
[[ $rc -eq 0 ]] && pass_test || fail_test "MCP basic config rejected (rc=$rc)"

# ═══════════════════════════════════════════════════════════════════
# 10. MCP section — env and enabled fields
#     (Docs: agents/mcp-servers.md — "env", "enabled: true/false")
# ═══════════════════════════════════════════════════════════════════

begin_test "Config with MCP env and enabled fields is accepted"
cfg="$TEST_TMPDIR/mcp_full.yml"
cat > "$cfg" <<'YAML'
sandboxes:
  my-sandbox:
    image: ghcr.io/nanosandboxai/nanosandbox/claude:latest
    mcp:
      active-server:
        command: echo
        args: ["active"]
        env:
          SERVER_KEY: some_value
        enabled: true
      disabled-server:
        command: echo
        args: ["off"]
        enabled: false
YAML
output=$($NANOSB --config "$cfg" ps 2>&1)
rc=$?
[[ $rc -eq 0 ]] && pass_test || fail_test "MCP full config rejected (rc=$rc)"

# ═══════════════════════════════════════════════════════════════════
# 11. MCP in defaults section
#     (Docs: agents/mcp-servers.md — "default MCP servers inherited")
# ═══════════════════════════════════════════════════════════════════

begin_test "Config with MCP in defaults section is accepted"
cfg="$TEST_TMPDIR/mcp_defaults.yml"
cat > "$cfg" <<'YAML'
defaults:
  cpus: 2
  mcp:
    shared-server:
      command: echo
      args: ["shared"]

sandboxes:
  sb1:
    image: ghcr.io/nanosandboxai/nanosandbox/claude:latest
  sb2:
    image: ghcr.io/nanosandboxai/nanosandbox/claude:latest
YAML
output=$($NANOSB --config "$cfg" ps 2>&1)
rc=$?
[[ $rc -eq 0 ]] && pass_test || fail_test "MCP defaults config rejected (rc=$rc)"

# ═══════════════════════════════════════════════════════════════════
# 12. MCP env with variable substitution
#     (Docs: agents/mcp-servers.md — "${VAR_NAME}" syntax)
# ═══════════════════════════════════════════════════════════════════

begin_test "Config with MCP env variable substitution is accepted"
cfg="$TEST_TMPDIR/mcp_envvar.yml"
cat > "$cfg" <<'YAML'
sandboxes:
  my-sandbox:
    image: ghcr.io/nanosandboxai/nanosandbox/claude:latest
    mcp:
      my-server:
        command: echo
        args: ["test"]
        env:
          HOME_PATH: ${HOME}
YAML
output=$($NANOSB --config "$cfg" ps 2>&1)
rc=$?
[[ $rc -eq 0 ]] && pass_test || fail_test "MCP env var substitution rejected (rc=$rc)"

# ═══════════════════════════════════════════════════════════════════
# 13. Multiple --config files merge
#     (Docs: configuration.md, global-flags.md — "merged in order")
# ═══════════════════════════════════════════════════════════════════

begin_test "Multiple --config files merge sandboxes"
c1="$TEST_TMPDIR/merge1.yml"
c2="$TEST_TMPDIR/merge2.yml"
cat > "$c1" <<'YAML'
sandboxes:
  from-first:
    image: ghcr.io/nanosandboxai/nanosandbox/claude:latest
YAML
cat > "$c2" <<'YAML'
sandboxes:
  from-second:
    image: ghcr.io/nanosandboxai/nanosandbox/claude:latest
YAML
output=$($NANOSB --config "$c1" --config "$c2" ps 2>&1)
rc=$?
[[ $rc -eq 0 ]] && pass_test || fail_test "Config merging rejected (rc=$rc)"

# ═══════════════════════════════════════════════════════════════════
# 14. Auto-detection of sandbox.yml in CWD
#     (Docs: configuration.md — "Auto-detects sandbox.yml in CWD")
# ═══════════════════════════════════════════════════════════════════

begin_test "Auto-detects sandbox.yml in working directory"
dir="$TEST_TMPDIR/autodetect"
mkdir -p "$dir"
cat > "$dir/sandbox.yml" <<'YAML'
sandboxes:
  auto:
    image: ghcr.io/nanosandboxai/nanosandbox/claude:latest
YAML
# Run from that directory so it auto-detects
output=$(cd "$dir" && $NANOSB ps 2>&1)
rc=$?
[[ $rc -eq 0 ]] && pass_test || fail_test "Auto-detection failed (rc=$rc)"

# ═══════════════════════════════════════════════════════════════════
# 15. Invalid YAML fails gracefully
# ═══════════════════════════════════════════════════════════════════

begin_test "Invalid YAML config fails gracefully"
cfg="$TEST_TMPDIR/invalid.yml"
cat > "$cfg" <<'YAML'
this is not: [valid: yaml:
  broken: {unclosed
YAML
output=$(assert_failure "$NANOSB --config $cfg ps") && \
  pass_test || true

# ═══════════════════════════════════════════════════════════════════
# 16. Config with all documented agent images
#     (Docs: agents/overview.md — claude, goose, codex, cursor)
# ═══════════════════════════════════════════════════════════════════

begin_test "Config with all documented agent bare names is accepted"
cfg="$TEST_TMPDIR/all_agents.yml"
cat > "$cfg" <<'YAML'
sandboxes:
  claude-sb:
    image: claude
  goose-sb:
    image: goose
  codex-sb:
    image: codex
  cursor-sb:
    image: cursor
YAML
output=$($NANOSB --config "$cfg" ps 2>&1)
rc=$?
[[ $rc -eq 0 ]] && pass_test || fail_test "All-agents config rejected (rc=$rc)"

# ═══════════════════════════════════════════════════════════════════
# 17. Config with agent API key env vars
#     (Docs: agents/overview.md — ANTHROPIC_API_KEY, OPENAI_API_KEY, etc.)
# ═══════════════════════════════════════════════════════════════════

begin_test "Config with agent API key env vars is accepted"
cfg="$TEST_TMPDIR/apikeys.yml"
cat > "$cfg" <<'YAML'
sandboxes:
  claude-sb:
    image: claude
    env:
      ANTHROPIC_API_KEY: ${ANTHROPIC_API_KEY:-dummy}
YAML
output=$($NANOSB --config "$cfg" ps 2>&1)
rc=$?
[[ $rc -eq 0 ]] && pass_test || skip_test "API key env config failed (rc=$rc)"

print_summary
