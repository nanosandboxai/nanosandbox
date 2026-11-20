// sandbox.go — a handle bound to one sandbox: lifecycle, execution, and
// observability verbs. Filesystem / storage / secrets / snapshots verbs live
// in `sandbox_fs.go` to keep this file focused.
package nanosandbox

// Sandbox is a handle bound to one sandbox.
type Sandbox struct {
	Client *SandboxClient
	Name   string
}

// Name returns the sandbox name.
func (b *Sandbox) GetName() string {
	return b.Name
}

// State returns the sandbox lifecycle state.
func (b *Sandbox) State() (string, error) {
	req := JsonObjectValue()
	req.Set("type", JsonStringValue("status"))
	req.Set("name", JsonStringValue(b.Name))
	resp, err := b.Client.Call(req)
	if err != nil {
		return "", err
	}
	st, _ := resp.Str("state")
	return st, nil
}

// Info fetches the sandbox's full info.
func (b *Sandbox) Info() (JsonValue, error) {
	return b.Client.Get(b.Name)
}

// Start starts the sandbox.
func (b *Sandbox) Start() error {
	req := JsonObjectValue()
	req.Set("type", JsonStringValue("start"))
	req.Set("name", JsonStringValue(b.Name))
	_, err := b.Client.Call(req)
	return err
}

// Stop stops the sandbox.
func (b *Sandbox) Stop(force bool) error {
	req := JsonObjectValue()
	req.Set("type", JsonStringValue("stop"))
	req.Set("name", JsonStringValue(b.Name))
	req.Set("force", JsonBoolValue(force))
	_, err := b.Client.Call(req)
	return err
}

// Kill kills the sandbox.
func (b *Sandbox) Kill() error {
	req := JsonObjectValue()
	req.Set("type", JsonStringValue("kill"))
	req.Set("name", JsonStringValue(b.Name))
	_, err := b.Client.Call(req)
	return err
}

// Remove removes the sandbox.
func (b *Sandbox) Remove(force bool) error {
	req := JsonObjectValue()
	req.Set("type", JsonStringValue("remove"))
	req.Set("name", JsonStringValue(b.Name))
	req.Set("force", JsonBoolValue(force))
	_, err := b.Client.Call(req)
	return err
}

// Pause pauses the sandbox.
func (b *Sandbox) Pause() error {
	req := JsonObjectValue()
	req.Set("type", JsonStringValue("pause"))
	req.Set("name", JsonStringValue(b.Name))
	_, err := b.Client.Call(req)
	return err
}

// Resume resumes the sandbox.
func (b *Sandbox) Resume() error {
	req := JsonObjectValue()
	req.Set("type", JsonStringValue("resume"))
	req.Set("name", JsonStringValue(b.Name))
	_, err := b.Client.Call(req)
	return err
}

// Touch refreshes the sandbox's idle timer.
func (b *Sandbox) Touch() error {
	req := JsonObjectValue()
	req.Set("type", JsonStringValue("touch"))
	req.Set("name", JsonStringValue(b.Name))
	_, err := b.Client.Call(req)
	return err
}

// Drain drains the sandbox.
func (b *Sandbox) Drain() error {
	req := JsonObjectValue()
	req.Set("type", JsonStringValue("drain"))
	req.Set("name", JsonStringValue(b.Name))
	_, err := b.Client.Call(req)
	return err
}

// Modify applies a modification spec to the live sandbox.
func (b *Sandbox) Modify(spec JsonValue) error {
	req := JsonObjectValue()
	req.Set("type", JsonStringValue("modify"))
	req.Set("name", JsonStringValue(b.Name))
	req.Set("spec", spec)
	_, err := b.Client.Call(req)
	return err
}

// WaitResult is the outcome of a blocking wait: the exit code, the wall-clock
// time the sandbox exited (ms since epoch), and the terminal lifecycle state.
type WaitResult struct {
	ExitCode   int
	ExitedAtMs uint64
	State      string
}

