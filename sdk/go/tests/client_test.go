// client_test.go — unit tests for the Go SDK using an in-process fake NDJSON
// helper on a temp Unix socket (mirrors sdk/python/tests/test_client.py).
package tests

import (
	"errors"
	"fmt"
	"github.com/nanosandboxai/nanosandbox/sdk/go/nanosandbox"
	"net"
	"os"
	"strings"
	"sync"
	"testing"
)

var helperCounter int
var helperMu sync.Mutex

// nextHelperPath returns a short unique socket path under TMPDIR so the path
// stays under the Unix SUN_LEN limit (~103 bytes).
func nextHelperPath() string {
	helperMu.Lock()
	defer helperMu.Unlock()
	helperCounter++
	dir := os.Getenv("TMPDIR")
	if dir == "" {
		dir = "/tmp"
	}
	return fmt.Sprintf("%s/nanosb-go-%d-%d.sock", dir, os.Getpid(), helperCounter)
}

// FakeHelper is a scripted NDJSON helper on a temp Unix socket.
type FakeHelper struct {
	Path     string
	Listener net.Listener
	Queue    []string
	Requests []string
	Mu       sync.Mutex
}

// NewFakeHelper starts a fake helper listening on a temp socket.
func NewFakeHelper(t *testing.T) *FakeHelper {
	path := nextHelperPath()
	listener, err := net.Listen("unix", path)
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	h := &FakeHelper{Path: path, Listener: listener, Mu: sync.Mutex{}}
	go h.serve()
	return h
}

// Push queues a scripted response frame.
func (h *FakeHelper) Push(obj nanosandbox.JsonValue) {
	h.Mu.Lock()
	h.Queue = append(h.Queue, nanosandbox.JsonStringify(obj))
	h.Mu.Unlock()
}

func (h *FakeHelper) pop() string {
	h.Mu.Lock()
	defer h.Mu.Unlock()
	if len(h.Queue) == 0 {
		return ""
	}
	s := h.Queue[0]
	h.Queue = h.Queue[1:]
	return s
}

func (h *FakeHelper) serve() {
	for {
		conn, err := h.Listener.Accept()
		if err != nil {
			return
		}
		go h.handle(conn)
	}
}

func (h *FakeHelper) handle(conn net.Conn) {
	reader := nanosandbox.NewLineReader(conn)
	for {
		line, err := reader.ReadLine()
		if err != nil || line == "" {
			break
		}
		req, perr := nanosandbox.JsonParse(line)
		if perr != nil {
			break
		}
		h.Mu.Lock()
		h.Requests = append(h.Requests, line)
		h.Mu.Unlock()
		rtype, _ := req.Str("type")
		if rtype == "hello" {
			resp := nanosandbox.JsonObjectValue()
			resp.Set("type", nanosandbox.JsonStringValue("hello_ok"))
			resp.Set("protocol", nanosandbox.JsonStringValue(nanosandbox.PROTOCOL_VERSION))
			resp.Set("helper_version", nanosandbox.JsonStringValue("test"))
			caps := nanosandbox.JsonArrayValue()
			for _, c := range []string{"lifecycle", "exec", "fs", "logs", "metrics"} {
				caps.Append(nanosandbox.JsonStringValue(c))
			}
			resp.Set("capabilities", caps)
			conn.Write([]byte(nanosandbox.JsonStringify(resp) + "\n"))
			continue
		}
		streaming := isStreaming(req)
		scripted := h.pop()
		if scripted == "" {
			scripted = `{"type":"error","code":"internal","message":"no script"}`
		}
		conn.Write([]byte(scripted + "\n"))
		if streaming && !isTerminal(scripted) {
			for {
				nxt := h.pop()
				if nxt == "" {
					break
				}
				conn.Write([]byte(nxt + "\n"))
				if isTerminal(nxt) {
					break
				}
			}
		}
	}
	conn.Close()
}

