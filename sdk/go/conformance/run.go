// conformance/run.go — conformance driver for the Go SDK.
//
// Runs every scenario in `sdk/conformance/scenarios/` against the
// `nanosb-conformance` harness and evaluates the scenario `assert` blocks per
// `sdk/conformance/SPEC.md`. Exits non-zero if any scenario fails.
//
// Usage:
//     go run ./sdk/go/conformance/run.go
//     NANOSB_CONFORMANCE_BIN=/path/to/nanosb-conformance go run ./sdk/go/conformance/run.go
//
// allow: SIZE_OK — single-purpose driver mirroring the 568-line Python
// reference (sdk/python/conformance/run.py); the assert evaluator is a
// data-table of per-key checks, not reusable library code.
package main

import (
	"encoding/base64"
	"errors"
	"fmt"
	"github.com/nanosandboxai/nanosandbox/sdk/go/nanosandbox"
	"os"
	"strings"
	"time"
)

const SCENARIO_BUDGET_SECS = 60
const READY_TIMEOUT_MS = 15000
const RECV_TIMEOUT_MS = 10000

// ErrorPair records one observed protocol error.
type ErrorPair struct {
	Code    string
	Message string
}

// DriverState accumulates observations from driving a scenario's steps.
type DriverState struct {
	Acked             []string
	Errors            []ErrorPair
	ExitCode          int
	Stdout            string
	Stderr            string
	GetResult         nanosandbox.JsonValue
	ListResult        []nanosandbox.JsonValue
	FsEntries         []nanosandbox.JsonValue
	FsStat            nanosandbox.JsonValue
	FsExists          bool
	FsReadBytes       []byte
	FsContent         string
	ChunksAcked       int
	FirstLogs         nanosandbox.JsonValue
	ResumeLogs        nanosandbox.JsonValue
	FollowLines       []nanosandbox.JsonValue
	Metrics           nanosandbox.JsonValue
	MetricsSamples    []nanosandbox.JsonValue
	VolumeCreated     nanosandbox.JsonValue
	Volumes           []nanosandbox.JsonValue
	VolumeGet         nanosandbox.JsonValue
	Secrets           []nanosandbox.JsonValue
	SnapshotCreated   nanosandbox.JsonValue
	Snapshots         []nanosandbox.JsonValue
	AttachEvents      []nanosandbox.JsonValue
	AttachExitCode    int
	PingOk            bool
	WaitResult        nanosandbox.WaitResult
	WaitSet           bool
	TimeoutCode       string
	UnsupportedObserved bool
}

func hasAck(state *DriverState, name string) bool {
	for _, a := range state.Acked {
		if a == name {
			return true
		}
	}
	return false
}

func addAck(state *DriverState, name string) {
	state.Acked = append(state.Acked, name)
}

// reqFromExpect returns the request fields for a verb, minus the type/name
// routing keys.
func reqFromExpect(expect nanosandbox.JsonValue) nanosandbox.JsonValue {
	out := nanosandbox.JsonObjectValue()
	for _, pair := range expect.AsObj() {
		if pair.Key != "type" && pair.Key != "name" {
			out.Set(pair.Key, pair.Val)
		}
	}
	return out
}

// execStreamEvent returns an event handler that accumulates stdout/stderr.
func execStreamEvent(state *DriverState) func(nanosandbox.JsonValue) {
	return func(ev nanosandbox.JsonValue) {
		event, _ := ev.Str("event")
		data, _ := ev.Str("data")
		if event == "stdout" {
			state.Stdout += data
		} else if event == "stderr" {
			state.Stderr += data
		}
	}
}

// recordError records a NanosbError observation without aborting.
func recordError(state *DriverState, err error) {
	ne, ok := errors.AsType[*nanosandbox.NanosbError](err)
	if ok {
		state.Errors = append(state.Errors, ErrorPair{Code: ne.Code, Message: ne.Message})
	}
}

