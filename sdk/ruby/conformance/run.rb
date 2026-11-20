#!/usr/bin/env ruby
# frozen_string_literal: true

# Conformance driver for the Ruby SDK.
#
# Runs every scenario in `sdk/conformance/scenarios/` against the
# `nanosb-conformance` harness and evaluates the scenario `assert` blocks per
# `sdk/conformance/SPEC.md`. Exits non-zero if any scenario fails.
#
# Usage:
#   ruby -I sdk/ruby/lib sdk/ruby/conformance/run.rb
#   NANOSB_CONFORMANCE_BIN=/path/to/nanosb-conformance ruby -I sdk/ruby/lib sdk/ruby/conformance/run.rb

require "json"
require "base64"
require "timeout"

$LOAD_PATH.unshift(File.expand_path("../lib", __dir__)) unless $LOAD_PATH.include?(File.expand_path("../lib", __dir__))
require "nanosandbox"

include Nanosandbox

REPO_ROOT = File.expand_path("../../..", __dir__)
SCENARIOS_DIR = File.join(REPO_ROOT, "sdk", "conformance", "scenarios")
SCENARIO_BUDGET = 60

class DriverState
  attr_accessor :acked, :errors, :exit_code, :stdout, :stderr, :get_result,
                :list_result, :fs_entries, :fs_stat, :fs_exists, :fs_read_bytes,
                :fs_content, :chunks_acked, :first_logs, :resume_logs, :follow_lines,
                :metrics, :metrics_samples, :volume_created, :volumes, :volume_get,
                :secrets, :snapshot_created, :snapshots, :attach_events,
                :attach_exit_code, :ping_ok, :wait_result, :timeout_code,
                :unsupported_observed

  def initialize
    @acked = {}
    @errors = []
    @exit_code = nil
    @stdout = ""
    @stderr = ""
    @get_result = nil
    @list_result = nil
    @fs_entries = nil
    @fs_stat = nil
    @fs_exists = nil
    @fs_read_bytes = nil
    @fs_content = nil
    @chunks_acked = 0
    @first_logs = nil
    @resume_logs = nil
    @follow_lines = []
    @metrics = nil
    @metrics_samples = []
    @volume_created = nil
    @volumes = nil
    @volume_get = nil
    @secrets = nil
    @snapshot_created = nil
    @snapshots = nil
    @attach_events = []
    @attach_exit_code = nil
    @ping_ok = false
    @wait_result = nil
    @timeout_code = nil
    @unsupported_observed = false
  end
end

def req_from_expect(expect)
  expect.reject { |k, _| %w[type name].include?(k) }
end