func isStreaming(req nanosandbox.JsonValue) bool {
	rtype, _ := req.Str("type")
	if rtype == "exec_stream" || rtype == "attach" || rtype == "metrics_stream" {
		return true
	}
	if rtype == "logs" {
		follow, _ := req.Bool("follow")
		return follow
	}
	return false
}

func isTerminal(line string) bool {
	v, err := nanosandbox.JsonParse(line)
	if err != nil {
		return false
	}
	rtype, _ := v.Str("type")
	if rtype == "guest" {
		ev, _ := v.Str("event")
		return ev == "exit" || ev == "error"
	}
	if rtype == "event" {
		ev, _ := v.Str("event")
		return ev == "done"
	}
	return false
}

// Close stops the fake helper's listener and removes the socket file.
func (h *FakeHelper) Close() {
	h.Listener.Close()
	os.Remove(h.Path)
}

func withHelper(t *testing.T, fn func(*FakeHelper)) {
	h := NewFakeHelper(t)
	defer h.Close()
	fn(h)
}

func TestHandshakeAndCapabilities(t *testing.T) {
	withHelper(t, func(h *FakeHelper) {
		client, err := nanosandbox.Connect(h.Path)
		if err != nil {
			t.Fatalf("connect: %v", err)
		}
		if !containsStr(client.Capabilities(), "lifecycle") {
			t.Fail()
		}
		if !client.Supports("exec") {
			t.Fail()
		}
	})
}

func TestPing(t *testing.T) {
	withHelper(t, func(h *FakeHelper) {
		h.Push(nanosandbox.JsonParseOr(`{"type":"pong","version":"9"}`))
		client, err := nanosandbox.Connect(h.Path)
		if err != nil {
			t.Fatalf("connect: %v", err)
		}
		v, perr := client.Ping()
		if perr != nil || v != "9" {
			t.Fatalf("ping: %q %v", v, perr)
		}
	})
}

func TestList(t *testing.T) {
	withHelper(t, func(h *FakeHelper) {
		h.Push(nanosandbox.JsonParseOr(`{"type":"list","sandboxes":[{"name":"a","state":"running","image":"alpine"}]}`))
		client, err := nanosandbox.Connect(h.Path)
		if err != nil {
			t.Fatalf("connect: %v", err)
		}
		boxes, lerr := client.List(false)
		if lerr != nil {
			t.Fatalf("list: %v", lerr)
		}
		name, _ := boxes[0].Str("name")
		if name != "a" {
			t.Fatalf("list name: %q", name)
		}
	})
}

func TestExec(t *testing.T) {
	withHelper(t, func(h *FakeHelper) {
		h.Push(nanosandbox.JsonParseOr(`{"type":"exec_result","exit_code":0,"stdout":"hi\n","stderr":"","duration_ms":1}`))
		client, err := nanosandbox.Connect(h.Path)
		if err != nil {
			t.Fatalf("connect: %v", err)
		}
		out, xerr := client.Sandbox("a").Exec("echo", []string{"hi"})
		if xerr != nil {
			t.Fatalf("exec: %v", xerr)
		}
		stdout, _ := out.Str("stdout")
		if stdout != "hi\n" {
			t.Fatalf("stdout: %q", stdout)
		}
	})
}

func TestErrorIsTyped(t *testing.T) {
	withHelper(t, func(h *FakeHelper) {
		h.Push(nanosandbox.JsonParseOr(`{"type":"error","code":"not_found","message":"nope"}`))
		client, err := nanosandbox.Connect(h.Path)
		if err != nil {
			t.Fatalf("connect: %v", err)
		}
		_, gerr := client.Get("missing")
		ne, ok := errorsAsNanosb(gerr)
		if !ok || !ne.IsNotFound() {
			t.Fatalf("expected not_found, got %v", gerr)
		}
	})
}