// Wait blocks until the sandbox reaches a terminal state, returning the exit
// code, exit timestamp, and state. A nil timeoutSecs waits indefinitely; a
// non-nil value bounds the wait. An error response is surfaced as an error.
func (b *Sandbox) Wait(timeoutSecs *float64) (WaitResult, error) {
	req := JsonObjectValue()
	req.Set("type", JsonStringValue("wait"))
	req.Set("name", JsonStringValue(b.Name))
	if timeoutSecs != nil {
		req.Set("timeout_secs", JsonNumberValue(*timeoutSecs))
	}
	resp, err := b.Client.Call(req)
	if err != nil {
		return WaitResult{}, err
	}
	code, _ := resp.Int("exit_code")
	at, _ := resp.Int("exited_at_ms")
	state, _ := resp.Str("state")
	return WaitResult{ExitCode: int(code), ExitedAtMs: uint64(at), State: state}, nil
}

// KillWith kills the sandbox with the given signal.
func (b *Sandbox) KillWith(signal int) error {
	req := JsonObjectValue()
	req.Set("type", JsonStringValue("kill"))
	req.Set("name", JsonStringValue(b.Name))
	req.Set("signal", JsonIntValue(int64(signal)))
	_, err := b.Client.Call(req)
	return err
}

// Restart re-boots the sandbox.
func (b *Sandbox) Restart() error {
	req := JsonObjectValue()
	req.Set("type", JsonStringValue("restart"))
	req.Set("name", JsonStringValue(b.Name))
	_, err := b.Client.Call(req)
	return err
}

// WaitUntilStopped polls the sandbox state until it is stopped.
func (b *Sandbox) WaitUntilStopped(timeoutSecs float64) (string, error) {
	res, err := b.Wait(&timeoutSecs)
	if err != nil {
		return "", err
	}
	return res.State, nil
}

// Exec runs a buffered command and returns the exec result.
func (b *Sandbox) Exec(command string, args []string) (JsonValue, error) {
	req := JsonObjectValue()
	req.Set("type", JsonStringValue("exec"))
	req.Set("name", JsonStringValue(b.Name))
	req.Set("command", JsonStringValue(command))
	argsArr := JsonArrayValue()
	for _, a := range args {
		argsArr.Append(JsonStringValue(a))
	}
	req.Set("args", argsArr)
	return b.Client.Call(req)
}

// ExecWith runs a buffered command from a request object.
func (b *Sandbox) ExecWith(request JsonValue) (JsonValue, error) {
	req := request
	req.Set("type", JsonStringValue("exec"))
	req.Set("name", JsonStringValue(b.Name))
	if !req.Has("args") {
		req.Set("args", JsonArrayValue())
	}
	return b.Client.Call(req)
}

// Shell runs a shell script and returns the buffered result.
func (b *Sandbox) Shell(script string) (JsonValue, error) {
	req := JsonObjectValue()
	req.Set("type", JsonStringValue("shell"))
	req.Set("name", JsonStringValue(b.Name))
	req.Set("script", JsonStringValue(script))
	return b.Client.Call(req)
}

// ExecStream streams a command's output to onEvent until the exit event,
// returning the exit code.
func (b *Sandbox) ExecStream(request JsonValue, onEvent func(JsonValue)) (int, error) {
	req := request
	req.Set("type", JsonStringValue("exec_stream"))
	req.Set("name", JsonStringValue(b.Name))
	if !req.Has("args") {
		req.Set("args", JsonArrayValue())
	}
	conn := NewConn(b.Client.Socket)
	err := conn.Open()
	if err != nil {
		return -1, err
	}
	code := -1
	serr := conn.Send(req)
	if serr == nil {
		ack, aerr := conn.Recv()
		if aerr != nil {
			err = aerr
		} else if ackType(ack) == "error" {
			err = errorFrom(ack)
		} else {
			for {
				resp, rerr := conn.Recv()
				if rerr != nil {
					err = rerr
					break
				}
				if ackType(resp) != "guest" {
					continue
				}
				ev, _ := resp.Str("event")
				if ev == "exit" {
					codeVal, _ := resp.Int("code")
					code = int(codeVal)
					onEvent(resp)
					break
				}
				if ev == "error" {
					onEvent(resp)
					msg, _ := resp.Str("message")
					err = &NanosbError{Code: "internal", Message: msg}
					break
				}
				onEvent(resp)
			}
		}
	} else {
		err = serr
	}
	conn.Close()
	return code, err
}