def drive_step(client, state, expect)
  etype = expect["type"]
  name = expect["name"]
  box = client.sandbox(name.nil? ? "" : name)
  begin
    case etype
    when "create"
      client.create(expect["spec"])
      state.acked["create"] = true
    when "get"
      state.get_result = client.get(name)
    when "list"
      state.list_result = client.list(expect.fetch("all", false))
    when "start"
      box.start
      state.acked["start"] = true
    when "stop"
      box.stop(expect.fetch("force", false))
      state.acked["stop"] = true
    when "kill"
      if expect.key?("signal")
        box.kill_with(expect["signal"])
      else
        box.kill
      end
      state.acked["kill"] = true
    when "wait"
      state.wait_result = box.wait(expect["timeout_secs"])
      state.acked["wait"] = true
    when "restart"
      box.restart
      state.acked["restart"] = true
    when "remove"
      box.remove(expect.fetch("force", false))
      state.acked["remove"] = true
    when "drain"
      box.drain
      state.acked["drain"] = true
    when "touch"
      box.touch
      state.acked["touch"] = true
    when "pause"
      box.pause
      state.acked["pause"] = true
    when "resume"
      box.resume
      state.acked["resume"] = true
    when "modify"
      box.modify(expect["spec"])
      state.acked["modify"] = true
    when "status"
      box.state
    when "ping"
      client.ping
      state.ping_ok = true
    when "exec"
      result = box.exec_with(req_from_expect(expect))
      state.exit_code = result["exit_code"]
      state.stdout = result["stdout"] || ""
      state.stderr = result["stderr"] || ""
    when "exec_stream"
      state.exit_code = box.exec_stream(req_from_expect(expect)) do |ev|
        state.stdout += ev["data"] || "" if ev["event"] == "stdout"
        state.stderr += ev["data"] || "" if ev["event"] == "stderr"
      end
    when "shell"
      result = box.shell(expect["script"])
      state.exit_code = result["exit_code"]
      state.stdout = result["stdout"] || ""
      state.stderr = result["stderr"] || ""
    when "attach"
      box.attach { |ev| state.attach_events << ev }
      exit_ev = state.attach_events.find { |e| e["event"] == "exit" }
      state.attach_exit_code = exit_ev && exit_ev["code"]
    when "attach_input"
      client.call_raw("type" => "attach_input", "data" => expect["data"])
      state.acked["input"] = true
    when "attach_resize"
      client.call_raw("type" => "attach_resize", "cols" => expect["cols"], "rows" => expect["rows"])
      state.acked["resize"] = true
    when "fs_read"
      state.fs_read_bytes = box.fs_read(expect["path"])
    when "fs_read_to_string"
      state.fs_content = box.fs_read_to_string(expect["path"])
    when "fs_write"
      box.fs_write(expect["path"], Nanosandbox.unb64(expect["data_base64"]))
      state.acked["write"] = true
    when "fs_list"
      state.fs_entries = box.fs_list(expect["path"])
    when "fs_stat"
      state.fs_stat = box.fs_stat(expect["path"])
    when "fs_exists"
      state.fs_exists = box.fs_exists(expect["path"])
    when "fs_mkdir"
      box.fs_mkdir(expect["path"], expect.fetch("recursive", false))
      state.acked["mkdir"] = true
    when "fs_remove"
      box.fs_remove(expect["path"])
    when "fs_remove_dir"
      box.fs_remove_dir(expect["path"], expect.fetch("recursive", false))
      state.acked["remove_dir"] = true
    when "fs_copy"
      box.fs_copy(expect["from"], expect["to"])
      state.acked["copy"] = true
    when "fs_rename"
      box.fs_rename(expect["from"], expect["to"])
      state.acked["rename"] = true
    when "fs_copy_from_host"
      box.fs_copy_from_host(expect["host_path"], expect["guest_path"])
      state.acked["copy_from_host"] = true
    when "fs_copy_to_host"
      box.fs_copy_to_host(expect["guest_path"], expect["host_path"])
      state.acked["copy_to_host"] = true
    when "logs"
      if expect["follow"]
        box.logs_follow_with(expect["tail"], expect["cursor"], expect["source"]) do |line|
          state.follow_lines << line
        end
      else
        req = { "type" => "logs", "name" => name, "follow" => false }
        %w[tail cursor source].each { |k| req[k] = expect[k] if expect.key?(k) }
        resp = client.call_raw(req)
        if state.first_logs.nil?
          state.first_logs = resp
        else
          state.resume_logs = resp
        end
      end
    when "metrics"
      state.metrics = box.metrics
    when "metrics_stream"
      box.metrics_stream(expect["interval_ms"]) { |s| state.metrics_samples << s }
    when "volume_create"
      vol = expect["volume"]
      state.volume_created = box.volume_create(vol["name"], vol["dir"])
    when "volume_list"
      state.volumes = box.volume_list
    when "volume_get"
      state.volume_get = box.volume_get(expect["volume"])
    when "volume_remove"
      box.volume_remove(expect["volume"])
      state.acked["remove"] = true
    when "mount_add"
      box.mount_add(expect["mount"])
      state.acked["mount_add"] = true
    when "secret_set"
      box.secret_set(expect["key"], expect["value"])
      state.acked["set"] = true
    when "secret_list"
      state.secrets = box.secret_list
    when "secret_remove"
      box.secret_remove(expect["key"])
      state.acked["remove"] = true
    when "snapshot_create"
      state.snapshot_created = box.snapshot_create(expect["dest"])
    when "snapshot_list"
      state.snapshots = client.snapshots
    when "snapshot_restore"
      box.snapshot_restore(expect["snapshot"], expect["name"])
      state.acked["restore"] = true
    when "snapshot_fork"
      box.snapshot_fork(expect["snapshot"], expect["name"])
      state.acked["fork"] = true
    when "snapshot_remove"
      box.snapshot_remove(expect["snapshot"])
      state.acked["remove"] = true
    else
      raise "unhandled expect.type #{etype.inspect}"
    end
  rescue NanosbError => e
    state.errors << [e.code, e.message]
  end
end

