// agent-gateway is a lightweight HTTP server that runs as PID 1 inside a
// libkrun VM. It receives messages from the host via HTTP, spawns the
// appropriate agent CLI, and streams output back as Server-Sent Events (SSE).
//
// This enables persistent VMs with multi-turn conversations: the VM boots
// once, and each chat message is an HTTP request rather than a new VM.
package main

import (
	"bufio"
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"log"
	"net"
	"net/http"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/nanosandboxai/agent-gateway/mcp"
	"github.com/nanosandboxai/agent-gateway/skills"
)

// No embedded mcp-servers.yaml — all MCP servers are user-defined at runtime.

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

const (
	listenAddr     = ":8080"
	defaultTimeout = 300 * time.Second
	defaultWorkDir = "/workspace"
)

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

// MessageRequest is the JSON body for POST /api/v1/message.
type MessageRequest struct {
	Message string            `json:"message"`
	Agent   string            `json:"agent"`
	Model   string            `json:"model"`
	Env     map[string]string `json:"env"`
	Timeout int               `json:"timeout"` // seconds, 0 = default
}

// ExecRequest is the JSON body for POST /api/v1/exec.
type ExecRequest struct {
	Command string            `json:"command"`
	Args    []string          `json:"args"`
	Env     map[string]string `json:"env"`
	Timeout int               `json:"timeout"`
}

// SSEEvent is a single SSE data payload.
type SSEEvent struct {
	Type string `json:"type"` // "stdout", "stderr", "exit", "error"
	Data string `json:"data,omitempty"`
	Code *int   `json:"code,omitempty"`
}

// agentSession tracks per-agent state for conversation continuity.
type agentSession struct {
	messageCount    int
	sessionID       string    // captured from agent output
	wasGenerating   bool      // was agent producing output when killed?
	interruptReason string    // "skills_update", "mcp_update", "agent_change"
	lastOutputTime  time.Time // when last SSE stdout event was sent
	pid             int       // current agent process PID
}

// ---------------------------------------------------------------------------
// Global state
// ---------------------------------------------------------------------------

var (
	startTime = time.Now()
	busy      sync.Mutex // only one agent command at a time
	sessions  = struct {
		sync.Mutex
		m map[string]*agentSession
	}{m: make(map[string]*agentSession)}
)

func getSession(agent string) *agentSession {
	sessions.Lock()
	defer sessions.Unlock()
	s, ok := sessions.m[agent]
	if !ok {
		s = &agentSession{}
		sessions.m[agent] = s
	}
	return s
}

// ---------------------------------------------------------------------------
// Network initialisation (replaces nanosb-net-init shell script)
// ---------------------------------------------------------------------------

func initNetwork() {
	// Configure eth0 with the static IP that gvproxy expects.
	// Try `ip` first (iproute2), fall back to `ifconfig`/`route`.
	if path, err := exec.LookPath("ip"); err == nil {
		run(path, "link", "set", "eth0", "up")
		run(path, "addr", "add", "192.168.127.2/24", "dev", "eth0")
		run(path, "route", "add", "default", "via", "192.168.127.1", "dev", "eth0")
	} else if path, err := exec.LookPath("ifconfig"); err == nil {
		run(path, "eth0", "192.168.127.2", "netmask", "255.255.255.0", "up")
		if routePath, err := exec.LookPath("route"); err == nil {
			run(routePath, "add", "default", "gw", "192.168.127.1")
		}
	} else {
		log.Println("[agent-gateway] WARNING: no networking tools found (ip/ifconfig)")
	}

	// Configure DNS — gvproxy's built-in DNS is at the gateway IP.
	_ = os.MkdirAll("/etc", 0755)
	_ = os.WriteFile("/etc/resolv.conf", []byte("nameserver 192.168.127.1\n"), 0644)
}

func run(name string, args ...string) {
	cmd := exec.Command(name, args...)
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	if err := cmd.Run(); err != nil {
		log.Printf("[agent-gateway] %s %v: %v", name, args, err)
	}
}

// ---------------------------------------------------------------------------
// Agent command builder
// ---------------------------------------------------------------------------