// driveStep invokes the SDK operation mapped from `expect.type` (SPEC §5).
func driveStep(client *nanosandbox.SandboxClient, state *DriverState, expect nanosandbox.JsonValue) error {
	etype, _ := expect.Str("type")
	name, _ := expect.Str("name")
	box := client.Sandbox(name)
	switch etype {
	case "create":
		spec, _ := expect.Get("spec")
		_, err := client.Create(spec)
		if err != nil {
			return err
		}
		addAck(state, "create")
	case "get":
		resp, err := client.Get(name)
		if err != nil {
			return err
		}
		state.GetResult = resp
	case "list":
		all, _ := expect.Bool("all")
		resp, err := client.List(all)
		if err != nil {
			return err
		}
		state.ListResult = resp
	case "start":
		err := box.Start()
		if err != nil {
			return err
		}
		addAck(state, "start")
	case "stop":
		force, _ := expect.Bool("force")
		err := box.Stop(force)
		if err != nil {
			return err
		}
		addAck(state, "stop")
	case "kill":
		if expect.Has("signal") {
			sig, _ := expect.Int("signal")
			err := box.KillWith(int(sig))
			if err != nil {
				return err
			}
		} else {
			err := box.Kill()
			if err != nil {
				return err
			}
		}
		addAck(state, "kill")
	case "wait":
		var timeout *float64
		if expect.Has("timeout_secs") {
			t, _ := expect.Num("timeout_secs")
			timeout = &t
		}
		res, err := box.Wait(timeout)
		if err != nil {
			return err
		}
		state.WaitResult = res
		state.WaitSet = true
		addAck(state, "wait")
	case "restart":
		err := box.Restart()
		if err != nil {
			return err
		}
		addAck(state, "restart")
	case "remove":
		force, _ := expect.Bool("force")
		err := box.Remove(force)
		if err != nil {
			return err
		}
		addAck(state, "remove")
	case "drain":
		err := box.Drain()
		if err != nil {
			return err
		}
		addAck(state, "drain")
	case "touch":
		err := box.Touch()
		if err != nil {
			return err
		}
		addAck(state, "touch")
	case "pause":
		err := box.Pause()
		if err != nil {
			return err
		}
		addAck(state, "pause")
	case "resume":
		err := box.Resume()
		if err != nil {
			return err
		}
		addAck(state, "resume")
	case "modify":
		spec, _ := expect.Get("spec")
		err := box.Modify(spec)
		if err != nil {
			return err
		}
		addAck(state, "modify")
	case "status":
		_, err := box.State()
		if err != nil {
			return err
		}
	case "ping":
		_, err := client.Ping()
		if err != nil {
			return err
		}
		state.PingOk = true
	case "exec":
		result, err := box.ExecWith(reqFromExpect(expect))
		if err != nil {
			return err
		}
		code, _ := result.Int("exit_code")
		state.ExitCode = int(code)
		stdout, _ := result.Str("stdout")
		stderr, _ := result.Str("stderr")
		state.Stdout = stdout
		state.Stderr = stderr
	case "exec_stream":
		code, err := box.ExecStream(reqFromExpect(expect), execStreamEvent(state))
		if err != nil {
			return err
		}
		state.ExitCode = code
	case "shell":
		script, _ := expect.Str("script")
		result, err := box.Shell(script)
		if err != nil {
			return err
		}
		code, _ := result.Int("exit_code")
		state.ExitCode = int(code)
		stdout, _ := result.Str("stdout")
		stderr, _ := result.Str("stderr")
		state.Stdout = stdout
		state.Stderr = stderr
	case "attach":
		err := box.Attach(func(ev nanosandbox.JsonValue) {
			state.AttachEvents = append(state.AttachEvents, ev)
		})
		if err != nil {
			return err
		}
		state.AttachExitCode = -1
		for _, ev := range state.AttachEvents {
			event, _ := ev.Str("event")
			if event == "exit" {
				code, _ := ev.Int("code")
				state.AttachExitCode = int(code)
			}
		}
	case "attach_input":
		data, _ := expect.Str("data")
		req := nanosandbox.JsonObjectValue()
		req.Set("type", nanosandbox.JsonStringValue("attach_input"))
		req.Set("data", nanosandbox.JsonStringValue(data))
		_, err := client.Call(req)
		if err != nil {
			return err
		}
		addAck(state, "input")
	case "attach_resize":
		cols, _ := expect.Int("cols")
		rows, _ := expect.Int("rows")
		req := nanosandbox.JsonObjectValue()
		req.Set("type", nanosandbox.JsonStringValue("attach_resize"))
		req.Set("cols", nanosandbox.JsonIntValue(cols))
		req.Set("rows", nanosandbox.JsonIntValue(rows))
		_, err := client.Call(req)
		if err != nil {
			return err
		}
		addAck(state, "resize")
	case "fs_read":
		path, _ := expect.Str("path")
		data, err := box.FsRead(path)
		if err != nil {
			return err
		}
		state.FsReadBytes = data
	case "fs_read_to_string":
		path, _ := expect.Str("path")
		content, err := box.FsReadToString(path)
		if err != nil {
			return err
		}
		state.FsContent = content
	case "fs_write":
		path, _ := expect.Str("path")
		b64, _ := expect.Str("data_base64")
		data, derr := nanosandbox.Unb64(b64)
		if derr != nil {
			return derr
		}
		err := box.FsWrite(path, data, false)
		if err != nil {
			return err
		}
		addAck(state, "write")
	case "fs_list":
		path, _ := expect.Str("path")
		entries, err := box.FsList(path)
		if err != nil {
			return err
		}
		state.FsEntries = entries
	case "fs_stat":
		path, _ := expect.Str("path")
		stat, err := box.FsStat(path)
		if err != nil {
			return err
		}
		state.FsStat = stat
	case "fs_exists":
		path, _ := expect.Str("path")
		exists, err := box.FsExists(path)
		if err != nil {
			return err
		}
		state.FsExists = exists
	case "fs_mkdir":
		path, _ := expect.Str("path")
		recursive, _ := expect.Bool("recursive")
		err := box.FsMkdir(path, recursive)
		if err != nil {
			return err
		}
		addAck(state, "mkdir")
	case "fs_remove":
		path, _ := expect.Str("path")
		err := box.FsRemove(path)
		if err != nil {
			return err
		}
	case "fs_remove_dir":
		path, _ := expect.Str("path")
		recursive, _ := expect.Bool("recursive")
		err := box.FsRemoveDir(path, recursive)
		if err != nil {
			return err
		}
		addAck(state, "remove_dir")
	case "fs_copy":
		from, _ := expect.Str("from")
		to, _ := expect.Str("to")
		err := box.FsCopy(from, to)
		if err != nil {
			return err
		}
		addAck(state, "copy")
	case "fs_rename":
		from, _ := expect.Str("from")
		to, _ := expect.Str("to")
		err := box.FsRename(from, to)
		if err != nil {
			return err
		}
		addAck(state, "rename")
	case "fs_copy_from_host":
		hostPath, _ := expect.Str("host_path")
		guestPath, _ := expect.Str("guest_path")
		err := box.FsCopyFromHost(hostPath, guestPath)
		if err != nil {
			return err
		}
		addAck(state, "copy_from_host")
	case "fs_copy_to_host":
		guestPath, _ := expect.Str("guest_path")
		hostPath, _ := expect.Str("host_path")
		err := box.FsCopyToHost(guestPath, hostPath)
		if err != nil {
			return err
		}
		addAck(state, "copy_to_host")
	case "logs":
		follow, _ := expect.Bool("follow")
		if follow {
			tail, _ := expect.Int("tail")
			cursor, _ := expect.Int("cursor")
			source, _ := expect.Str("source")
			err := box.LogsFollowWith(func(line nanosandbox.JsonValue) {
				state.FollowLines = append(state.FollowLines, line)
			}, int(tail), int(cursor), source)
			if err != nil {
				return err
			}
		} else {
			req := nanosandbox.JsonObjectValue()
			req.Set("type", nanosandbox.JsonStringValue("logs"))
			req.Set("name", nanosandbox.JsonStringValue(name))
			req.Set("follow", nanosandbox.JsonBoolValue(false))
			if expect.Has("tail") {
				tail, _ := expect.Int("tail")
				req.Set("tail", nanosandbox.JsonIntValue(tail))
			}
			if expect.Has("cursor") {
				cursor, _ := expect.Int("cursor")
				req.Set("cursor", nanosandbox.JsonIntValue(cursor))
			}
			if expect.Has("source") {
				source, _ := expect.Str("source")
				req.Set("source", nanosandbox.JsonStringValue(source))
			}
			resp, err := client.Call(req)
			if err != nil {
				return err
			}
			if state.FirstLogs.IsNull() {
				state.FirstLogs = resp
			} else {
				state.ResumeLogs = resp
			}
		}
	case "metrics":
		resp, err := box.Metrics()
		if err != nil {
			return err
		}
		state.Metrics = resp
	case "metrics_stream":
		interval, _ := expect.Int("interval_ms")
		err := box.MetricsStream(int(interval), func(sample nanosandbox.JsonValue) {
			state.MetricsSamples = append(state.MetricsSamples, sample)
		})
		if err != nil {
			return err
		}
	case "volume_create":
		vol, _ := expect.Get("volume")
		vname, _ := vol.Str("name")
		vdir, _ := vol.Str("dir")
		resp, err := box.VolumeCreate(vname, vdir)
		if err != nil {
			return err
		}
		state.VolumeCreated = resp
	case "volume_list":
		vols, err := box.VolumeList()
		if err != nil {
			return err
		}
		state.Volumes = vols
	case "volume_get":
		volume, _ := expect.Str("volume")
		resp, err := box.VolumeGet(volume)
		if err != nil {
			return err
		}
		state.VolumeGet = resp
	case "volume_remove":
		volume, _ := expect.Str("volume")
		err := box.VolumeRemove(volume)
		if err != nil {
			return err
		}
		addAck(state, "remove")
	case "mount_add":
		mount, _ := expect.Get("mount")
		err := box.MountAdd(mount)
		if err != nil {
			return err
		}
		addAck(state, "mount_add")
	case "secret_set":
		key, _ := expect.Str("key")
		value, _ := expect.Str("value")
		err := box.SecretSet(key, value)
		if err != nil {
			return err
		}
		addAck(state, "set")
	case "secret_list":
		secrets, err := box.SecretList()
		if err != nil {
			return err
		}
		state.Secrets = secrets
	case "secret_remove":
		key, _ := expect.Str("key")
		err := box.SecretRemove(key)
		if err != nil {
			return err
		}
		addAck(state, "remove")
	case "snapshot_create":
		dest, _ := expect.Str("dest")
		resp, err := box.SnapshotCreate(dest)
		if err != nil {
			return err
		}
		state.SnapshotCreated = resp
	case "snapshot_list":
		snaps, err := client.Snapshots()
		if err != nil {
			return err
		}
		state.Snapshots = snaps
	case "snapshot_restore":
		snapshot, _ := expect.Str("snapshot")
		sname, _ := expect.Str("name")
		err := box.SnapshotRestore(snapshot, sname)
		if err != nil {
			return err
		}
		addAck(state, "restore")
	case "snapshot_fork":
		snapshot, _ := expect.Str("snapshot")
		sname, _ := expect.Str("name")
		err := box.SnapshotFork(snapshot, sname)
		if err != nil {
			return err
		}
		addAck(state, "fork")
	case "snapshot_remove":
		snapshot, _ := expect.Str("snapshot")
		err := box.SnapshotRemove(snapshot)
		if err != nil {
			return err
		}
		addAck(state, "remove")
	default:
		return errors.New("unhandled expect.type " + etype)
	}
	return nil
}