ACK_KEYS = {
  "create_acked" => "create", "start_acked" => "start", "stop_acked" => "stop",
  "kill_acked" => "kill", "wait_acked" => "wait", "restart_acked" => "restart",
  "remove_acked" => "remove", "drain_acked" => "drain",
  "touch_acked" => "touch", "pause_acked" => "pause", "resume_acked" => "resume",
  "modify_acked" => "modify", "write_acked" => "write", "mkdir_acked" => "mkdir",
  "remove_dir_acked" => "remove_dir", "copy_acked" => "copy", "rename_acked" => "rename",
  "copy_from_host_acked" => "copy_from_host", "copy_to_host_acked" => "copy_to_host",
  "mount_add_acked" => "mount_add", "set_acked" => "set", "restore_acked" => "restore",
  "fork_acked" => "fork", "input_acked" => "input", "resize_acked" => "resize",
  "write_stream_acked" => "write_stream"
}.freeze

def evaluate_assert(assert, state)
  failures = []
  check = ->(cond, msg) { failures << msg unless cond }

  check.call(state.exit_code == assert["exit_code"], "exit_code expected #{assert['exit_code']} got #{state.exit_code}") if assert.key?("exit_code")
  check.call(state.stdout == assert["stdout_equals"], "stdout_equals expected #{assert['stdout_equals'].inspect} got #{state.stdout.inspect}") if assert.key?("stdout_equals")
  check.call(state.stderr == assert["stderr_equals"], "stderr_equals mismatch") if assert.key?("stderr_equals")
  %w[stdout_contains stderr_contains].each do |key|
    next unless assert.key?(key)

    hay = key == "stdout_contains" ? state.stdout : state.stderr
    assert[key].each { |n| check.call(hay.include?(n), "#{key}: #{n.inspect} not in #{hay.inspect}") }
  end
  ACK_KEYS.each { |key, nm| check.call(state.acked[nm], "#{key}: expected ack #{nm}") if assert.key?(key) }
  if assert.key?("wait_exit_code")
    got = state.wait_result && state.wait_result[:exit_code]
    check.call(got == assert["wait_exit_code"], "wait_exit_code expected #{assert['wait_exit_code']} got #{got}")
  end
  if assert.key?("wait_state")
    got = state.wait_result && state.wait_result[:state]
    check.call(got == assert["wait_state"], "wait_state expected #{assert['wait_state']} got #{got}")
  end
  if assert.key?("get_name")
    check.call(state.get_result && state.get_result["name"] == assert["get_name"], "get_name mismatch")
  end
  if assert.key?("get_state")
    check.call(state.get_result && state.get_result["state"] == assert["get_state"], "get_state mismatch")
  end
  if assert.key?("get_image")
    check.call(state.get_result && state.get_result["image"] == assert["get_image"], "get_image mismatch")
  end
  if assert.key?("list_names")
    names = (state.list_result || []).map { |s| s["name"] }
    check.call(names == assert["list_names"], "list_names expected #{assert['list_names']} got #{names}")
  end
  if assert.key?("list_states")
    states = (state.list_result || []).map { |s| s["state"] }
    check.call(states == assert["list_states"], "list_states expected #{assert['list_states']} got #{states}")
  end
  if assert.key?("entries_contain")
    names = (state.fs_entries || []).map { |e| e["name"] }
    assert["entries_contain"].each { |n| check.call(names.include?(n), "entries_contain #{n.inspect} not in #{names}") }
  end
  if assert.key?("stat_is_dir")
    check.call(!state.fs_stat.nil? && state.fs_stat["is_dir"] == assert["stat_is_dir"], "stat_is_dir mismatch")
  end
  check.call(state.fs_exists == assert["exists"], "exists expected #{assert['exists']}") if assert.key?("exists")
  if assert.key?("read_bytes_base64")
    got = Nanosandbox.b64(state.fs_read_bytes || "")
    check.call(got == assert["read_bytes_base64"], "read_bytes_base64 expected #{assert['read_bytes_base64']} got #{got}")
  end
  check.call(state.fs_content == assert["content_equals"], "content_equals mismatch") if assert.key?("content_equals")
  check.call(state.chunks_acked == assert["chunks_acked"], "chunks_acked expected #{assert['chunks_acked']}") if assert.key?("chunks_acked")
  { "lines_contain" => state.first_logs, "resume_lines_contain" => state.resume_logs }.each do |key, lines|
    next unless assert.key?(key)

    data = ((lines || {})["lines"] || []).map { |l| l["data"] || "" }
    assert[key].each { |n| check.call(data.any? { |d| d.include?(n) }, "#{key}: #{n.inspect} not in #{data}") }
  end
  if assert.key?("follow_lines_contain")
    data = state.follow_lines.map { |l| l["data"] || "" }
    assert["follow_lines_contain"].each { |n| check.call(data.any? { |d| d.include?(n) }, "follow_lines_contain #{n.inspect}") }
  end
  if assert.key?("next_cursor")
    got = state.first_logs && state.first_logs["next_cursor"]
    check.call(got == assert["next_cursor"], "next_cursor expected #{assert['next_cursor']} got #{got}")
  end
  if assert.key?("all_source")
    srcs = ((state.first_logs || {})["lines"] || []).map { |l| l["source"] }
    check.call(srcs.all? { |s| s == assert["all_source"] }, "all_source mismatch #{srcs}")
  end
  check.call(state.metrics && state.metrics["cpu_percent"] == assert["cpu_percent"], "cpu_percent mismatch") if assert.key?("cpu_percent")
  check.call(state.metrics && state.metrics["memory_used_bytes"] == assert["memory_used_bytes"], "memory_used_bytes mismatch") if assert.key?("memory_used_bytes")
  if assert.key?("has_fields")
    assert["has_fields"].each { |f| check.call(!state.metrics.nil? && state.metrics.key?(f), "has_fields #{f.inspect}") }
  end
  check.call(state.metrics_samples.length == assert["stream_samples"], "stream_samples expected #{assert['stream_samples']}") if assert.key?("stream_samples")
  if assert.key?("first_cpu_percent")
    got = state.metrics_samples.empty? ? nil : state.metrics_samples[0]["cpu_percent"]
    check.call(got == assert["first_cpu_percent"], "first_cpu_percent expected #{assert['first_cpu_percent']}")
  end
  check.call(state.volume_created && state.volume_created["name"] == assert["volume_created"], "volume_created mismatch") if assert.key?("volume_created")
  if assert.key?("volumes_contain")
    names = (state.volumes || []).map { |v| v["name"] }
    assert["volumes_contain"].each { |n| check.call(names.include?(n), "volumes_contain #{n.inspect}") }
  end
  check.call(state.volume_get && state.volume_get["name"] == assert["volume_get_name"], "volume_get_name mismatch") if assert.key?("volume_get_name")
  if assert.key?("secrets_contain")
    keys = (state.secrets || []).map { |s| s["key"] }
    assert["secrets_contain"].each { |k| check.call(keys.include?(k), "secrets_contain #{k.inspect}") }
  end
  if assert.key?("secret_values_redacted")
    vals = (state.secrets || []).map { |s| s["value"] }
    check.call(vals.all? { |v| v.nil? || v == "" }, "secret_values_redacted #{vals}")
  end
  check.call(state.snapshot_created && state.snapshot_created["id"] == assert["snapshot_created"], "snapshot_created mismatch") if assert.key?("snapshot_created")
  if assert.key?("snapshots_contain")
    ids = (state.snapshots || []).map { |s| s["id"] }
    assert["snapshots_contain"].each { |s| check.call(ids.include?(s), "snapshots_contain #{s.inspect}") }
  end
  if assert.key?("attach_events")
    got = state.attach_events.map { |e| e["event"] }
    check.call(got == assert["attach_events"], "attach_events expected #{assert['attach_events']} got #{got}")
  end
  check.call(state.attach_exit_code == assert["attach_exit_code"], "attach_exit_code mismatch") if assert.key?("attach_exit_code")
  if assert.key?("error_code")
    got = state.errors.empty? ? nil : state.errors[0][0]
    check.call(got == assert["error_code"], "error_code expected #{assert['error_code']} got #{got}")
  end
  if assert.key?("second_error_code")
    got = state.errors.length > 1 ? state.errors[1][0] : nil
    check.call(got == assert["second_error_code"], "second_error_code expected #{assert['second_error_code']} got #{got}")
  end
  if assert.key?("error_message_contains")
    msg = state.errors.empty? ? "" : state.errors[0][1]
    check.call(msg.include?(assert["error_message_contains"]), "error_message_contains")
  end
  if assert.key?("expects_timeout")
    check.call(state.timeout_code == assert.fetch("timeout_code", "timeout"), "timeout_code expected #{assert.fetch('timeout_code', 'timeout')} got #{state.timeout_code}")
  end
  check.call(state.unsupported_observed, "expected unsupported error") if assert.key?("expects_unsupported")
  check.call(state.ping_ok, "ping_ok: ping did not succeed") if assert.key?("ping_ok")
  check.call(state.exit_code == assert["exec_exit_code"], "exec_exit_code mismatch") if assert.key?("exec_exit_code")
  check.call(state.stdout == assert["exec_stdout"], "exec_stdout mismatch") if assert.key?("exec_stdout")
  failures