func TestFsWriteThenRead(t *testing.T) {
	withHelper(t, func(h *FakeHelper) {
		h.Push(nanosandbox.JsonParseOr(`{"type":"ok","version":"1"}`))
		h.Push(nanosandbox.JsonParseOr(`{"type":"fs_data","data_base64":"aGVsbG8=","eof":true}`))
		client, err := nanosandbox.Connect(h.Path)
		if err != nil {
			t.Fatalf("connect: %v", err)
		}
		box := client.Sandbox("a")
		werr := box.FsWrite("/tmp/x", []byte("hello"), false)
		if werr != nil {
			t.Fatalf("write: %v", werr)
		}
		data, rerr := box.FsRead("/tmp/x")
		if rerr != nil {
			t.Fatalf("read: %v", rerr)
		}
		if string(data) != "hello" {
			t.Fatalf("read bytes: %q", string(data))
		}
	})
}

func TestExecStream(t *testing.T) {
	withHelper(t, func(h *FakeHelper) {
		h.Push(nanosandbox.JsonParseOr(`{"type":"ok","version":"1"}`))
		h.Push(nanosandbox.JsonParseOr(`{"type":"guest","event":"stdout","data":"a"}`))
		h.Push(nanosandbox.JsonParseOr(`{"type":"guest","event":"exit","code":0}`))
		client, err := nanosandbox.Connect(h.Path)
		if err != nil {
			t.Fatalf("connect: %v", err)
		}
		var events []nanosandbox.JsonValue
		code, serr := client.Sandbox("a").ExecStream(
			nanosandbox.JsonObjectValue().SetT("command", nanosandbox.JsonStringValue("x")),
			func(ev nanosandbox.JsonValue) {
				events = append(events, ev)
			},
		)
		if serr != nil {
			t.Fatalf("exec_stream: %v", serr)
		}
		if code != 0 {
			t.Fatalf("exit code: %d", code)
		}
		data, _ := events[0].Str("data")
		if data != "a" {
			t.Fatalf("event data: %q", data)
		}
	})
}

func TestBuilder(t *testing.T) {
	b := nanosandbox.NewSandboxBuilder("alpine")
	b.WithName("demo")
	b.WithResources(2, 512)
	b.PublishTcp(8080, 80)
	b.WithLabel("team", "sdk")
	spec := b.Build()
	name, _ := spec.Str("name")
	if name != "demo" {
		t.Fatalf("name: %q", name)
	}
	network, _ := spec.Get("network")
	pub, _ := network.GetArr("publish")
	proto, _ := pub[0].Str("protocol")
	if proto != "tcp" {
		t.Fatalf("protocol: %q", proto)
	}
	labels, _ := spec.Get("labels")
	team, _ := labels.Str("team")
	if team != "sdk" {
		t.Fatalf("label: %q", team)
	}
}

func TestBuilderMountAndSecretMethods(t *testing.T) {
	b := nanosandbox.NewSandboxBuilder("alpine")
	b.SetDetached(false)
	b.SetHostname("box")
	b.MountFile("/host/f", "/guest/f")
	b.MountTmpfs("/tmp")
	b.MountOptions([]string{"noexec", "nosuid"})
	b.MountOwner(1000, 1000)
	b.SetSecretEnv("API_KEY", "api-key")
	spec := b.Build()
	detached, _ := spec.Bool("detached")
	if detached {
		t.Fail()
	}
	hostname, _ := spec.Str("hostname")
	if hostname != "box" {
		t.Fatalf("hostname: %q", hostname)
	}
	mounts, _ := spec.GetArr("mounts")
	file, _ := mounts[0].Bool("file")
	if !file {
		t.Fail()
	}
	tmpfs, _ := mounts[1].Bool("tmpfs")
	if !tmpfs {
		t.Fail()
	}
	mtype, _ := mounts[1].Str("mount_type")
	if mtype != "tmpfs" {
		t.Fatalf("mount_type: %q", mtype)
	}
	opts, _ := mounts[1].GetArr("options")
	if len(opts) != 2 {
		t.Fatalf("options len: %d", len(opts))
	}
	uid, _ := mounts[1].Int("uid")
	if uid != 1000 {
		t.Fatalf("uid: %d", uid)
	}
	secretEnv, _ := spec.Get("secret_env")
	sk, _ := secretEnv.Str("API_KEY")
	if sk != "api-key" {
		t.Fatalf("secret_env: %q", sk)
	}
}