// FileLineReader assembles newline-delimited lines from a file (the harness
// stdout pipe).
type FileLineReader struct {
	F   *os.File
	Buf []byte
	Eof bool
}

func (r *FileLineReader) ReadLine() (string, error) {
	for {
		idx := indexByte(r.Buf, byte('\n'))
		if idx >= 0 {
			line := string(r.Buf[0:idx])
			r.Buf = r.Buf[idx+1:]
			return line, nil
		}
		if r.Eof {
			if len(r.Buf) == 0 {
				return "", nil
			}
			line := string(r.Buf)
			r.Buf = []byte{}
			return line, nil
		}
		var chunk [4096]byte
		n, err := r.F.Read(chunk[0:])
		if err != nil {
			return "", err
		}
		if n == 0 {
			r.Eof = true
		} else {
			for i := 0; i < n; i++ {
				r.Buf = append(r.Buf, chunk[i])
			}
		}
	}
}

func indexByte(b []byte, c byte) int {
	for i := 0; i < len(b); i++ {
		if b[i] == c {
			return i
		}
	}
	return -1
}

// runScenario runs one scenario; returns (passed, message).
func runScenario(binPath, scenarioFile string) (bool, string) {
	scenario, err := readScenario(scenarioFile)
	if err != nil {
		return false, "cannot read scenario: " + err.Error()
	}
	assertBlock, _ := scenario.Get("assert")

	r, w, perr := os.Pipe()
	if perr != nil {
		return false, "pipe: " + perr.Error()
	}
	re, we, eerr := os.Pipe()
	if eerr != nil {
		r.Close()
		w.Close()
		return false, "pipe: " + eerr.Error()
	}
	attr := &os.ProcAttr{Files: []*os.File{nil, w, re}}
	p, serr := os.StartProcess(binPath, []string{binPath, "serve", "--scenario", scenarioFile}, attr)
	w.Close()
	re.Close()
	if serr != nil {
		r.Close()
		we.Close()
		return false, "cannot start harness: " + serr.Error()
	}
	sock := ""
	defer func() {
		p.Kill()
		r.Close()
		we.Close()
		if sock != "" {
			rerr := os.Remove(sock)
			if rerr != nil {
				fmt.Fprintf(os.Stderr, "cleanup: cannot remove %s: %v\n", sock, rerr)
			}
		}
	}()

	// Read the READY line with a deadline.
	reader := &FileLineReader{F: r}
	r.SetReadDeadline(time.Now().Add(READY_TIMEOUT_MS * time.Millisecond))
	line, lerr := reader.ReadLine()
	if lerr != nil {
		return false, "harness did not print READY: " + lerr.Error()
	}
	if !strings.HasPrefix(line, "READY ") {
		return false, "harness did not print READY: " + line
	}
	sock = strings.TrimPrefix(line, "READY ")

	client, cerr := nanosandbox.Connect(sock)
	if cerr != nil {
		return false, "connect: " + cerr.Error()
	}
	state := &DriverState{}
	state.FirstLogs = nanosandbox.JsonNullValue()
	var writeConn *nanosandbox.Conn
	derr := error(nil)
	for _, step := range getArrOrEmpty(scenario, "steps") {
		expect, _ := step.Get("expect")
		_, hasTimeout := step.Int("timeout_ms")
		expectsTimeout, _ := assertBlock.Bool("expects_timeout")
		if expectsTimeout && hasTimeout {
			// Do NOT send the request; wait for the server's timeout error.
			conn := nanosandbox.NewConn(sock)
			oerr := conn.Open()
			if oerr != nil {
				derr = oerr
				break
			}
			conn.SetReadDeadline(time.Now().Add(RECV_TIMEOUT_MS * time.Millisecond))
			resp, rerr := conn.Recv()
			conn.Close()
			if rerr != nil {
				derr = rerr
				break
			}
			code, _ := resp.Str("code")
			state.TimeoutCode = code
			break
		}
		etype, _ := expect.Str("type")
		if etype == "fs_data" {
			if writeConn == nil {
				derr = errors.New("fs_data step without an open write stream")
				break
			}
			b64, _ := expect.Str("data_base64")
			eof, _ := expect.Bool("eof")
			body := nanosandbox.JsonObjectValue()
			body.Set("type", nanosandbox.JsonStringValue("fs_data"))
			body.Set("data_base64", nanosandbox.JsonStringValue(b64))
			body.Set("eof", nanosandbox.JsonBoolValue(eof))
			_, cerr := writeConn.Call(body)
			if cerr != nil {
				derr = cerr
				break
			}
			state.ChunksAcked++
			continue
		}
		if etype == "fs_write_stream" {
			writeConn = nanosandbox.NewConn(sock)
			oerr := writeConn.Open()
			if oerr != nil {
				derr = oerr
				break
			}
			req := nanosandbox.JsonObjectValue()
			req.Set("type", nanosandbox.JsonStringValue("fs_write_stream"))
			req.Set("name", nanosandbox.JsonStringValue(strOrEmpty(expect, "name")))
			req.Set("path", nanosandbox.JsonStringValue(strOrEmpty(expect, "path")))
			serr := writeConn.Send(req)
			if serr != nil {
				derr = serr
				break
			}
			ack, aerr := writeConn.Recv()
			if aerr != nil {
				derr = aerr
				break
			}
			atype, _ := ack.Str("type")
			if atype == "error" {
				code, _ := ack.Str("code")
				msg, _ := ack.Str("message")
				state.Errors = append(state.Errors, ErrorPair{Code: code, Message: msg})
			} else {
				addAck(state, "write_stream")
			}
			continue
		}
		derr = driveStep(client, state, expect)
		if derr != nil {
			recordError(state, derr)
			derr = error(nil)
		}
	}
	if writeConn != nil {
		writeConn.Close()
	}

	if derr != nil {
		recordError(state, derr)
	}

	if expectsUnsupported(assertBlock) {
		verb, _ := assertBlock.Str("unsupported_verb")
		if client.Supports(verb) {
			state.UnsupportedObserved = false
		} else {
			_, uerr := client.Sandbox("box").Exec("echo", []string{})
			ne, ok := errors.AsType[*nanosandbox.NanosbError](uerr)
			state.UnsupportedObserved = ok && ne.Code == "unsupported"
		}
	}

	failures := evaluateAssert(assertBlock, state)
	if len(failures) > 0 {
		return false, strings.Join(failures, "; ")
	}
	return true, "ok"
}