end

def run_scenario(bin_path, scenario_file)
  scenario = JSON.parse(File.read(scenario_file))
  assert_block = scenario["assert"] || {}
  sid = scenario["id"]

  io = IO.popen([bin_path, "serve", "--scenario", scenario_file])
  begin
    line = io.gets
    line = line.nil? ? "" : line.strip
    unless line.start_with?("READY ")
      return [false, "harness did not print READY"]
    end
    sock = line.split(" ", 2)[1]

    client = SandboxClient.connect(sock)
    state = DriverState.new
    write_conn = nil
    begin
      scenario["steps"].each do |step|
        expect = step["expect"] || {}
        etype = expect["type"]
        if assert_block["expects_timeout"] && !step["timeout_ms"].nil?
          conn = Conn.new(sock)
          begin
            ready = IO.select([conn.sock], nil, nil, 10)
            resp = ready ? conn.recv : {}
            state.timeout_code = resp["code"]
          ensure
            conn.close
          end
          break
        end
        if etype == "fs_data"
          write_conn.send_msg("type" => "fs_data", "data_base64" => expect["data_base64"], "eof" => expect["eof"])
          write_conn.recv
          state.chunks_acked += 1
          next
        end
        if etype == "fs_write_stream"
          write_conn = Conn.new(sock)
          write_conn.send_msg("type" => "fs_write_stream", "name" => expect["name"], "path" => expect["path"])
          ack = write_conn.recv
          if ack["type"] == "error"
            state.errors << [ack["code"], ack["message"]]
          else
            state.acked["write_stream"] = true
          end
          next
        end
        drive_step(client, state, expect)
      end
    ensure
      write_conn&.close
    end

    if assert_block["expects_unsupported"]
      verb = assert_block["unsupported_verb"]
      if client.supports(verb)
        state.unsupported_observed = false
      else
        begin
          if verb == "exec"
            client.sandbox("box").exec("echo", [])
            state.unsupported_observed = false
          end
        rescue NanosbError => e
          state.unsupported_observed = e.code == "unsupported"
        end
      end
    end

    failures = evaluate_assert(assert_block, state)
    failures.empty? ? [true, "ok"] : [false, failures.join("; ")]
  ensure
    Process.kill("TERM", io.pid)
    begin
      io.close
    rescue StandardError
      nil
    end
  end