func TestVolumeBuilder(t *testing.T) {
	b := nanosandbox.NewVolumeBuilder("data")
	b.WithDir("/tmp/data")
	b.VolumeQuota(1024)
	vol := b.Build()
	name, _ := vol.Str("name")
	if name != "data" {
		t.Fatalf("name: %q", name)
	}
	dir, _ := vol.Str("dir")
	if dir != "/tmp/data" {
		t.Fatalf("dir: %q", dir)
	}
	quota, _ := vol.Int("quota_bytes")
	if quota != 1024 {
		t.Fatalf("quota: %d", quota)
	}
}

func TestModify(t *testing.T) {
	withHelper(t, func(h *FakeHelper) {
		h.Push(nanosandbox.JsonParseOr(`{"type":"ok","version":"1"}`))
		client, err := nanosandbox.Connect(h.Path)
		if err != nil {
			t.Fatalf("connect: %v", err)
		}
		spec := nanosandbox.JsonObjectValue()
		spec.Set("idle_timeout_secs", nanosandbox.JsonIntValue(300))
		labels := nanosandbox.JsonObjectValue()
		labels.Set("team", nanosandbox.JsonStringValue("sdk"))
		spec.Set("labels", labels)
		merr := client.Sandbox("a").Modify(spec)
		if merr != nil {
			t.Fatalf("modify: %v", merr)
		}
	})
}

func TestWait(t *testing.T) {
	withHelper(t, func(h *FakeHelper) {
		h.Push(nanosandbox.JsonParseOr(`{"type":"wait","exit_code":137,"exited_at_ms":1234,"state":"stopped"}`))
		client, err := nanosandbox.Connect(h.Path)
		if err != nil {
			t.Fatalf("connect: %v", err)
		}
		secs := 30.0
		res, werr := client.Sandbox("a").Wait(&secs)
		if werr != nil {
			t.Fatalf("wait: %v", werr)
		}
		if res.ExitCode != 137 {
			t.Fatalf("exit_code: %d", res.ExitCode)
		}
		if res.ExitedAtMs != 1234 {
			t.Fatalf("exited_at_ms: %d", res.ExitedAtMs)
		}
		if res.State != "stopped" {
			t.Fatalf("state: %q", res.State)
		}
	})
}

func TestWaitNilTimeoutOmitsTimeoutSecs(t *testing.T) {
	withHelper(t, func(h *FakeHelper) {
		h.Push(nanosandbox.JsonParseOr(`{"type":"wait","exit_code":0,"exited_at_ms":1,"state":"stopped"}`))
		client, err := nanosandbox.Connect(h.Path)
		if err != nil {
			t.Fatalf("connect: %v", err)
		}
		_, werr := client.Sandbox("a").Wait(nil)
		if werr != nil {
			t.Fatalf("wait: %v", werr)
		}
	})
}

func TestWaitErrorResponse(t *testing.T) {
	withHelper(t, func(h *FakeHelper) {
		h.Push(nanosandbox.JsonParseOr(`{"type":"error","code":"timeout","message":"timed out"}`))
		client, err := nanosandbox.Connect(h.Path)
		if err != nil {
			t.Fatalf("connect: %v", err)
		}
		_, werr := client.Sandbox("a").Wait(nil)
		if werr == nil {
			t.Fatalf("expected error, got nil")
		}
		ne, ok := errorsAsNanosb(werr)
		if !ok || ne.Code != "timeout" {
			t.Fatalf("expected timeout error, got %v", werr)
		}
	})
}

func TestKillWithSendsSignal(t *testing.T) {
	withHelper(t, func(h *FakeHelper) {
		h.Push(nanosandbox.JsonParseOr(`{"type":"ok","version":"1"}`))
		client, err := nanosandbox.Connect(h.Path)
		if err != nil {
			t.Fatalf("connect: %v", err)
		}
		kerr := client.Sandbox("a").KillWith(15)
		if kerr != nil {
			t.Fatalf("kill_with: %v", kerr)
		}
		if !strings.Contains(strings.Join(h.Requests, "\n"), `"signal":15`) {
			t.Fatalf("kill frame missing signal: %v", h.Requests)
		}
	})
}