func expectsUnsupported(assertBlock nanosandbox.JsonValue) bool {
	v, _ := assertBlock.Bool("expects_unsupported")
	return v
}

// readScenario reads and parses a scenario JSON file.
func readScenario(path string) (nanosandbox.JsonValue, error) {
	f, err := os.Open(path)
	if err != nil {
		return nanosandbox.JsonNullValue(), err
	}
	var buf [65536]byte
	n, rerr := f.Read(buf[0:])
	f.Close()
	if rerr != nil {
		return nanosandbox.JsonNullValue(), rerr
	}
	v, perr := nanosandbox.JsonParse(string(buf[0:n]))
	if perr != nil {
		return nanosandbox.JsonNullValue(), perr
	}
	return v, nil
}

// getArrOrEmpty returns the array for key, or an empty array if absent.
func getArrOrEmpty(v nanosandbox.JsonValue, key string) []nanosandbox.JsonValue {
	arr, ok := v.GetArr(key)
	if !ok {
		return []nanosandbox.JsonValue{}
	}
	return arr
}

// strOrEmpty returns the string for key, or "" if absent.
func strOrEmpty(v nanosandbox.JsonValue, key string) string {
	s, _ := v.Str(key)
	return s
}

var ACK_KEYS = []string{
	"create_acked", "start_acked", "stop_acked", "kill_acked", "wait_acked",
	"restart_acked", "remove_acked",
	"drain_acked", "touch_acked", "pause_acked", "resume_acked", "modify_acked",
	"write_acked", "mkdir_acked", "remove_dir_acked", "copy_acked", "rename_acked",
	"copy_from_host_acked", "copy_to_host_acked", "mount_add_acked", "set_acked",
	"restore_acked", "fork_acked", "input_acked", "resize_acked", "write_stream_acked",
}