func buildAgentCommand(req *MessageRequest, sess *agentSession) (string, []string) {
	message := req.Message

	// Auto-inject continuation message after interrupted restart
	if sess.wasGenerating && sess.interruptReason != "" {
		continuation := fmt.Sprintf(
			"Your previous operation was interrupted to apply configuration changes (%s). "+
				"Check the current state of any files you were modifying for incomplete content, "+
				"then continue where you left off. Do not repeat already-completed work.",
			sess.interruptReason)
		if message == "" {
			message = continuation
		} else {
			message = continuation + "\n\n" + message
		}
		sess.wasGenerating = false
		sess.interruptReason = ""
	}

	switch req.Agent {
	case "claude", "claude-code":
		args := []string{
			"--print", message,
			"--verbose",
			"--output-format", "stream-json",
			"--include-partial-messages",
		}
		switch permissions {
		case "allow_all":
			args = append(args, "--dangerously-skip-permissions")
		case "accept_edits":
			args = append(args, "--permission-mode", "acceptEdits")
		}
		if req.Model != "" {
			args = append(args, "--model", req.Model)
		}
		// Use --resume with explicit session ID when available (e.g., after restart).
		// Fall back to --continue for normal multi-turn continuation.
		if sess.sessionID != "" {
			args = append(args, "--resume", sess.sessionID)
		} else if sess.messageCount > 0 {
			args = append(args, "--continue")
		}
		return "claude", args

	case "goose":
		if req.Model != "" {
			if req.Env == nil {
				req.Env = make(map[string]string)
			}
			req.Env["GOOSE_DEFAULT_MODEL"] = req.Model
		}
		if sess.sessionID != "" {
			return "goose", []string{"session", "resume", sess.sessionID, "--message", message}
		}
		return "goose", []string{"run", "--text", message}

	case "codex":
		if sess.sessionID != "" {
			return "codex", []string{"resume", sess.sessionID, message}
		}
		args := []string{"exec"}
		switch permissions {
		case "allow_all":
			args = append(args, "--full-auto")
		case "accept_edits":
			args = append(args, "--full-auto")
		}
		args = append(args, "--json")
		if req.Model != "" {
			args = append(args, "--model", req.Model)
		}
		args = append(args, message)
		return "codex", args

	case "cursor", "cursor-agent":
		args := []string{"--message", message}
		switch permissions {
		case "allow_all":
			args = append(args, "--force")
			// --trust and --approve-mcps require --print (headless mode)
			if autoMode {
				args = append(args, "--trust", "--approve-mcps")
			}
		case "accept_edits":
			// --trust requires --print (headless mode)
			if autoMode {
				args = append(args, "--trust")
			}
		}
		if req.Model != "" {
			args = append(args, "--model", req.Model)
		}
		if sess.sessionID != "" {
			args = append(args, "--resume", sess.sessionID)
		}
		return "cursor-agent", args

	default:
		// Custom/unknown agent: treat agent name as the binary
		return req.Agent, []string{message}
	}
}

// normalizeAgent returns a canonical agent name for session lookup.
func normalizeAgent(agent string) string {
	switch agent {
	case "claude", "claude-code":
		return "claude"
	case "cursor", "cursor-agent":
		return "cursor"
	default:
		return agent
	}
}

// extractSessionID parses agent output to capture the session identifier.
// Returns empty string if no session ID found in this line.
func extractSessionID(agent, line string) string {
	switch agent {
	case "claude":
		// Claude stream-json emits: {"type":"system","session_id":"..."}
		var msg struct {
			Type      string `json:"type"`
			SessionID string `json:"session_id"`
		}
		if json.Unmarshal([]byte(line), &msg) == nil && msg.SessionID != "" {
			return msg.SessionID
		}
	case "goose":
		// Goose outputs session ID in various formats.
		// Try JSON first, then text patterns.
		var msg struct {
			SessionID string `json:"session_id"`
		}
		if json.Unmarshal([]byte(line), &msg) == nil && msg.SessionID != "" {
			return msg.SessionID
		}
		// Text pattern: "session: <id>" or "Session: <id>"
		if idx := strings.Index(strings.ToLower(line), "session:"); idx >= 0 {
			rest := strings.TrimSpace(line[idx+len("session:"):])
			if parts := strings.Fields(rest); len(parts) > 0 {
				return parts[0]
			}
		}
	case "codex":
		// Codex exec --json emits: {"session_id":"sess_..."}
		var msg struct {
			SessionID string `json:"session_id"`
		}
		if json.Unmarshal([]byte(line), &msg) == nil && msg.SessionID != "" {
			return msg.SessionID
		}
	case "cursor":
		// Cursor emits: {"chatId":"chat_..."}
		var msg struct {
			ChatID string `json:"chatId"`
		}
		if json.Unmarshal([]byte(line), &msg) == nil && msg.ChatID != "" {
			return msg.ChatID
		}
	}
	return ""
}

// ---------------------------------------------------------------------------
// SSE streaming helpers
// ---------------------------------------------------------------------------

func sseWrite(w http.ResponseWriter, evt SSEEvent) {
	data, _ := json.Marshal(evt)
	fmt.Fprintf(w, "data: %s\n\n", data)
	if f, ok := w.(http.Flusher); ok {
		f.Flush()
	}
}