// ShellStream streams a shell script's output to onEvent.
func (b *Sandbox) ShellStream(script string, onEvent func(JsonValue)) (int, error) {
	req := JsonObjectValue()
	req.Set("command", JsonStringValue(script))
	req.Set("shell", JsonBoolValue(true))
	return b.ExecStream(req, onEvent)
}

// Attach attaches to the sandbox console, delivering guest events to onEvent
// until the exit/error event.
func (b *Sandbox) Attach(onEvent func(JsonValue)) error {
	conn := NewConn(b.Client.Socket)
	err := conn.Open()
	if err != nil {
		return err
	}
	req := JsonObjectValue()
	req.Set("type", JsonStringValue("attach"))
	req.Set("name", JsonStringValue(b.Name))
	serr := conn.Send(req)
	if serr == nil {
		ack, aerr := conn.Recv()
		if aerr != nil {
			err = aerr
		} else if ackType(ack) == "error" {
			err = errorFrom(ack)
		} else {
			for {
				resp, rerr := conn.Recv()
				if rerr != nil {
					err = rerr
					break
				}
				if ackType(resp) != "guest" {
					continue
				}
				onEvent(resp)
				ev, _ := resp.Str("event")
				if ev == "exit" || ev == "error" {
					break
				}
			}
		}
	} else {
		err = serr
	}
	conn.Close()
	return err
}

// AttachSession opens a live attach session for driving console input/resize
// and reading guest events on the same connection.
func (b *Sandbox) AttachSession() (*AttachSession, error) {
	conn := NewConn(b.Client.Socket)
	err := conn.Open()
	if err != nil {
		return nil, err
	}
	req := JsonObjectValue()
	req.Set("type", JsonStringValue("attach"))
	req.Set("name", JsonStringValue(b.Name))
	serr := conn.Send(req)
	if serr != nil {
		conn.Close()
		return nil, serr
	}
	ack, aerr := conn.Recv()
	if aerr != nil {
		conn.Close()
		return nil, aerr
	}
	if ackType(ack) == "error" {
		conn.Close()
		return nil, errorFrom(ack)
	}
	return &AttachSession{Conn: conn}, nil
}

// Metrics returns a resource usage snapshot.
func (b *Sandbox) Metrics() (JsonValue, error) {
	req := JsonObjectValue()
	req.Set("type", JsonStringValue("metrics"))
	req.Set("name", JsonStringValue(b.Name))
	return b.Client.Call(req)
}

// MetricsStream streams metrics samples to onEvent until the done event.
func (b *Sandbox) MetricsStream(intervalMs int, onEvent func(JsonValue)) error {
	req := JsonObjectValue()
	req.Set("type", JsonStringValue("metrics_stream"))
	req.Set("name", JsonStringValue(b.Name))
	if intervalMs > 0 {
		req.Set("interval_ms", JsonIntValue(int64(intervalMs)))
	}
	return b.streamEvents(req, "metrics", onEvent)
}

// Logs returns buffered log lines.
func (b *Sandbox) Logs(tail int) ([]JsonValue, error) {
	return b.LogsWith(tail, -1, "")
}

// LogsWith returns buffered log lines with tail/cursor/source filters.
func (b *Sandbox) LogsWith(tail, cursor int, source string) ([]JsonValue, error) {
	req := JsonObjectValue()
	req.Set("type", JsonStringValue("logs"))
	req.Set("name", JsonStringValue(b.Name))
	req.Set("follow", JsonBoolValue(false))
	if tail > 0 {
		req.Set("tail", JsonIntValue(int64(tail)))
	}
	if cursor >= 0 {
		req.Set("cursor", JsonIntValue(int64(cursor)))
	}
	if source != "" {
		req.Set("source", JsonStringValue(source))
	}
	resp, err := b.Client.Call(req)
	if err != nil {
		return nil, err
	}
	lines, _ := resp.GetArr("lines")
	return lines, nil
}