var ACK_NAMES = []string{
	"create", "start", "stop", "kill", "wait", "restart", "remove",
	"drain", "touch", "pause", "resume", "modify",
	"write", "mkdir", "remove_dir", "copy", "rename",
	"copy_from_host", "copy_to_host", "mount_add", "set",
	"restore", "fork", "input", "resize", "write_stream",
}

// evaluateAssert interprets the assert keys the driver understands (SPEC §7).
func evaluateAssert(assertBlock nanosandbox.JsonValue, state *DriverState) []string {
	var failures []string
	check := func(cond bool, msg string) {
		if !cond {
			failures = append(failures, msg)
		}
	}

	if assertBlock.Has("exit_code") {
		want, _ := assertBlock.Int("exit_code")
		check(state.ExitCode == int(want), fmt.Sprintf("exit_code: expected %d, got %d", want, state.ExitCode))
	}
	if assertBlock.Has("stdout_equals") {
		want, _ := assertBlock.Str("stdout_equals")
		check(state.Stdout == want, fmt.Sprintf("stdout_equals: expected %q, got %q", want, state.Stdout))
	}
	if assertBlock.Has("stderr_equals") {
		want, _ := assertBlock.Str("stderr_equals")
		check(state.Stderr == want, fmt.Sprintf("stderr_equals: expected %q, got %q", want, state.Stderr))
	}
	for _, key := range []string{"stdout_contains", "stderr_contains"} {
		if assertBlock.Has(key) {
			needles, _ := assertBlock.GetArr(key)
			for _, nv := range needles {
				needle := nv.AsString()
				haystack := state.Stdout
				if key == "stderr_contains" {
					haystack = state.Stderr
				}
				check(strings.Contains(haystack, needle), fmt.Sprintf("%s: %q not in %q", key, needle, haystack))
			}
		}
	}
	for i := 0; i < len(ACK_KEYS); i++ {
		if assertBlock.Has(ACK_KEYS[i]) {
			check(hasAck(state, ACK_NAMES[i]), fmt.Sprintf("%s: expected ack for %q", ACK_KEYS[i], ACK_NAMES[i]))
		}
	}
	if assertBlock.Has("wait_exit_code") {
		want, _ := assertBlock.Int("wait_exit_code")
		got := -1
		if state.WaitSet {
			got = state.WaitResult.ExitCode
		}
		check(got == int(want), fmt.Sprintf("wait_exit_code: expected %d, got %d", want, got))
	}
	if assertBlock.Has("wait_state") {
		want, _ := assertBlock.Str("wait_state")
		got := ""
		if state.WaitSet {
			got = state.WaitResult.State
		}
		check(got == want, fmt.Sprintf("wait_state: expected %q, got %q", want, got))
	}
	if assertBlock.Has("get_name") {
		want, _ := assertBlock.Str("get_name")
		got, _ := state.GetResult.Str("name")
		check(got == want, fmt.Sprintf("get_name: expected %q, got %q", want, got))
	}
	if assertBlock.Has("get_state") {
		want, _ := assertBlock.Str("get_state")
		got, _ := state.GetResult.Str("state")
		check(got == want, fmt.Sprintf("get_state: expected %q, got %q", want, got))
	}
	if assertBlock.Has("get_image") {
		want, _ := assertBlock.Str("get_image")
		got, _ := state.GetResult.Str("image")
		check(got == want, fmt.Sprintf("get_image: expected %q, got %q", want, got))
	}
	if assertBlock.Has("list_names") {
		want, _ := assertBlock.GetArr("list_names")
		var names []string
		for _, s := range state.ListResult {
			n, _ := s.Str("name")
			names = append(names, n)
		}
		check(strings.Join(names, ",") == joinStrings(want), fmt.Sprintf("list_names: expected %q, got %q", joinStrings(want), strings.Join(names, ",")))
	}
	if assertBlock.Has("list_states") {
		want, _ := assertBlock.GetArr("list_states")
		var states []string
		for _, s := range state.ListResult {
			n, _ := s.Str("state")
			states = append(states, n)
		}
		check(strings.Join(states, ",") == joinStrings(want), fmt.Sprintf("list_states: expected %q, got %q", joinStrings(want), strings.Join(states, ",")))
	}
	if assertBlock.Has("entries_contain") {
		want, _ := assertBlock.GetArr("entries_contain")
		var names []string
		for _, e := range state.FsEntries {
			n, _ := e.Str("name")
			names = append(names, n)
		}
		for _, nv := range want {
			needle := nv.AsString()
			check(containsString(names, needle), fmt.Sprintf("entries_contain: %q not in %q", needle, strings.Join(names, ",")))
		}
	}
	if assertBlock.Has("stat_is_dir") {
		want, _ := assertBlock.Bool("stat_is_dir")
		got, _ := state.FsStat.Bool("is_dir")
		check(got == want, fmt.Sprintf("stat_is_dir: expected %v, got %v", want, got))
	}
	if assertBlock.Has("exists") {
		want, _ := assertBlock.Bool("exists")
		check(state.FsExists == want, fmt.Sprintf("exists: expected %v, got %v", want, state.FsExists))
	}
	if assertBlock.Has("read_bytes_base64") {
		want, _ := assertBlock.Str("read_bytes_base64")
		got := base64.StdEncoding.EncodeToString(state.FsReadBytes)
		check(got == want, fmt.Sprintf("read_bytes_base64: expected %q, got %q", want, got))
	}
	if assertBlock.Has("content_equals") {
		want, _ := assertBlock.Str("content_equals")
		check(state.FsContent == want, fmt.Sprintf("content_equals: expected %q, got %q", want, state.FsContent))
	}
	if assertBlock.Has("chunks_acked") {
		want, _ := assertBlock.Int("chunks_acked")
		check(state.ChunksAcked == int(want), fmt.Sprintf("chunks_acked: expected %d, got %d", want, state.ChunksAcked))
	}
	for _, pair := range []string{"lines_contain", "resume_lines_contain"} {
		if assertBlock.Has(pair) {
			lines := state.FirstLogs
			if pair == "resume_lines_contain" {
				lines = state.ResumeLogs
			}
			lineArr, _ := lines.GetArr("lines")
			var data []string
			for _, l := range lineArr {
				d, _ := l.Str("data")
				data = append(data, d)
			}
			want, _ := assertBlock.GetArr(pair)
			for _, nv := range want {
				needle := nv.AsString()
				check(anyContains(data, needle), fmt.Sprintf("%s: %q not in %q", pair, needle, strings.Join(data, "|")))
			}
		}
	}
	if assertBlock.Has("follow_lines_contain") {
		var data []string
		for _, l := range state.FollowLines {
			d, _ := l.Str("data")
			data = append(data, d)
		}
		want, _ := assertBlock.GetArr("follow_lines_contain")
		for _, nv := range want {
			needle := nv.AsString()
			check(anyContains(data, needle), fmt.Sprintf("follow_lines_contain: %q not in %q", needle, strings.Join(data, "|")))
		}
	}
	if assertBlock.Has("next_cursor") {
		want, _ := assertBlock.Int("next_cursor")
		got, _ := state.FirstLogs.Int("next_cursor")
		check(got == want, fmt.Sprintf("next_cursor: expected %d, got %d", want, got))
	}
	if assertBlock.Has("all_source") {
		want, _ := assertBlock.Str("all_source")
		lineArr, _ := state.FirstLogs.GetArr("lines")
		allMatch := true
		for _, l := range lineArr {
			src, _ := l.Str("source")
			if src != want {
				allMatch = false
			}
		}
		check(allMatch, fmt.Sprintf("all_source: expected all %q", want))
	}
	if assertBlock.Has("cpu_percent") {
		want, _ := assertBlock.Num("cpu_percent")
		got, _ := state.Metrics.Num("cpu_percent")
		check(got == want, fmt.Sprintf("cpu_percent: expected %v, got %v", want, got))
	}
	if assertBlock.Has("memory_used_bytes") {
		want, _ := assertBlock.Int("memory_used_bytes")
		got, _ := state.Metrics.Int("memory_used_bytes")
		check(got == want, fmt.Sprintf("memory_used_bytes: expected %d, got %d", want, got))
	}
	if assertBlock.Has("has_fields") {
		fields, _ := assertBlock.GetArr("has_fields")
		for _, fv := range fields {
			f := fv.AsString()
			check(state.Metrics.Has(f), fmt.Sprintf("has_fields: %q missing", f))
		}
	}
	if assertBlock.Has("stream_samples") {
		want, _ := assertBlock.Int("stream_samples")
		check(len(state.MetricsSamples) == int(want), fmt.Sprintf("stream_samples: expected %d, got %d", want, len(state.MetricsSamples)))
	}
	if assertBlock.Has("first_cpu_percent") {
		want, _ := assertBlock.Num("first_cpu_percent")
		got := -1.0
		if len(state.MetricsSamples) > 0 {
			got, _ = state.MetricsSamples[0].Num("cpu_percent")
		}
		check(got == want, fmt.Sprintf("first_cpu_percent: expected %v, got %v", want, got))
	}
	if assertBlock.Has("volume_created") {
		want, _ := assertBlock.Str("volume_created")
		got, _ := state.VolumeCreated.Str("name")
		check(got == want, fmt.Sprintf("volume_created: expected %q, got %q", want, got))
	}
	if assertBlock.Has("volumes_contain") {
		want, _ := assertBlock.GetArr("volumes_contain")
		var names []string
		for _, v := range state.Volumes {
			n, _ := v.Str("name")
			names = append(names, n)
		}
		for _, nv := range want {
			needle := nv.AsString()
			check(containsString(names, needle), fmt.Sprintf("volumes_contain: %q not in %q", needle, strings.Join(names, ",")))
		}
	}
	if assertBlock.Has("volume_get_name") {
		want, _ := assertBlock.Str("volume_get_name")
		got, _ := state.VolumeGet.Str("name")
		check(got == want, fmt.Sprintf("volume_get_name: expected %q, got %q", want, got))
	}
	if assertBlock.Has("secrets_contain") {
		want, _ := assertBlock.GetArr("secrets_contain")
		var keys []string
		for _, s := range state.Secrets {
			k, _ := s.Str("key")
			keys = append(keys, k)
		}
		for _, nv := range want {
			needle := nv.AsString()
			check(containsString(keys, needle), fmt.Sprintf("secrets_contain: %q not in %q", needle, strings.Join(keys, ",")))
		}
	}
	if assertBlock.Has("secret_values_redacted") {
		redacted := true
		for _, s := range state.Secrets {
			val, ok := s.Str("value")
			if ok && val != "" {
				redacted = false
			}
		}
		check(redacted, "secret_values_redacted: values not redacted")
	}
	if assertBlock.Has("snapshot_created") {
		want, _ := assertBlock.Str("snapshot_created")
		got, _ := state.SnapshotCreated.Str("id")
		check(got == want, fmt.Sprintf("snapshot_created: expected %q, got %q", want, got))
	}
	if assertBlock.Has("snapshots_contain") {
		want, _ := assertBlock.GetArr("snapshots_contain")
		var ids []string
		for _, s := range state.Snapshots {
			id, _ := s.Str("id")
			ids = append(ids, id)
		}
		for _, nv := range want {
			needle := nv.AsString()
			check(containsString(ids, needle), fmt.Sprintf("snapshots_contain: %q not in %q", needle, strings.Join(ids, ",")))
		}
	}
	if assertBlock.Has("attach_events") {
		want, _ := assertBlock.GetArr("attach_events")
		var got []string
		for _, ev := range state.AttachEvents {
			e, _ := ev.Str("event")
			got = append(got, e)
		}
		check(strings.Join(got, ",") == joinStrings(want), fmt.Sprintf("attach_events: expected %q, got %q", joinStrings(want), strings.Join(got, ",")))
	}
	if assertBlock.Has("attach_exit_code") {
		want, _ := assertBlock.Int("attach_exit_code")
		check(state.AttachExitCode == int(want), fmt.Sprintf("attach_exit_code: expected %d, got %d", want, state.AttachExitCode))
	}
	if assertBlock.Has("error_code") {
		want, _ := assertBlock.Str("error_code")
		got := ""
		if len(state.Errors) > 0 {
			got = state.Errors[0].Code
		}
		check(got == want, fmt.Sprintf("error_code: expected %q, got %q", want, got))
	}
	if assertBlock.Has("second_error_code") {
		want, _ := assertBlock.Str("second_error_code")
		got := ""
		if len(state.Errors) > 1 {
			got = state.Errors[1].Code
		}
		check(got == want, fmt.Sprintf("second_error_code: expected %q, got %q", want, got))
	}
	if assertBlock.Has("error_message_contains") {
		want, _ := assertBlock.Str("error_message_contains")
		msg := ""
		if len(state.Errors) > 0 {
			msg = state.Errors[0].Message
		}
		check(strings.Contains(msg, want), fmt.Sprintf("error_message_contains: %q not in %q", want, msg))
	}
	if assertBlock.Has("expects_timeout") {
		want, _ := assertBlock.Str("timeout_code")
		if want == "" {
			want = "timeout"
		}
		check(state.TimeoutCode == want, fmt.Sprintf("timeout_code: expected %q, got %q", want, state.TimeoutCode))
	}
	if assertBlock.Has("expects_unsupported") {
		check(state.UnsupportedObserved, "expected an unsupported error for the unadvertised verb")
	}
	if assertBlock.Has("ping_ok") {
		check(state.PingOk, "ping_ok: ping did not succeed")
	}
	if assertBlock.Has("exec_exit_code") {
		want, _ := assertBlock.Int("exec_exit_code")
		check(state.ExitCode == int(want), fmt.Sprintf("exec_exit_code: expected %d, got %d", want, state.ExitCode))
	}
	if assertBlock.Has("exec_stdout") {
		want, _ := assertBlock.Str("exec_stdout")
		check(state.Stdout == want, fmt.Sprintf("exec_stdout: expected %q, got %q", want, state.Stdout))
	}
	return failures
}