// streamCommand spawns a command, streams stdout/stderr as SSE, and returns
// the exit code. It blocks until the command finishes or the context is done.
// When agentName and sess are provided, it tracks PID and extracts session IDs.
func streamCommand(ctx context.Context, w http.ResponseWriter, bin string, args []string, env map[string]string, agentName string, sess *agentSession) int {
	cmd := exec.CommandContext(ctx, bin, args...)
	cmd.Dir = defaultWorkDir

	// Run agent commands as the 'developer' user (UID 1000, GID 1000).
	// The agent-gateway runs as root (started by init), but agents like
	// Claude Code refuse --dangerously-skip-permissions when running as root.
	cmd.SysProcAttr = sysProcAttrForDeveloper()

	// Build environment: inherit base env, overlay request-specific vars.
	cmdEnv := os.Environ()
	for k, v := range env {
		cmdEnv = append(cmdEnv, fmt.Sprintf("%s=%s", k, v))
	}
	// Ensure basic vars are set. HOME/USER must be force-overwritten: the
	// gateway runs as root (inherits HOME=/root from PID 1) but subprocesses
	// run as developer (UID 1000). glibc's getenv returns the FIRST matching
	// entry in environ, so appending alone is not enough — we must strip
	// any pre-existing HOME=/USER= entries before setting the developer values.
	cmdEnv = setEnv(cmdEnv, "HOME", "/home/developer")
	cmdEnv = setEnv(cmdEnv, "USER", "developer")
	cmdEnv = ensureEnv(cmdEnv, "PATH", "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin")
	cmdEnv = ensureEnv(cmdEnv, "TERM", "dumb")
	// Goose permissions are set via environment variable.
	if agentName == "goose" {
		switch permissions {
		case "allow_all":
			cmdEnv = append(cmdEnv, "GOOSE_MODE=auto")
		case "accept_edits":
			cmdEnv = append(cmdEnv, "GOOSE_MODE=smart_approve")
		}
	}
	cmd.Env = cmdEnv

	stdout, err := cmd.StdoutPipe()
	if err != nil {
		sseWrite(w, SSEEvent{Type: "error", Data: fmt.Sprintf("stdout pipe: %v", err)})
		return -1
	}
	stderr, err := cmd.StderrPipe()
	if err != nil {
		sseWrite(w, SSEEvent{Type: "error", Data: fmt.Sprintf("stderr pipe: %v", err)})
		return -1
	}

	if err := cmd.Start(); err != nil {
		sseWrite(w, SSEEvent{Type: "error", Data: fmt.Sprintf("start: %v", err)})
		return -1
	}

	// Track PID for restart support.
	if sess != nil {
		sess.pid = cmd.Process.Pid
	}

	// Stream stdout and stderr concurrently.
	var wg sync.WaitGroup
	wg.Add(2)

	// Stdout: log every line (truncated) so we can see whether the subprocess
	// is producing output at all during long-running agent runs.
	stdoutLines := 0
	go func() {
		defer wg.Done()
		scanner := bufio.NewScanner(stdout)
		scanner.Buffer(make([]byte, 256*1024), 1024*1024) // 1MB line buffer
		for scanner.Scan() {
			line := scanner.Text()
			stdoutLines++
			preview := line
			if len(preview) > 200 {
				preview = preview[:200] + "...(truncated)"
			}
			log.Printf("[agent-gateway] stdout#%d: %s", stdoutLines, preview)
			sseWrite(w, SSEEvent{Type: "stdout", Data: line})

			if sess != nil {
				sess.lastOutputTime = time.Now()

				// Extract session ID from agent output (first occurrence wins).
				if sess.sessionID == "" && agentName != "" {
					if id := extractSessionID(agentName, line); id != "" {
						sess.sessionID = id
						log.Printf("[agent-gateway] captured session ID for %s: %s", agentName, id)
					}
				}
			}
		}
		if err := scanner.Err(); err != nil {
			log.Printf("[agent-gateway] stdout scanner error after %d lines: %v", stdoutLines, err)
		} else {
			log.Printf("[agent-gateway] stdout EOF after %d lines", stdoutLines)
		}
	}()

	stderrLines := 0
	go func() {
		defer wg.Done()
		scanner := bufio.NewScanner(stderr)
		scanner.Buffer(make([]byte, 256*1024), 1024*1024)
		for scanner.Scan() {
			line := scanner.Text()
			stderrLines++
			preview := line
			if len(preview) > 200 {
				preview = preview[:200] + "...(truncated)"
			}
			log.Printf("[agent-gateway] stderr#%d: %s", stderrLines, preview)
			sseWrite(w, SSEEvent{Type: "stderr", Data: line})
		}
		if err := scanner.Err(); err != nil {
			log.Printf("[agent-gateway] stderr scanner error after %d lines: %v", stderrLines, err)
		} else {
			log.Printf("[agent-gateway] stderr EOF after %d lines", stderrLines)
		}
	}()

	wg.Wait()

	// Clear PID after process exits.
	if sess != nil {
		sess.pid = 0
	}

	exitCode := 0
	if err := cmd.Wait(); err != nil {
		if exitErr, ok := err.(*exec.ExitError); ok {
			exitCode = exitErr.ExitCode()
		} else {
			exitCode = -1
		}
	}
	return exitCode
}

func ensureEnv(env []string, key, fallback string) []string {
	prefix := key + "="
	for _, e := range env {
		if len(e) > len(prefix) && e[:len(prefix)] == prefix {
			return env
		}
	}
	return append(env, prefix+fallback)
}

// setEnv strips any pre-existing entries for `key` from env and appends
// `key=value`. Use this instead of ensureEnv when the new value must take
// precedence over inherited values (e.g., HOME/USER when switching UID).
func setEnv(env []string, key, value string) []string {
	prefix := key + "="
	filtered := env[:0]
	for _, e := range env {
		if len(e) > len(prefix) && e[:len(prefix)] == prefix {
			continue
		}
		filtered = append(filtered, e)
	}
	return append(filtered, prefix+value)
}

// ---------------------------------------------------------------------------
// State Persistence
// ---------------------------------------------------------------------------

// persistState saves gateway state after a mutation. Best-effort: logs but does not fail.
func persistState(mcpMgr *mcp.Manager, skillsMgr *skills.Manager) {
	if err := saveGatewayState(mcpMgr, skillsMgr, gatewayStateFile); err != nil {
		log.Printf("[agent-gateway] WARNING: failed to persist state: %v", err)
	}
}

// ---------------------------------------------------------------------------
// HTTP Handlers
// ---------------------------------------------------------------------------

func healthHandler(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Content-Type", "application/json")
	uptimeMs := time.Since(startTime).Milliseconds()
	fmt.Fprintf(w, `{"status":"ok","uptime_ms":%d}`, uptimeMs)
}

