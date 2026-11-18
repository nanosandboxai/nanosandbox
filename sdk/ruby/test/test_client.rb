# frozen_string_literal: true

# Unit tests for the Ruby SDK using an in-process fake NDJSON helper on a temp
# Unix socket (mirrors sdk/python/tests/test_client.py).

require "minitest/autorun"
require "socket"
require "tmpdir"
require "fileutils"
require "json"
require "base64"
require "thread"

$LOAD_PATH.unshift(File.expand_path("../lib", __dir__))
require "nanosandbox"

include Nanosandbox

class FakeHelper
  attr_reader :path, :requests

  def initialize
    @dir = Dir.mktmpdir("nanosb-rb-")
    @path = File.join(@dir, "helper.sock")
    @server = UNIXServer.new(@path)
    @queue = []
    @requests = []
    @lock = Mutex.new
    @stop = false
    @thread = Thread.new { serve }
  end

  def push(obj)
    @lock.synchronize { @queue << JSON.generate(obj) }
  end

  def pop
    @lock.synchronize { @queue.shift }
  end

  def close
    @stop = true
    @server.close
    FileUtils.remove_entry(@dir) if File.directory?(@dir)
  rescue IOError
    nil
  end

  private

  def serve
    loop do
      break if @stop

      conn = begin
        @server.accept
      rescue IOError, Errno::EBADF
        break
      end
      Thread.new(conn) { |c| handle(c) }
    end
  end

  def streaming?(req)
    return true if %w[exec_stream attach metrics_stream].include?(req["type"])

    req["type"] == "logs" && req["follow"]
  end

  def terminal?(line)
    obj = JSON.parse(line)
    return %w[exit error].include?(obj["event"]) if obj["type"] == "guest"
    return obj["event"] == "done" if obj["type"] == "event"

    false
  rescue JSON::ParserError
    false
  end

  def handle(conn)
    while (line = conn.gets)
      req = JSON.parse(line)
      @lock.synchronize { @requests << req }
      if req["type"] == "hello"
        conn.write(JSON.generate(
                     "type" => "hello_ok", "protocol" => PROTOCOL_VERSION,
                     "helper_version" => "test",
                     "capabilities" => %w[lifecycle exec fs logs metrics]
                   ) + "\n")
        next
      end
      scripted = pop || JSON.generate("type" => "error", "code" => "internal", "message" => "no script")
      conn.write(scripted + "\n")
      next unless streaming?(req) && !terminal?(scripted)

      loop do
        item = pop
        break if item.nil?

        conn.write(item + "\n")
        break if terminal?(item)
      end
    end
  ensure
    conn.close
  end
end

class ClientTest < Minitest::Test
  def setup
    @helper = FakeHelper.new
  end

  def teardown
    @helper.close
  end

  def test_handshake_and_capabilities
    client = SandboxClient.connect(@helper.path)
    assert_includes client.capabilities, "lifecycle"
    assert client.supports("exec")
  end

  def test_ping
    @helper.push("type" => "pong", "version" => "9")
    client = SandboxClient.connect(@helper.path)
    assert_equal "9", client.ping
  end

  def test_exec
    @helper.push("type" => "exec_result", "exit_code" => 0, "stdout" => "hi\n",
                 "stderr" => "", "duration_ms" => 1)
    client = SandboxClient.connect(@helper.path)
    out = client.sandbox("a").exec("echo", ["hi"])
    assert_equal "hi\n", out["stdout"]
  end

  def test_error_is_typed
    @helper.push("type" => "error", "code" => "not_found", "message" => "nope")
    client = SandboxClient.connect(@helper.path)
    err = assert_raises(NanosbError) { client.get("missing") }
    assert err.is_not_found?
  end

  def test_fs_write_then_read
    @helper.push("type" => "ok", "version" => "1")
    @helper.push("type" => "fs_data", "data_base64" => Base64.strict_encode64("hello"), "eof" => true)
    client = SandboxClient.connect(@helper.path)
    box = client.sandbox("a")
    box.fs_write("/tmp/x", "hello")
    assert_equal "hello", box.fs_read("/tmp/x")
  end

  def test_exec_stream
    @helper.push("type" => "ok", "version" => "1")
    @helper.push("type" => "guest", "event" => "stdout", "data" => "a")
    @helper.push("type" => "guest", "event" => "exit", "code" => 0)
    client = SandboxClient.connect(@helper.path)
    events = []
    code = client.sandbox("a").exec_stream({ "command" => "x" }) { |ev| events << ev }
    assert_equal 0, code
    assert_equal "a", events.first["data"]
  end

  def test_wait_returns_hash
    @helper.push("type" => "wait", "exit_code" => 137, "exited_at_ms" => 1234, "state" => "stopped")
    client = SandboxClient.connect(@helper.path)
    result = client.sandbox("a").wait(30)
    assert_equal 137, result[:exit_code]
    assert_equal 1234, result[:exited_at_ms]
    assert_equal "stopped", result[:state]
  end

  def test_kill_with_sends_signal
    @helper.push("type" => "ok", "version" => "1")
    client = SandboxClient.connect(@helper.path)
    client.sandbox("a").kill_with(15)
    req = @helper.requests.find { |r| r["type"] == "kill" }
    assert_equal 15, req["signal"]
  end

  def test_restart_sends_restart
    @helper.push("type" => "ok", "version" => "1")
    client = SandboxClient.connect(@helper.path)
    client.sandbox("a").restart
    req = @helper.requests.find { |r| r["type"] == "restart" }
    assert_equal "a", req["name"]
  end

  def test_wait_error_raises
    @helper.push("type" => "error", "code" => "not_found", "message" => "nope")
    client = SandboxClient.connect(@helper.path)
    err = assert_raises(NanosbError) { client.sandbox("a").wait }
    assert err.is_not_found?
  end

  def test_builder
    spec = SandboxBuilder.new("alpine")
                         .with_name("demo")
                         .with_resources(2, 512)
                         .publish_tcp(8080, 80)
                         .with_label("team", "sdk")
                         .build
    assert_equal "demo", spec["name"]
    assert_equal "tcp", spec["network"]["publish"][0]["protocol"]
    assert_equal "sdk", spec["labels"]["team"]
  end
end