func TestRestartSendsRestart(t *testing.T) {
	withHelper(t, func(h *FakeHelper) {
		h.Push(nanosandbox.JsonParseOr(`{"type":"ok","version":"1"}`))
		client, err := nanosandbox.Connect(h.Path)
		if err != nil {
			t.Fatalf("connect: %v", err)
		}
		rerr := client.Sandbox("a").Restart()
		if rerr != nil {
			t.Fatalf("restart: %v", rerr)
		}
		if !strings.Contains(strings.Join(h.Requests, "\n"), `"type":"restart"`) {
			t.Fatalf("restart frame missing type: %v", h.Requests)
		}
	})
}

func TestWaitUntilStopped(t *testing.T) {
	withHelper(t, func(h *FakeHelper) {
		h.Push(nanosandbox.JsonParseOr(`{"type":"wait","exit_code":0,"exited_at_ms":1,"state":"stopped"}`))
		client, err := nanosandbox.Connect(h.Path)
		if err != nil {
			t.Fatalf("connect: %v", err)
		}
		st, werr := client.Sandbox("a").WaitUntilStopped(5)
		if werr != nil {
			t.Fatalf("wait: %v", werr)
		}
		if st != "stopped" {
			t.Fatalf("state: %q", st)
		}
	})
}

func TestShellStream(t *testing.T) {
	withHelper(t, func(h *FakeHelper) {
		h.Push(nanosandbox.JsonParseOr(`{"type":"ok","version":"1"}`))
		h.Push(nanosandbox.JsonParseOr(`{"type":"guest","event":"stdout","data":"hi"}`))
		h.Push(nanosandbox.JsonParseOr(`{"type":"guest","event":"exit","code":0}`))
		client, err := nanosandbox.Connect(h.Path)
		if err != nil {
			t.Fatalf("connect: %v", err)
		}
		var out []nanosandbox.JsonValue
		code, serr := client.Sandbox("a").ShellStream("echo hi", func(ev nanosandbox.JsonValue) {
			out = append(out, ev)
		})
		if serr != nil {
			t.Fatalf("shell_stream: %v", serr)
		}
		if code != 0 {
			t.Fatalf("exit code: %d", code)
		}
		data, _ := out[0].Str("data")
		if data != "hi" {
			t.Fatalf("data: %q", data)
		}
	})
}

func TestAttachSessionDrivesInputResizeAndReadsEvents(t *testing.T) {
	withHelper(t, func(h *FakeHelper) {
		h.Push(nanosandbox.JsonParseOr(`{"type":"ok","version":"1"}`))
		h.Push(nanosandbox.JsonParseOr(`{"type":"ok","version":"1"}`))
		h.Push(nanosandbox.JsonParseOr(`{"type":"ok","version":"1"}`))
		h.Push(nanosandbox.JsonParseOr(`{"type":"guest","event":"stdout","data":"hi"}`))
		h.Push(nanosandbox.JsonParseOr(`{"type":"guest","event":"exit","code":0}`))
		client, err := nanosandbox.Connect(h.Path)
		if err != nil {
			t.Fatalf("connect: %v", err)
		}
		session, aerr := client.Sandbox("a").AttachSession()
		if aerr != nil {
			t.Fatalf("attach: %v", aerr)
		}
		defer session.Close()
		ierr := session.SendInput([]byte("x"))
		if ierr != nil {
			t.Fatalf("send_input: %v", ierr)
		}
		rerr := session.Resize(80, 24)
		if rerr != nil {
			t.Fatalf("resize: %v", rerr)
		}
		var out []string
		for {
			ev, eerr := session.ReadEvents()
			if eerr != nil {
				t.Fatalf("read_events: %v", eerr)
			}
			if ev.IsNull() {
				break
			}
			data, _ := ev.Str("data")
			out = append(out, data)
		}
		if len(out) != 1 || out[0] != "hi" {
			t.Fatalf("events: %q", strings.Join(out, ","))
		}
	})
}