func messageHandler(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}

	// Only one agent command at a time.
	if !busy.TryLock() {
		http.Error(w, `{"error":"agent is busy with another request"}`, http.StatusConflict)
		return
	}
	defer busy.Unlock()

	var req MessageRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		http.Error(w, fmt.Sprintf(`{"error":"invalid json: %v"}`, err), http.StatusBadRequest)
		return
	}

	agentName := normalizeAgent(req.Agent)
	sess := getSession(agentName)
	bin, args := buildAgentCommand(&req, sess)

	timeout := defaultTimeout
	if req.Timeout > 0 {
		timeout = time.Duration(req.Timeout) * time.Second
	}
	ctx, cancel := context.WithTimeout(r.Context(), timeout)
	defer cancel()

	// SSE headers
	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-cache")
	w.Header().Set("Connection", "keep-alive")
	w.Header().Set("X-Accel-Buffering", "no")
	w.WriteHeader(http.StatusOK)

	log.Printf("[agent-gateway] message #%d to %s: %s %v",
		sess.messageCount+1, agentName, bin, args)

	exitCode := streamCommand(ctx, w, bin, args, req.Env, agentName, sess)

	code := exitCode
	sseWrite(w, SSEEvent{Type: "exit", Code: &code})

	sess.messageCount++
}

func execHandler(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}

	if !busy.TryLock() {
		http.Error(w, `{"error":"agent is busy with another request"}`, http.StatusConflict)
		return
	}
	defer busy.Unlock()

	var req ExecRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		http.Error(w, fmt.Sprintf(`{"error":"invalid json: %v"}`, err), http.StatusBadRequest)
		return
	}

	timeout := defaultTimeout
	if req.Timeout > 0 {
		timeout = time.Duration(req.Timeout) * time.Second
	}
	ctx, cancel := context.WithTimeout(r.Context(), timeout)
	defer cancel()

	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-cache")
	w.Header().Set("Connection", "keep-alive")
	w.Header().Set("X-Accel-Buffering", "no")
	w.WriteHeader(http.StatusOK)

	log.Printf("[agent-gateway] exec: %s %v", req.Command, req.Args)

	exitCode := streamCommand(ctx, w, req.Command, req.Args, req.Env, "", nil)

	code := exitCode
	sseWrite(w, SSEEvent{Type: "exit", Code: &code})
}

func stopHandler(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	fmt.Fprint(w, `{"status":"stopping"}`)
	log.Println("[agent-gateway] stop requested, shutting down")
	go func() {
		time.Sleep(100 * time.Millisecond)
		os.Exit(0)
	}()
}

// ---------------------------------------------------------------------------
// MCP Management Handlers
// ---------------------------------------------------------------------------

// mcpMutationResponse is returned by all MCP mutation endpoints (add, update,
// remove, enable, disable). Includes per-agent compatibility info and any
// warnings from config generation.
type mcpMutationResponse struct {
	Status   string                                `json:"status"`
	Name     string                                `json:"name"`
	Agents   map[string]*mcp.ServerAgentStatus     `json:"agents,omitempty"`
	Warnings []string                              `json:"warnings,omitempty"`
}

func mcpListHandler(mgr *mcp.Manager) http.HandlerFunc {
	type serverEntry struct {
		*mcp.McpServerDef
		Agents map[string]*mcp.ServerAgentStatus `json:"agents,omitempty"`
	}
	return func(w http.ResponseWriter, r *http.Request) {
		servers := mgr.ListServers()
		result := make(map[string]*serverEntry, len(servers))
		for name, def := range servers {
			result[name] = &serverEntry{
				McpServerDef: def,
				Agents:       mgr.ServerCompatInfo(name),
			}
		}
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(result)
	}
}

func mcpAddHandler(mgr *mcp.Manager, skillsMgr *skills.Manager) http.HandlerFunc {
	type addRequest struct {
		Name    string            `json:"name"`
		Command string            `json:"command"`
		Args    []string          `json:"args"`
		Env     map[string]string `json:"env"`
		Enabled bool              `json:"enabled"`
	}
	return func(w http.ResponseWriter, r *http.Request) {
		var req addRequest
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			http.Error(w, fmt.Sprintf(`{"error":"invalid json: %v"}`, err), http.StatusBadRequest)
			return
		}
		if req.Name == "" || req.Command == "" {
			http.Error(w, `{"error":"name and command are required"}`, http.StatusBadRequest)
			return
		}
		mgr.AddServer(req.Name, &mcp.McpServerDef{
			Command: req.Command,
			Args:    req.Args,
			Env:     req.Env,
			Enabled: req.Enabled,
			Source:  "runtime",
		})

		var warnings []string
		if err := mgr.GenerateAllConfigs(); err != nil {
			log.Printf("[agent-gateway] WARNING: failed to regenerate MCP configs: %v", err)
			warnings = append(warnings, fmt.Sprintf("config generation: %v", err))
		}
		persistState(mgr, skillsMgr)

		resp := mcpMutationResponse{
			Status:   "added",
			Name:     req.Name,
			Agents:   mgr.ServerCompatInfo(req.Name),
			Warnings: warnings,
		}
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusCreated)
		json.NewEncoder(w).Encode(resp)
	}
}