// LogsFollow streams log lines to onLine until the done event.
func (b *Sandbox) LogsFollow(onLine func(JsonValue), tail int) error {
	return b.LogsFollowWith(onLine, tail, -1, "")
}

// LogsFollowWith streams log lines with tail/cursor/source filters.
func (b *Sandbox) LogsFollowWith(onLine func(JsonValue), tail, cursor int, source string) error {
	req := JsonObjectValue()
	req.Set("type", JsonStringValue("logs"))
	req.Set("name", JsonStringValue(b.Name))
	req.Set("follow", JsonBoolValue(true))
	if tail > 0 {
		req.Set("tail", JsonIntValue(int64(tail)))
	}
	if cursor >= 0 {
		req.Set("cursor", JsonIntValue(int64(cursor)))
	}
	if source != "" {
		req.Set("source", JsonStringValue(source))
	}
	return b.streamEvents(req, "log", onLine)
}

// streamEvents drives a generic event stream: opening ack, then event frames
// until the done frame, delivering matching events to onEvent.
func (b *Sandbox) streamEvents(req JsonValue, eventName string, onEvent func(JsonValue)) error {
	conn := NewConn(b.Client.Socket)
	err := conn.Open()
	if err != nil {
		return err
	}
	serr := conn.Send(req)
	if serr == nil {
		ack, aerr := conn.Recv()
		if aerr != nil {
			err = aerr
		} else if ackType(ack) == "error" {
			err = errorFrom(ack)
		} else {
			for {
				resp, rerr := conn.Recv()
				if rerr != nil {
					err = rerr
					break
				}
				if ackType(resp) != "event" {
					continue
				}
				ev, _ := resp.Str("event")
				if ev == "done" {
					break
				}
				if ev == eventName {
					onEvent(resp)
				}
			}
		}
	} else {
		err = serr
	}
	conn.Close()
	return err
}

// AttachSession is a live attach session: drive console input/resize and read
// guest events on the same connection. ReadEvents returns a null value once
// the stream ends.
type AttachSession struct {
	Conn *Conn
}

// SendInput writes bytes to the guest console.
func (s *AttachSession) SendInput(data []byte) error {
	req := JsonObjectValue()
	req.Set("type", JsonStringValue("attach_input"))
	req.Set("data", JsonStringValue(string(data)))
	_, err := s.Conn.Call(req)
	return err
}

// Resize resizes the guest console.
func (s *AttachSession) Resize(cols, rows int) error {
	req := JsonObjectValue()
	req.Set("type", JsonStringValue("attach_resize"))
	req.Set("cols", JsonIntValue(int64(cols)))
	req.Set("rows", JsonIntValue(int64(rows)))
	_, err := s.Conn.Call(req)
	return err
}

// ReadEvents returns the next guest event, or a null value at stream end.
func (s *AttachSession) ReadEvents() (JsonValue, error) {
	for {
		resp, err := s.Conn.Recv()
		if err != nil {
			return JsonNullValue(), err
		}
		if ackType(resp) != "guest" {
			continue
		}
		ev, _ := resp.Str("event")
		if ev == "exit" || ev == "error" {
			return JsonNullValue(), nil
		}
		return resp, nil
	}
}

// Close closes the attach session connection.
func (s *AttachSession) Close() {
	s.Conn.Close()
}

func ackType(v JsonValue) string {
	t, _ := v.Str("type")
	return t
}

func errorFrom(v JsonValue) error {
	code, _ := v.Str("code")
	msg, _ := v.Str("message")
	if code == "" {
		code = "internal"
	}
	return &NanosbError{Code: code, Message: msg}
}