func TestMetricsStream(t *testing.T) {
	withHelper(t, func(h *FakeHelper) {
		h.Push(nanosandbox.JsonParseOr(`{"type":"ok","version":"1"}`))
		h.Push(nanosandbox.JsonParseOr(`{"type":"event","event":"metrics","cpu_percent":1.5,"memory_used_bytes":10,"memory_total_bytes":100,"disk_used_bytes":0,"disk_total_bytes":0,"net_rx_bytes":0,"net_tx_bytes":0,"timestamp_ms":0}`))
		h.Push(nanosandbox.JsonParseOr(`{"type":"event","event":"done"}`))
		client, err := nanosandbox.Connect(h.Path)
		if err != nil {
			t.Fatalf("connect: %v", err)
		}
		var samples []nanosandbox.JsonValue
		merr := client.Sandbox("a").MetricsStream(100, func(sample nanosandbox.JsonValue) {
			samples = append(samples, sample)
		})
		if merr != nil {
			t.Fatalf("metrics_stream: %v", merr)
		}
		if len(samples) != 1 {
			t.Fatalf("samples: %d", len(samples))
		}
		cpu, _ := samples[0].Num("cpu_percent")
		if cpu != 1.5 {
			t.Fatalf("cpu: %v", cpu)
		}
	})
}

func TestLogsWithCursorAndSource(t *testing.T) {
	withHelper(t, func(h *FakeHelper) {
		h.Push(nanosandbox.JsonParseOr(`{"type":"logs","lines":[{"cursor":5,"timestamp_ms":0,"source":"stdout","data":"x"}],"next_cursor":6}`))
		client, err := nanosandbox.Connect(h.Path)
		if err != nil {
			t.Fatalf("connect: %v", err)
		}
		lines, lerr := client.Sandbox("a").LogsWith(10, 5, "stdout")
		if lerr != nil {
			t.Fatalf("logs: %v", lerr)
		}
		if len(lines) != 1 {
			t.Fatalf("lines: %d", len(lines))
		}
		cursor, _ := lines[0].Int("cursor")
		if cursor != 5 {
			t.Fatalf("cursor: %d", cursor)
		}
	})
}

func TestLogsFollowWithCursorAndSource(t *testing.T) {
	withHelper(t, func(h *FakeHelper) {
		h.Push(nanosandbox.JsonParseOr(`{"type":"ok","version":"1"}`))
		h.Push(nanosandbox.JsonParseOr(`{"type":"event","event":"log","cursor":5,"timestamp_ms":0,"source":"stdout","data":"x"}`))
		h.Push(nanosandbox.JsonParseOr(`{"type":"event","event":"done"}`))
		client, err := nanosandbox.Connect(h.Path)
		if err != nil {
			t.Fatalf("connect: %v", err)
		}
		var out []nanosandbox.JsonValue
		lerr := client.Sandbox("a").LogsFollowWith(func(line nanosandbox.JsonValue) {
			out = append(out, line)
		}, 10, 5, "stdout")
		if lerr != nil {
			t.Fatalf("logs_follow: %v", lerr)
		}
		if len(out) != 1 {
			t.Fatalf("lines: %d", len(out))
		}
		data, _ := out[0].Str("data")
		if data != "x" {
			t.Fatalf("data: %q", data)
		}
	})
}

func TestFsReadRange(t *testing.T) {
	withHelper(t, func(h *FakeHelper) {
		h.Push(nanosandbox.JsonParseOr(`{"type":"fs_data","data_base64":"aGVsbG8=","eof":true}`))
		client, err := nanosandbox.Connect(h.Path)
		if err != nil {
			t.Fatalf("connect: %v", err)
		}
		data, rerr := client.Sandbox("a").FsReadRange("/tmp/x", 0, 5)
		if rerr != nil {
			t.Fatalf("read_range: %v", rerr)
		}
		if string(data) != "hello" {
			t.Fatalf("bytes: %q", string(data))
		}
	})
}