func mcpUpdateHandler(mgr *mcp.Manager, skillsMgr *skills.Manager) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		name := r.PathValue("name")
		if name == "" {
			http.Error(w, `{"error":"server name required"}`, http.StatusBadRequest)
			return
		}
		var def mcp.McpServerDef
		if err := json.NewDecoder(r.Body).Decode(&def); err != nil {
			http.Error(w, fmt.Sprintf(`{"error":"invalid json: %v"}`, err), http.StatusBadRequest)
			return
		}
		if mgr.GetServer(name) == nil {
			http.Error(w, fmt.Sprintf(`{"error":"server %q not found"}`, name), http.StatusNotFound)
			return
		}
		mgr.AddServer(name, &def)
		var warnings []string
		if err := mgr.GenerateAllConfigs(); err != nil {
			log.Printf("[agent-gateway] WARNING: failed to regenerate MCP configs: %v", err)
			warnings = append(warnings, fmt.Sprintf("config generation: %v", err))
		}
		persistState(mgr, skillsMgr)
		resp := mcpMutationResponse{Status: "updated", Name: name, Agents: mgr.ServerCompatInfo(name), Warnings: warnings}
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(resp)
	}
}

func mcpDeleteHandler(mgr *mcp.Manager, skillsMgr *skills.Manager) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		name := r.PathValue("name")
		if name == "" {
			http.Error(w, `{"error":"server name required"}`, http.StatusBadRequest)
			return
		}
		if mgr.GetServer(name) == nil {
			http.Error(w, fmt.Sprintf(`{"error":"server %q not found"}`, name), http.StatusNotFound)
			return
		}
		mgr.RemoveServer(name)
		var warnings []string
		if err := mgr.GenerateAllConfigs(); err != nil {
			log.Printf("[agent-gateway] WARNING: failed to regenerate MCP configs: %v", err)
			warnings = append(warnings, fmt.Sprintf("config generation: %v", err))
		}
		persistState(mgr, skillsMgr)
		resp := mcpMutationResponse{Status: "removed", Name: name, Warnings: warnings}
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(resp)
	}
}

func mcpEnableHandler(mgr *mcp.Manager, skillsMgr *skills.Manager) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		name := r.PathValue("name")
		if mgr.GetServer(name) == nil {
			http.Error(w, fmt.Sprintf(`{"error":"server %q not found"}`, name), http.StatusNotFound)
			return
		}
		mgr.EnableServer(name)
		var warnings []string
		if err := mgr.GenerateAllConfigs(); err != nil {
			log.Printf("[agent-gateway] WARNING: failed to regenerate MCP configs: %v", err)
			warnings = append(warnings, fmt.Sprintf("config generation: %v", err))
		}
		persistState(mgr, skillsMgr)
		resp := mcpMutationResponse{Status: "enabled", Name: name, Agents: mgr.ServerCompatInfo(name), Warnings: warnings}
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(resp)
	}
}

func mcpDisableHandler(mgr *mcp.Manager, skillsMgr *skills.Manager) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		name := r.PathValue("name")
		if mgr.GetServer(name) == nil {
			http.Error(w, fmt.Sprintf(`{"error":"server %q not found"}`, name), http.StatusNotFound)
			return
		}
		mgr.DisableServer(name)
		var warnings []string
		if err := mgr.GenerateAllConfigs(); err != nil {
			log.Printf("[agent-gateway] WARNING: failed to regenerate MCP configs: %v", err)
			warnings = append(warnings, fmt.Sprintf("config generation: %v", err))
		}
		persistState(mgr, skillsMgr)
		resp := mcpMutationResponse{Status: "disabled", Name: name, Warnings: warnings}
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(resp)
	}
}

func mcpRegenerateHandler(mgr *mcp.Manager) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if err := mgr.GenerateAllConfigs(); err != nil {
			http.Error(w, fmt.Sprintf(`{"error":"regeneration failed: %v"}`, err), http.StatusInternalServerError)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		fmt.Fprint(w, `{"status":"regenerated"}`)
	}
}

// ---------------------------------------------------------------------------
// Skills Management Handlers
// ---------------------------------------------------------------------------

func skillsListHandler(mgr *skills.Manager) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(mgr.ListSkills())
	}
}

func skillsAddHandler(mgr *skills.Manager, mcpMgr *mcp.Manager) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var def skills.SkillDef
		if err := json.NewDecoder(r.Body).Decode(&def); err != nil {
			http.Error(w, fmt.Sprintf(`{"error":"invalid json: %v"}`, err), http.StatusBadRequest)
			return
		}
		if def.Name == "" {
			http.Error(w, `{"error":"name is required"}`, http.StatusBadRequest)
			return
		}
		def.Source = skills.SourceRuntime
		mgr.AddSkill(def.Name, &def)
		if err := mgr.GenerateAllConfigs(); err != nil {
			log.Printf("[agent-gateway] WARNING: failed to regenerate skill configs: %v", err)
		}
		persistState(mcpMgr, mgr)
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusCreated)
		fmt.Fprintf(w, `{"status":"added","name":%q}`, def.Name)
	}
}

func skillsDeleteHandler(mgr *skills.Manager, mcpMgr *mcp.Manager) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		name := r.PathValue("name")
		if name == "" {
			http.Error(w, `{"error":"skill name required"}`, http.StatusBadRequest)
			return
		}
		if mgr.GetSkill(name) == nil {
			http.Error(w, fmt.Sprintf(`{"error":"skill %q not found"}`, name), http.StatusNotFound)
			return
		}
		mgr.RemoveSkill(name)
		if err := mgr.GenerateAllConfigs(); err != nil {
			log.Printf("[agent-gateway] WARNING: failed to regenerate skill configs: %v", err)
		}
		persistState(mcpMgr, mgr)
		w.Header().Set("Content-Type", "application/json")
		fmt.Fprintf(w, `{"status":"removed","name":%q}`, name)
	}
}

