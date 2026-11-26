# frozen_string_literal: true

# Protocol client for the nanosandbox helper (nanosb-protocol v1.0.0).
#
# Mirrors sdk/python/nanosandbox/client.py. Each call dials a fresh connection
# and re-negotiates the hello handshake (stateless).

require "socket"
require "json"
require "base64"

module Nanosandbox
  PROTOCOL_VERSION = "1.0.0"
  CLIENT_NAME = "nanosandbox-ruby"
  # The SDK's own release version, used to pin the runtime version when the user
  # has not set NANOSB_VERSION (see nanosandbox/runtime.rb).
  VERSION = "0.3.0"

  class NanosbError < StandardError
    attr_reader :code, :message

    def initialize(code, message)
      @code = code
      @message = message
      super("#{code}: #{message}")
    end

    def is_not_found?
      @code == "not_found"
    end
  end

  def self.b64(data)
    Base64.strict_encode64(data)
  end

  def self.unb64(text)
    Base64.decode64(text)
  end

  # One negotiated connection (dial -> hello -> request -> response).
  class Conn
    attr_reader :hello, :sock

    def initialize(path, client_name = CLIENT_NAME)
      @sock = Socket.new(Socket::AF_UNIX, Socket::SOCK_STREAM, 0)
      @sock.connect(Socket.pack_sockaddr_un(path))
      @hello = handshake(client_name)
    end

    def handshake(client_name)
      send_msg(
        "type" => "hello",
        "client" => client_name,
        "sdk_version" => PROTOCOL_VERSION,
        "protocol_min" => PROTOCOL_VERSION,
        "protocol_max" => PROTOCOL_VERSION
      )
      resp = recv
      unless resp["type"] == "hello_ok"
        raise NanosbError.new(resp["code"] || "internal", resp["message"] || "bad handshake")
      end
      unless resp["protocol"] == PROTOCOL_VERSION
        raise NanosbError.new("incompatible_version",
                              "helper speaks #{resp['protocol']}, client needs #{PROTOCOL_VERSION}")
      end
      resp
    end

    def send_msg(message)
      @sock.write(JSON.generate(message) + "\n")
    end

    def recv
      line = @sock.gets
      raise NanosbError.new("transport", "connection closed") if line.nil?

      JSON.parse(line)
    end

    def call(message)
      send_msg(message)
      resp = recv
      if resp["type"] == "error"
        raise NanosbError.new(resp["code"] || "internal", resp["message"] || "")
      end
      resp
    end

    def close
      @sock.close unless @sock.closed?
    end
  end

  # Fluent builder for a sandbox creation spec.
  class SandboxBuilder
    def initialize(image)
      @image = image
      @name = nil
      @cpus = nil
      @memory_mb = nil
      @env = {}
      @mounts = []
      @network = {}
      @workdir = nil
      @timeout_secs = nil
      @user = nil
      @home = nil
      @command = nil
      @command_args = []
      @labels = {}
      @ephemeral = false
      @replace = false
      @detached = true
      @hostname = nil
      @secret_env = {}
    end

    def with_name(name)
      @name = name
      self
    end

    def with_resources(cpus, memory_mb)
      @cpus = cpus
      @memory_mb = memory_mb
      self
    end

    def with_env(key, value)
      @env[key] = value
      self
    end

    def with_label(key, value)
      @labels[key] = value
      self
    end

    def mount(host_path, guest_path, readonly = false)
      @mounts << {
        "host_path" => host_path,
        "guest_path" => guest_path,
        "readonly" => readonly,
        "mount_type" => "virtiofs"
      }
      self
    end

    def publish_tcp(host_port, guest_port)
      @network["enabled"] = true
      (@network["publish"] ||= []) << {
        "host_port" => host_port, "container_port" => guest_port, "protocol" => "tcp"
      }
      self
    end

    def publish_udp(host_port, guest_port)
      @network["enabled"] = true
      (@network["publish"] ||= []) << {
        "host_port" => host_port, "container_port" => guest_port, "protocol" => "udp"
      }
      self
    end

    def detached(detached)
      @detached = detached
      self
    end

    def hostname(hostname)
      @hostname = hostname
      self
    end

    def mount_file(host_path, guest_path)
      @mounts << {
        "host_path" => host_path, "guest_path" => guest_path, "readonly" => false,
        "mount_type" => "virtiofs", "file" => true
      }
      self
    end

    def mount_tmpfs(guest_path)
      @mounts << {
        "host_path" => "", "guest_path" => guest_path, "readonly" => false,
        "mount_type" => "tmpfs", "tmpfs" => true
      }
      self
    end

    def mount_options(options)
      @mounts.last["options"] = options.dup unless @mounts.empty?
      self
    end

    def mount_owner(uid, gid)
      unless @mounts.empty?
        @mounts.last["uid"] = uid
        @mounts.last["gid"] = gid
      end
      self
    end

    def secret_env(key, secret_key)
      @secret_env[key] = secret_key
      self
    end

    def build
      spec = { "image" => @image, "detached" => @detached }
      {
        "name" => @name, "cpus" => @cpus, "memory_mb" => @memory_mb,
        "workdir" => @workdir, "timeout_secs" => @timeout_secs,
        "user" => @user, "home" => @home, "command" => @command
      }.each do |k, v|
        spec[k] = v unless v.nil?
      end
      spec["hostname"] = @hostname unless @hostname.nil?
      spec["env"] = @env unless @env.empty?
      spec["mounts"] = @mounts unless @mounts.empty?
      spec["network"] = @network unless @network.empty?
      spec["command_args"] = @command_args unless @command_args.empty?
      spec["labels"] = @labels unless @labels.empty?
      spec["ephemeral"] = true if @ephemeral
      spec["replace"] = true if @replace
      spec["secret_env"] = @secret_env unless @secret_env.empty?
      spec
    end
  end

  # Fluent builder for a volume creation spec.
  class VolumeBuilder
    def initialize(name)
      @name = name
      @dir = nil
      @disk_image = nil
      @fstype = nil
      @quota_bytes = nil
    end

    def with_dir(dir)
      @dir = dir
      self
    end

    def disk_image(image)
      @disk_image = image
      self
    end

    def fstype(fstype)
      @fstype = fstype
      self
    end

    def volume_quota(bytes)
      @quota_bytes = bytes
      self
    end

    def build
      spec = { "name" => @name }
      { "dir" => @dir, "disk_image" => @disk_image,
        "fstype" => @fstype, "quota_bytes" => @quota_bytes }.each do |k, v|
        spec[k] = v unless v.nil?
      end
      spec
    end
  end

  # A live attach session: drive input/resize and read events on one connection.
  class AttachSession
    def initialize(conn)
      @conn = conn
    end

    def send_input(data)
      text = data.dup.force_encoding("UTF-8")
      @conn.call("type" => "attach_input", "data" => text)
    end

    def resize(cols, rows)
      @conn.call("type" => "attach_resize", "cols" => cols, "rows" => rows)
    end

    # Returns the next guest event hash, or nil once the stream ends.
    def read_events
      loop do
        resp = @conn.recv
        next unless resp["type"] == "guest"

        return nil if %w[exit error].include?(resp["event"])

        return resp
      end
    end

    def close
      @conn.close
    end
  end

  # A handle bound to one sandbox.
  class Sandbox
    attr_reader :name

    def initialize(client, name)
      @client = client
      @name = name
    end

    def state
      @client.call_raw("type" => "status", "name" => @name)["state"]
    end

    def info
      @client.get(@name)
    end

    def start
      @client.call_raw("type" => "start", "name" => @name)
    end

    def stop(force = false)
      @client.call_raw("type" => "stop", "name" => @name, "force" => force)
    end

    def kill
      @client.call_raw("type" => "kill", "name" => @name)
    end

    def kill_with(signal)
      @client.call_raw("type" => "kill", "name" => @name, "signal" => signal)
    end

    def restart
      @client.call_raw("type" => "restart", "name" => @name)
    end

    def wait(timeout_secs = nil)
      req = { "type" => "wait", "name" => @name }
      req["timeout_secs"] = timeout_secs unless timeout_secs.nil?
      resp = @client.call_raw(req)
      {
        exit_code: resp["exit_code"],
        exited_at_ms: resp["exited_at_ms"],
        state: resp["state"]
      }
    end

    def remove(force = false)
      @client.call_raw("type" => "remove", "name" => @name, "force" => force)
    end

    def pause
      @client.call_raw("type" => "pause", "name" => @name)
    end

    def resume
      @client.call_raw("type" => "resume", "name" => @name)
    end

    def touch
      @client.call_raw("type" => "touch", "name" => @name)
    end

    def drain
      @client.call_raw("type" => "drain", "name" => @name)
    end

    def modify(spec)
      @client.call_raw("type" => "modify", "name" => @name, "spec" => spec)
    end

    def wait_until_stopped(timeout_secs = 30.0)
      result = wait(timeout_secs)
      raise NanosbError.new("internal", "sandbox entered error state") if result[:state] == "error"

      result[:state]
    end

    def exec(command, args = nil)
      exec_with("command" => command, "args" => args || [])
    end

    def exec_with(request)
      req = request.dup
      req["type"] = "exec"
      req["name"] = @name
      req["args"] ||= []
      @client.call_raw(req)
    end

    def shell(script)
      @client.call_raw("type" => "shell", "name" => @name, "script" => script)
    end

    def exec_stream(request, &on_event)
      req = request.dup
      req["type"] = "exec_stream"
      req["name"] = @name
      req["args"] ||= []
      code = -1
      conn = Conn.new(@client.socket)
      begin
        conn.send_msg(req)
        ack = conn.recv
        raise NanosbError.new(ack["code"] || "internal", ack["message"] || "") if ack["type"] == "error"

        loop do
          resp = conn.recv
          next unless resp["type"] == "guest"

          event = resp["event"]
          if event == "exit"
            code = resp["code"] || -1
            on_event.call(resp) if on_event
            break
          end
          if event == "error"
            on_event.call(resp) if on_event
            raise NanosbError.new("internal", resp["message"] || "")
          end
          on_event.call(resp) if on_event
        end
      ensure
        conn.close
      end
      code
    end

    def shell_stream(script, &on_event)
      exec_stream({ "command" => script, "shell" => true }, &on_event)
    end

    def attach(&on_event)
      conn = Conn.new(@client.socket)
      begin
        conn.send_msg("type" => "attach", "name" => @name)
        ack = conn.recv
        raise NanosbError.new(ack["code"] || "internal", ack["message"] || "") if ack["type"] == "error"

        loop do
          resp = conn.recv
          next unless resp["type"] == "guest"

          on_event.call(resp) if on_event
          break if %w[exit error].include?(resp["event"])
        end
      ensure
        conn.close
      end
    end

    def attach_session
      conn = Conn.new(@client.socket)
      conn.send_msg("type" => "attach", "name" => @name)
      ack = conn.recv
      if ack["type"] == "error"
        conn.close
        raise NanosbError.new(ack["code"] || "internal", ack["message"] || "")
      end
      AttachSession.new(conn)
    end

    def metrics
      @client.call_raw("type" => "metrics", "name" => @name)
    end

    def metrics_stream(interval_ms, &on_event)
      req = { "type" => "metrics_stream", "name" => @name }
      req["interval_ms"] = interval_ms unless interval_ms.nil?
      conn = Conn.new(@client.socket)
      begin
        conn.send_msg(req)
        ack = conn.recv
        raise NanosbError.new(ack["code"] || "internal", ack["message"] || "") if ack["type"] == "error"

        loop do
          resp = conn.recv
          next unless resp["type"] == "event"

          break if resp["event"] == "done"

          on_event.call(resp) if on_event && resp["event"] == "metrics"
        end
      ensure
        conn.close
      end
    end

    def logs(tail = nil)
      req = { "type" => "logs", "name" => @name, "follow" => false }
      req["tail"] = tail unless tail.nil?
      @client.call_raw(req)["lines"]
    end

    def logs_with(tail = nil, cursor = nil, source = nil)
      req = { "type" => "logs", "name" => @name, "follow" => false }
      req["tail"] = tail unless tail.nil?
      req["cursor"] = cursor unless cursor.nil?
      req["source"] = source unless source.nil?
      @client.call_raw(req)["lines"]
    end

    def logs_follow(tail = nil, &on_line)
      logs_follow_with(tail, nil, nil, &on_line)
    end

    def logs_follow_with(tail = nil, cursor = nil, source = nil, &on_line)
      req = { "type" => "logs", "name" => @name, "follow" => true }
      req["tail"] = tail unless tail.nil?
      req["cursor"] = cursor unless cursor.nil?
      req["source"] = source unless source.nil?
      conn = Conn.new(@client.socket)
      begin
        conn.send_msg(req)
        ack = conn.recv
        raise NanosbError.new(ack["code"] || "internal", ack["message"] || "") if ack["type"] == "error"

        loop do
          resp = conn.recv
          next unless resp["type"] == "event"

          break if resp["event"] == "done"

          on_line.call(resp) if on_line && resp["event"] == "log"
        end
      ensure
        conn.close
      end
    end

    def fs_read(path, offset = nil, length = nil)
      req = { "type" => "fs_read", "name" => @name, "path" => path }
      req["offset"] = offset unless offset.nil?
      req["length"] = length unless length.nil?
      Nanosandbox.unb64(@client.call_raw(req)["data_base64"])
    end

    def fs_read_range(path, offset, length)
      Nanosandbox.unb64(
        @client.call_raw(
          "type" => "fs_read", "name" => @name, "path" => path, "offset" => offset, "length" => length
        )["data_base64"]
      )
    end

    def fs_read_stream(path, chunk_size, &on_chunk)
      chunk_size = 1 if chunk_size < 1
      offset = 0
      loop do
        chunk = fs_read_range(path, offset, chunk_size)
        break if chunk.empty?

        on_chunk.call(chunk) if on_chunk
        break if chunk.bytesize < chunk_size

        offset += chunk.bytesize
      end
    end

    def fs_write_stream(path, data, chunk_size)
      conn = Conn.new(@client.socket)
      begin
        conn.send_msg("type" => "fs_write_stream", "name" => @name, "path" => path)
        ack = conn.recv
        raise NanosbError.new(ack["code"] || "internal", ack["message"] || "") if ack["type"] == "error"

        chunk_size = 1 if chunk_size < 1
        chunks = []
        i = 0
        while i < data.bytesize
          chunks << data.byteslice(i, chunk_size)
          i += chunk_size
        end
        chunks = [""] if chunks.empty?
        chunks.each_with_index do |chunk, idx|
          conn.call("type" => "fs_data", "data_base64" => Nanosandbox.b64(chunk),
                    "eof" => idx == chunks.length - 1)
        end
      ensure
        conn.close
      end
    end

    def fs_read_to_string(path)
      @client.call_raw("type" => "fs_read_to_string", "name" => @name, "path" => path)["content"]
    end

    def fs_write(path, data, append = false)
      @client.call_raw("type" => "fs_write", "name" => @name, "path" => path,
                       "data_base64" => Nanosandbox.b64(data), "append" => append)
    end

    def fs_list(path)
      @client.call_raw("type" => "fs_list", "name" => @name, "path" => path)["entries"]
    end

    def fs_stat(path)
      @client.call_raw("type" => "fs_stat", "name" => @name, "path" => path)
    end

    def fs_exists(path)
      @client.call_raw("type" => "fs_exists", "name" => @name, "path" => path)["exists"]
    end

    def fs_mkdir(path, recursive = false)
      @client.call_raw("type" => "fs_mkdir", "name" => @name, "path" => path, "recursive" => recursive)
    end

    def fs_remove(path)
      @client.call_raw("type" => "fs_remove", "name" => @name, "path" => path)
    end

    def fs_remove_dir(path, recursive = false)
      @client.call_raw("type" => "fs_remove_dir", "name" => @name, "path" => path, "recursive" => recursive)
    end

    def fs_copy(src, dst)
      @client.call_raw("type" => "fs_copy", "name" => @name, "from" => src, "to" => dst)
    end

    def fs_rename(src, dst)
      @client.call_raw("type" => "fs_rename", "name" => @name, "from" => src, "to" => dst)
    end

    def fs_copy_from_host(host_path, guest_path)
      @client.call_raw("type" => "fs_copy_from_host", "name" => @name,
                       "host_path" => host_path, "guest_path" => guest_path)
    end

    def fs_copy_to_host(guest_path, host_path)
      @client.call_raw("type" => "fs_copy_to_host", "name" => @name,
                       "host_path" => host_path, "guest_path" => guest_path)
    end

    def volume_create(name, dir = nil)
      @client.call_raw("type" => "volume_create", "name" => @name,
                       "volume" => { "name" => name, "dir" => dir })
    end

    def volume_list
      @client.call_raw("type" => "volume_list", "name" => @name)["volumes"]
    end

    def volume_get(volume)
      @client.call_raw("type" => "volume_get", "name" => @name, "volume" => volume)
    end

    def volume_remove(volume)
      @client.call_raw("type" => "volume_remove", "name" => @name, "volume" => volume)
    end

    def volume_fs(name)
      info = volume_get(name)
      return info["dir"] if info && info["dir"]

      home = ENV["NANOSB_HOME"] || File.join(Dir.home, ".nanosandbox")
      File.join(home, "volumes", name)
    end

    def mount_add(mount)
      @client.call_raw("type" => "mount_add", "name" => @name, "mount" => mount)
    end

    def secret_set(key, value)
      @client.call_raw("type" => "secret_set", "name" => @name, "key" => key, "value" => value)
    end

    def secret_list
      @client.call_raw("type" => "secret_list", "name" => @name)["secrets"]
    end

    def secret_remove(key)
      @client.call_raw("type" => "secret_remove", "name" => @name, "key" => key)
    end

    def snapshot_create(dest = nil)
      @client.call_raw("type" => "snapshot_create", "name" => @name,
                       "dest" => dest, "include_memory" => false)
    end

    def snapshot_restore(snapshot, name)
      @client.call_raw("type" => "snapshot_restore", "snapshot" => snapshot, "name" => name)
    end

    def snapshot_fork(snapshot, name)
      @client.call_raw("type" => "snapshot_fork", "snapshot" => snapshot, "name" => name)
    end

    def snapshot_remove(snapshot)
      @client.call_raw("type" => "snapshot_remove", "snapshot" => snapshot)
    end
  end

  # A protocol client. Each call dials and re-negotiates (stateless).
  class SandboxClient
    attr_reader :socket

    def initialize(socket_path)
      @socket = socket_path
      conn = Conn.new(socket_path)
      begin
        @caps = conn.hello["capabilities"] || []
      ensure
        conn.close
      end
    end

    def self.connect(socket_path)
      new(socket_path)
    end

    def capabilities
      @caps.dup
    end

    def supports(capability)
      @caps.include?(capability)
    end

    def call_raw(message)
      conn = Conn.new(@socket)
      begin
        conn.call(message)
      ensure
        conn.close
      end
    end

    def ping
      call_raw("type" => "ping")["version"]
    end

    def list(all = false, label = nil)
      req = { "type" => "list", "all" => all }
      req["label"] = label unless label.nil?
      call_raw(req)["sandboxes"]
    end

    def list_with_label(label, all = false)
      call_raw("type" => "list", "all" => all, "label" => label)["sandboxes"]
    end

    def list_page(limit = nil, cursor = nil, label = nil)
      req = { "type" => "list", "all" => true }
      req["limit"] = limit unless limit.nil?
      req["cursor"] = cursor unless cursor.nil?
      req["label"] = label unless label.nil?
      call_raw(req)
    end

    def snapshots
      call_raw("type" => "snapshot_list")["snapshots"]
    end

    def get(name)
      call_raw("type" => "get", "name" => name)
    end

    def create(spec)
      call_raw("type" => "create", "spec" => spec)
      Sandbox.new(self, spec["name"])
    end

    def create_with(builder)
      spec = builder.build
      raise ArgumentError, "spec.name is required to create" unless spec.key?("name")

      create(spec)
    end

    def sandbox(name)
      Sandbox.new(self, name)
    end
  end
end