func joinStrings(vals []nanosandbox.JsonValue) string {
	var out []string
	for _, v := range vals {
		out = append(out, v.AsString())
	}
	return strings.Join(out, ",")
}

func containsString(haystack []string, needle string) bool {
	for _, s := range haystack {
		if s == needle {
			return true
		}
	}
	return false
}

func anyContains(haystack []string, needle string) bool {
	for _, s := range haystack {
		if strings.Contains(s, needle) {
			return true
		}
	}
	return false
}

// findBinary locates the nanosb-conformance harness binary.
func findBinary() string {
	env := os.Getenv("NANOSB_CONFORMANCE_BIN")
	if env != "" {
		return env
	}
	return "target/debug/nanosb-conformance"
}

func main() {
	binPath := findBinary()
	scenariosDir := "sdk/conformance/scenarios"
	entries, derr := os.ReadDir(scenariosDir)
	if derr != nil {
		fmt.Fprintf(os.Stderr, "error: cannot read scenarios dir %s: %v\n", scenariosDir, derr)
		os.Exit(1)
	}
	var files []string
	for _, e := range entries {
		name := e.Name()
		if strings.HasSuffix(name, ".json") {
			files = append(files, scenariosDir + "/" + name)
		}
	}
	if len(files) == 0 {
		fmt.Fprintf(os.Stderr, "error: no scenarios found in %s\n", scenariosDir)
		os.Exit(1)
	}
	sortStrings(files)

	passed := 0
	failed := 0
	for _, scenarioFile := range files {
		scenario, serr := readScenario(scenarioFile)
		if serr != nil {
			failed++
			fmt.Println("FAIL " + scenarioFile + ": " + serr.Error())
			continue
		}
		sid, _ := scenario.Str("id")
		ok, message := runScenario(binPath, scenarioFile)
		if ok {
			passed++
			fmt.Println("PASS " + sid)
		} else {
			failed++
			fmt.Println("FAIL " + sid + ": " + message)
		}
	}

	fmt.Println("")
	fmt.Println(fmt.Sprintf("%d passed, %d failed", passed, failed))
	if failed > 0 {
		os.Exit(1)
	}
}

func sortStrings(arr []string) {
	for i := 0; i < len(arr); i++ {
		for j := i + 1; j < len(arr); j++ {
			if arr[j] < arr[i] {
				tmp := arr[i]
				arr[i] = arr[j]
				arr[j] = tmp
			}
		}
	}
}