func skillsGetHandler(mgr *skills.Manager) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		name := r.PathValue("name")
		if name == "" {
			http.Error(w, `{"error":"skill name required"}`, http.StatusBadRequest)
			return
		}
		skill := mgr.GetSkill(name)
		if skill == nil {
			http.Error(w, fmt.Sprintf(`{"error":"skill %q not found"}`, name), http.StatusNotFound)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(skill)
	}
}

func skillsRegenerateHandler(mgr *skills.Manager) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if err := mgr.GenerateAllConfigs(); err != nil {
			http.Error(w, fmt.Sprintf(`{"error":"regeneration failed: %v"}`, err), http.StatusInternalServerError)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		fmt.Fprint(w, `{"status":"regenerated"}`)
	}
}

// ---------------------------------------------------------------------------
// Agent Definition Handlers
// ---------------------------------------------------------------------------

func agentGetHandler(mgr *skills.Manager) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		name, prompt := mgr.GetAgentDefinition()
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(map[string]interface{}{
			"name":        name,
			"prompt":      prompt,
			"auto_mode":   autoMode,
			"permissions": permissions,
		})
	}
}

func agentSetHandler(mgr *skills.Manager, mcpMgr *mcp.Manager) http.HandlerFunc {
	type setRequest struct {
		Name   string `json:"name"`
		Prompt string `json:"prompt"`
	}
	return func(w http.ResponseWriter, r *http.Request) {
		var req setRequest
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			http.Error(w, fmt.Sprintf(`{"error":"invalid json: %v"}`, err), http.StatusBadRequest)
			return
		}
		if req.Name == "" {
			http.Error(w, `{"error":"name is required"}`, http.StatusBadRequest)
			return
		}
		mgr.SetAgentDefinition(req.Name, req.Prompt)
		if err := mgr.GenerateAllConfigs(); err != nil {
			log.Printf("[agent-gateway] WARNING: failed to regenerate skill/prompt configs: %v", err)
		}
		persistState(mcpMgr, mgr)
		w.Header().Set("Content-Type", "application/json")
		fmt.Fprintf(w, `{"status":"set","name":%q}`, req.Name)
	}
}

// ClaudeSettings holds Claude-specific settings written to ~/.claude/settings.json.
type ClaudeSettings struct {
	Theme string `json:"theme,omitempty"`
}

// BootstrapRequest is the JSON body for POST /api/v1/agent/bootstrap.
// It sets up the complete agent configuration in one call.
type BootstrapRequest struct {
	AgentName      string                       `json:"agent_name"`
	Prompt         string                       `json:"prompt"`
	Skills         []skills.SkillDef            `json:"skills"`
	McpServers     map[string]*mcp.McpServerDef `json:"mcp_servers,omitempty"`
	AutoMode       bool                         `json:"auto_mode"`
	Permissions    string                       `json:"permissions,omitempty"`
	AgentType      string                       `json:"agent_type,omitempty"`
	ClaudeSettings *ClaudeSettings              `json:"claude_settings,omitempty"`
}

// autoMode controls whether agents run fully autonomously (no confirmation prompts).
// Set via bootstrap request.
var autoMode bool

// permissions controls the agent permission level: "default", "accept_edits", "allow_all".
// When auto_mode is true, permissions is forced to "allow_all".
var permissions string

// writeClaudeSettings merges sandbox-specified Claude settings into
// ~/.claude/settings.json. Reads the existing file first so user preferences
// set through the TUI (e.g. a theme they chose previously) are preserved for
// any keys not overridden by the sandbox config.
func writeClaudeSettings(s *ClaudeSettings) {
	const path = "/home/developer/.claude/settings.json"

	existing := map[string]interface{}{}
	if data, err := os.ReadFile(path); err == nil {
		_ = json.Unmarshal(data, &existing)
	}

	if s.Theme != "" {
		existing["theme"] = s.Theme
	}

	data, err := json.MarshalIndent(existing, "", "  ")
	if err != nil {
		log.Printf("[agent-gateway] WARNING: failed to marshal Claude settings: %v", err)
		return
	}
	if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
		log.Printf("[agent-gateway] WARNING: failed to create .claude dir: %v", err)
		return
	}
	if err := os.WriteFile(path, data, 0644); err != nil {
		log.Printf("[agent-gateway] WARNING: failed to write Claude settings: %v", err)
		return
	}
	log.Printf("[agent-gateway] wrote Claude settings (theme=%q)", s.Theme)
}