func TestFsReadStreamChunks(t *testing.T) {
	withHelper(t, func(h *FakeHelper) {
		h.Push(nanosandbox.JsonParseOr(`{"type":"fs_data","data_base64":"aGVsbG8=","eof":true}`))
		h.Push(nanosandbox.JsonParseOr(`{"type":"fs_data","data_base64":"","eof":true}`))
		client, err := nanosandbox.Connect(h.Path)
		if err != nil {
			t.Fatalf("connect: %v", err)
		}
		var out []byte
		serr := client.Sandbox("a").FsReadStream("/tmp/x", 3, func(chunk []byte) {
			for i := 0; i < len(chunk); i++ {
				out = append(out, chunk[i])
			}
		})
		if serr != nil {
			t.Fatalf("read_stream: %v", serr)
		}
		if string(out) != "hello" {
			t.Fatalf("bytes: %q", string(out))
		}
	})
}

func TestFsWriteStream(t *testing.T) {
	withHelper(t, func(h *FakeHelper) {
		h.Push(nanosandbox.JsonParseOr(`{"type":"ok","version":"1"}`))
		h.Push(nanosandbox.JsonParseOr(`{"type":"ok","version":"1"}`))
		h.Push(nanosandbox.JsonParseOr(`{"type":"ok","version":"1"}`))
		client, err := nanosandbox.Connect(h.Path)
		if err != nil {
			t.Fatalf("connect: %v", err)
		}
		werr := client.Sandbox("a").FsWriteStream("/tmp/big.bin", []byte("chunk1chunk2"), 6)
		if werr != nil {
			t.Fatalf("write_stream: %v", werr)
		}
	})
}

func TestVolumeGetRemoveAndFs(t *testing.T) {
	withHelper(t, func(h *FakeHelper) {
		h.Push(nanosandbox.JsonParseOr(`{"type":"volume","name":"data","dir":"/tmp/vol-data","used_bytes":0}`))
		h.Push(nanosandbox.JsonParseOr(`{"type":"volume","name":"data","dir":"/tmp/vol-data","used_bytes":0}`))
		h.Push(nanosandbox.JsonParseOr(`{"type":"ok","version":"1"}`))
		client, err := nanosandbox.Connect(h.Path)
		if err != nil {
			t.Fatalf("connect: %v", err)
		}
		box := client.Sandbox("a")
		info, gerr := box.VolumeGet("data")
		if gerr != nil {
			t.Fatalf("volume_get: %v", gerr)
		}
		name, _ := info.Str("name")
		if name != "data" {
			t.Fatalf("name: %q", name)
		}
		dir, ferr := box.VolumeFs("data")
		if ferr != nil {
			t.Fatalf("volume_fs: %v", ferr)
		}
		if dir != "/tmp/vol-data" {
			t.Fatalf("dir: %q", dir)
		}
		verr := box.VolumeRemove("data")
		if verr != nil {
			t.Fatalf("volume_remove: %v", verr)
		}
	})
}

func TestMountAdd(t *testing.T) {
	withHelper(t, func(h *FakeHelper) {
		h.Push(nanosandbox.JsonParseOr(`{"type":"ok","version":"1"}`))
		client, err := nanosandbox.Connect(h.Path)
		if err != nil {
			t.Fatalf("connect: %v", err)
		}
		mount := nanosandbox.JsonObjectValue()
		mount.Set("host_path", nanosandbox.JsonStringValue("data"))
		mount.Set("guest_path", nanosandbox.JsonStringValue("/mnt/data"))
		mount.Set("mount_type", nanosandbox.JsonStringValue("bind"))
		merr := client.Sandbox("a").MountAdd(mount)
		if merr != nil {
			t.Fatalf("mount_add: %v", merr)
		}
	})
}