end

def find_binary
  env = ENV["NANOSB_CONFORMANCE_BIN"]
  if env
    abort("error: NANOSB_CONFORMANCE_BIN=#{env} does not exist") unless File.exist?(env)

    return env
  end
  default = File.join(REPO_ROOT, "target", "debug", "nanosb-conformance")
  unless File.exist?(default)
    abort("error: nanosb-conformance not found at #{default}; build it or set NANOSB_CONFORMANCE_BIN")
  end
  default
end

bin_path = find_binary
scenarios = Dir.glob(File.join(SCENARIOS_DIR, "*.json")).sort
abort("error: no scenarios in #{SCENARIOS_DIR}") if scenarios.empty?

passed = 0
failed = 0
scenarios.each do |scenario_file|
  sid = JSON.parse(File.read(scenario_file))["id"]
  ok = false
  message = "ok"
  begin
    Timeout.timeout(SCENARIO_BUDGET) do
      ok, message = run_scenario(bin_path, scenario_file)
    end
  rescue Timeout::Error
    ok = false
    message = "scenario exceeded time budget"
  rescue StandardError => e
    ok = false
    message = "driver error: #{e.message}"
  end
  if ok
    passed += 1
    puts "PASS #{sid}"
  else
    failed += 1
    puts "FAIL #{sid}: #{message}"
  end
end

puts "\n#{passed} passed, #{failed} failed"
exit(failed.zero? ? 0 : 1)