func agentBootstrapHandler(skillsMgr *skills.Manager, mcpMgr *mcp.Manager) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var req BootstrapRequest
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			http.Error(w, fmt.Sprintf(`{"error":"invalid json: %v"}`, err), http.StatusBadRequest)
			return
		}

		// Reset config-sourced items; runtime-added (TUI) items are preserved.
		skillsMgr.ResetConfig()
		mcpMgr.ResetConfig()

		// Set agent type — controls which config files are generated.
		if req.AgentType != "" {
			skillsMgr.SetAgentType(req.AgentType)
			mcpMgr.SetAgentType(req.AgentType)
			log.Printf("[agent-gateway] agent type set to %q", req.AgentType)
		}

		// Set agent definition
		if req.AgentName != "" {
			skillsMgr.SetAgentDefinition(req.AgentName, req.Prompt)
		}

		// Set auto mode and permissions
		autoMode = req.AutoMode
		permissions = req.Permissions
		if autoMode {
			permissions = "allow_all"
			log.Printf("[agent-gateway] auto mode enabled (permissions forced to allow_all)")
		} else if permissions != "" {
			log.Printf("[agent-gateway] permissions set to %q", permissions)
		}

		// Apply Claude-specific settings (e.g. theme) if provided.
		if req.ClaudeSettings != nil {
			writeClaudeSettings(req.ClaudeSettings)
		}

		// Add all skills (tagged as config-sourced)
		for i := range req.Skills {
			s := &req.Skills[i]
			s.Source = skills.SourceConfig
			skillsMgr.AddSkill(s.Name, s)
		}

		// Add MCP servers if provided (tagged as config-sourced)
		for name, def := range req.McpServers {
			def.Source = "config"
			mcpMgr.AddServer(name, def)
		}

		// Regenerate all configs
		var errs []string
		if err := skillsMgr.GenerateAllConfigs(); err != nil {
			errs = append(errs, fmt.Sprintf("skills: %v", err))
		}
		if err := mcpMgr.GenerateAllConfigs(); err != nil {
			errs = append(errs, fmt.Sprintf("mcp: %v", err))
		}

		if len(errs) > 0 {
			log.Printf("[agent-gateway] WARNING: bootstrap config generation errors: %v", errs)
		}

		persistState(mcpMgr, skillsMgr)

		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusCreated)
		fmt.Fprintf(w, `{"status":"bootstrapped","agent":%q,"skills":%d,"mcp_servers":%d}`,
			req.AgentName, len(req.Skills), len(req.McpServers))
	}
}

// ---------------------------------------------------------------------------
// Agent Restart Handler
// ---------------------------------------------------------------------------

// RestartRequest is the JSON body for POST /api/v1/agent/restart.
type RestartRequest struct {
	Agent  string `json:"agent"`
	Reason string `json:"reason"` // "skills_update", "mcp_update", "agent_change"
}

// RestartResponse is returned by POST /api/v1/agent/restart.
type RestartResponse struct {
	SessionID     string `json:"session_id"`
	WasGenerating bool   `json:"was_generating"`
	Restarted     bool   `json:"restarted"`
}

func agentRestartHandler() http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var req RestartRequest
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			http.Error(w, fmt.Sprintf(`{"error":"invalid json: %v"}`, err), http.StatusBadRequest)
			return
		}

		agentName := normalizeAgent(req.Agent)
		if agentName == "" {
			http.Error(w, `{"error":"agent is required"}`, http.StatusBadRequest)
			return
		}

		sess := getSession(agentName)
		resp := RestartResponse{
			SessionID: sess.sessionID,
		}

		// Determine if agent was actively generating output.
		if sess.pid > 0 && !sess.lastOutputTime.IsZero() &&
			time.Since(sess.lastOutputTime) < 5*time.Second {
			sess.wasGenerating = true
			resp.WasGenerating = true
		}

		sess.interruptReason = req.Reason

		// Kill the running agent process if any.
		if sess.pid > 0 {
			log.Printf("[agent-gateway] restarting %s (pid=%d, reason=%s, session=%s, wasGenerating=%v)",
				agentName, sess.pid, req.Reason, sess.sessionID, sess.wasGenerating)

			// SIGTERM first for graceful shutdown.
			_ = syscall.Kill(sess.pid, syscall.SIGTERM)

			// Give it 3 seconds, then SIGKILL.
			go func(pid int) {
				time.Sleep(3 * time.Second)
				// Check if still alive (best effort).
				if err := syscall.Kill(pid, 0); err == nil {
					log.Printf("[agent-gateway] force-killing %s (pid=%d)", agentName, pid)
					_ = syscall.Kill(pid, syscall.SIGKILL)
				}
			}(sess.pid)

			resp.Restarted = true
		} else {
			log.Printf("[agent-gateway] restart requested for %s but no active process", agentName)
		}

		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(resp)
	}
}

// ---------------------------------------------------------------------------
// Route Setup
// ---------------------------------------------------------------------------