func TestSecretRemove(t *testing.T) {
	withHelper(t, func(h *FakeHelper) {
		h.Push(nanosandbox.JsonParseOr(`{"type":"ok","version":"1"}`))
		client, err := nanosandbox.Connect(h.Path)
		if err != nil {
			t.Fatalf("connect: %v", err)
		}
		serr := client.Sandbox("a").SecretRemove("API_KEY")
		if serr != nil {
			t.Fatalf("secret_remove: %v", serr)
		}
	})
}

func TestSnapshotRestoreForkRemove(t *testing.T) {
	withHelper(t, func(h *FakeHelper) {
		h.Push(nanosandbox.JsonParseOr(`{"type":"ok","version":"1"}`))
		h.Push(nanosandbox.JsonParseOr(`{"type":"ok","version":"1"}`))
		h.Push(nanosandbox.JsonParseOr(`{"type":"ok","version":"1"}`))
		client, err := nanosandbox.Connect(h.Path)
		if err != nil {
			t.Fatalf("connect: %v", err)
		}
		box := client.Sandbox("a")
		rerr := box.SnapshotRestore("snap-1", "box")
		if rerr != nil {
			t.Fatalf("restore: %v", rerr)
		}
		ferr := box.SnapshotFork("snap-1", "forked")
		if ferr != nil {
			t.Fatalf("fork: %v", ferr)
		}
		serr := box.SnapshotRemove("snap-1")
		if serr != nil {
			t.Fatalf("remove: %v", serr)
		}
	})
}

func TestListPageReturnsNextCursor(t *testing.T) {
	withHelper(t, func(h *FakeHelper) {
		h.Push(nanosandbox.JsonParseOr(`{"type":"list","sandboxes":[{"name":"a","state":"running","image":"alpine"}],"next_cursor":"abc"}`))
		client, err := nanosandbox.Connect(h.Path)
		if err != nil {
			t.Fatalf("connect: %v", err)
		}
		page, perr := client.ListPage(10, "prev", "team=sdk")
		if perr != nil {
			t.Fatalf("list_page: %v", perr)
		}
		boxes, _ := page.GetArr("sandboxes")
		if len(boxes) != 1 {
			t.Fatalf("boxes: %d", len(boxes))
		}
		cursor, _ := page.Str("next_cursor")
		if cursor != "abc" {
			t.Fatalf("cursor: %q", cursor)
		}
	})
}

func TestListWithLabel(t *testing.T) {
	withHelper(t, func(h *FakeHelper) {
		h.Push(nanosandbox.JsonParseOr(`{"type":"list","sandboxes":[{"name":"a","state":"running","image":"alpine"}]}`))
		client, err := nanosandbox.Connect(h.Path)
		if err != nil {
			t.Fatalf("connect: %v", err)
		}
		boxes, lerr := client.ListWithLabel("team=sdk", false)
		if lerr != nil {
			t.Fatalf("list: %v", lerr)
		}
		name, _ := boxes[0].Str("name")
		if name != "a" {
			t.Fatalf("name: %q", name)
		}
	})
}

func TestSnapshots(t *testing.T) {
	withHelper(t, func(h *FakeHelper) {
		h.Push(nanosandbox.JsonParseOr(`{"type":"snapshots","snapshots":[{"id":"snap-1","sandbox":"box","created_at_ms":1000}]}`))
		client, err := nanosandbox.Connect(h.Path)
		if err != nil {
			t.Fatalf("connect: %v", err)
		}
		snaps, serr := client.Snapshots()
		if serr != nil {
			t.Fatalf("snapshots: %v", serr)
		}
		id, _ := snaps[0].Str("id")
		if id != "snap-1" {
			t.Fatalf("id: %q", id)
		}
	})
}

func containsStr(haystack []string, needle string) bool {
	for _, s := range haystack {
		if s == needle {
			return true
		}
	}
	return false
}

func errorsAsNanosb(err error) (*nanosandbox.NanosbError, bool) {
	return errors.AsType[*nanosandbox.NanosbError](err)
}