// setupMux creates and registers all HTTP routes on a new ServeMux.
// Extracted from main() so that tests can use the same routing.
func setupMux(mcpMgr *mcp.Manager, skillsMgr *skills.Manager) *http.ServeMux {
	mux := http.NewServeMux()
	mux.HandleFunc("/health", healthHandler)
	mux.HandleFunc("/api/v1/health", healthHandler)
	mux.HandleFunc("/api/v1/message", messageHandler)
	mux.HandleFunc("/api/v1/exec", execHandler)
	mux.HandleFunc("/api/v1/stop", stopHandler)

	// MCP management routes
	mux.HandleFunc("GET /api/v1/mcp/servers", mcpListHandler(mcpMgr))
	mux.HandleFunc("POST /api/v1/mcp/servers", mcpAddHandler(mcpMgr, skillsMgr))
	mux.HandleFunc("PUT /api/v1/mcp/servers/{name}", mcpUpdateHandler(mcpMgr, skillsMgr))
	mux.HandleFunc("DELETE /api/v1/mcp/servers/{name}", mcpDeleteHandler(mcpMgr, skillsMgr))
	mux.HandleFunc("POST /api/v1/mcp/servers/{name}/enable", mcpEnableHandler(mcpMgr, skillsMgr))
	mux.HandleFunc("POST /api/v1/mcp/servers/{name}/disable", mcpDisableHandler(mcpMgr, skillsMgr))
	mux.HandleFunc("POST /api/v1/mcp/regenerate", mcpRegenerateHandler(mcpMgr))

	// Skills management routes
	mux.HandleFunc("GET /api/v1/skills", skillsListHandler(skillsMgr))
	mux.HandleFunc("POST /api/v1/skills", skillsAddHandler(skillsMgr, mcpMgr))
	mux.HandleFunc("GET /api/v1/skills/{name}", skillsGetHandler(skillsMgr))
	mux.HandleFunc("DELETE /api/v1/skills/{name}", skillsDeleteHandler(skillsMgr, mcpMgr))
	mux.HandleFunc("POST /api/v1/skills/regenerate", skillsRegenerateHandler(skillsMgr))

	// Agent definition routes
	mux.HandleFunc("GET /api/v1/agent", agentGetHandler(skillsMgr))
	mux.HandleFunc("POST /api/v1/agent", agentSetHandler(skillsMgr, mcpMgr))
	mux.HandleFunc("POST /api/v1/agent/bootstrap", agentBootstrapHandler(skillsMgr, mcpMgr))
	mux.HandleFunc("POST /api/v1/agent/restart", agentRestartHandler())

	return mux
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

func main() {
	// Handle --mount subcommand: direct mount syscall for init scripts.
	// util-linux's mount binary can refuse even for root in micro-VMs,
	// so the init script uses this instead.
	if len(os.Args) >= 5 && os.Args[1] == "--mount" {
		// Usage: agent-gateway --mount <source> <target> <fstype>
		src, tgt, fstype := os.Args[2], os.Args[3], os.Args[4]
		if err := sysMount(src, tgt, fstype); err != nil {
			fmt.Fprintf(os.Stderr, "mount(%s, %s, %s) failed: %v\n", src, tgt, fstype, err)
			os.Exit(1)
		}
		fmt.Printf("mount(%s, %s, %s) OK\n", src, tgt, fstype)
		os.Exit(0)
	}

	skipNetworkInit := flag.Bool("skip-network-init", false, "Skip network configuration (done by init script)")
	flag.Parse()

	log.SetFlags(log.Ldate | log.Ltime | log.Lmicroseconds)
	log.Printf("[agent-gateway] starting... (PID: %d)", os.Getpid())

	// As PID 1 we must reap orphan children to avoid zombies.
	// We do NOT ignore SIGCHLD because Go's exec.Cmd.Wait() uses waitpid()
	// and ignoring SIGCHLD causes the kernel to auto-reap, making Wait() fail
	// with "no child processes".
	// Instead, start a goroutine that reaps any orphan processes.
	go func() {
		for {
			var ws syscall.WaitStatus
			pid, err := syscall.Wait4(-1, &ws, syscall.WNOHANG, nil)
			if pid <= 0 || err != nil {
				time.Sleep(1 * time.Second)
			}
		}
	}()

	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, syscall.SIGTERM, syscall.SIGINT)

	// Configure networking before starting the HTTP server
	// (skipped when nanosb-init.sh has already done this).
	if !*skipNetworkInit {
		initNetwork()
		log.Println("[agent-gateway] network configured")
	} else {
		log.Println("[agent-gateway] skipping network init (handled by init script)")
	}

	// Ensure working directory exists.
	_ = os.MkdirAll(defaultWorkDir, 0755)

	// Initialize MCP manager — starts empty, users add servers at runtime.
	mcpMgr := mcp.NewManager()

	// Initialize skills manager (starts empty; populated by bootstrap or runtime API).
	skillsMgr := skills.NewManager()
	log.Println("[agent-gateway] skills manager initialized")

	// Reload persisted state from previous run (no-op on first boot).
	if err := loadGatewayState(mcpMgr, skillsMgr, gatewayStateFile); err != nil {
		log.Printf("[agent-gateway] WARNING: could not load prior state: %v", err)
	}

	if err := mcpMgr.GenerateAllConfigs(); err != nil {
		log.Printf("[agent-gateway] WARNING: failed to generate MCP configs: %v", err)
	} else {
		log.Println("[agent-gateway] MCP configs generated for all agents")
	}

	if err := skillsMgr.GenerateAllConfigs(); err != nil {
		log.Printf("[agent-gateway] WARNING: could not regenerate skill configs after state load: %v", err)
	}

	// Register routes.
	mux := setupMux(mcpMgr, skillsMgr)

	server := &http.Server{
		Addr:         listenAddr,
		Handler:      mux,
		ReadTimeout:  10 * time.Second,
		WriteTimeout: 0, // SSE streams can be long-lived
	}

	// Create TCP listener first to catch bind errors synchronously.
	// This ensures we fail fast if the port is already in use.
	listener, err := net.Listen("tcp", listenAddr)
	if err != nil {
		log.Fatalf("[agent-gateway] failed to bind to %s: %v", listenAddr, err)
	}
	log.Printf("[agent-gateway] listening on %s", listenAddr)

	// Start HTTP server in background on the bound listener.
	go func() {
		if err := server.Serve(listener); err != http.ErrServerClosed {
			log.Fatalf("[agent-gateway] server error: %v", err)
		}
	}()

	// Wait for shutdown signal.
	sig := <-sigCh
	log.Printf("[agent-gateway] received %v, shutting down", sig)
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	_ = server.Shutdown(ctx)
}
