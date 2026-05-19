package main

import (
	"os"
	"strings"
	"testing"
)

// --- extractSessionID tests ---

func TestExtractSessionID_Claude(t *testing.T) {
	line := `{"type":"system","session_id":"sess-abc-123","message":"starting"}`
	id := extractSessionID("claude", line)
	if id != "sess-abc-123" {
		t.Errorf("expected sess-abc-123, got %q", id)
	}
}

func TestExtractSessionID_Claude_NoSessionID(t *testing.T) {
	line := `{"type":"stdout","data":"hello world"}`
	id := extractSessionID("claude", line)
	if id != "" {
		t.Errorf("expected empty, got %q", id)
	}
}

func TestExtractSessionID_Claude_NotJSON(t *testing.T) {
	id := extractSessionID("claude", "just plain text")
	if id != "" {
		t.Errorf("expected empty, got %q", id)
	}
}

func TestExtractSessionID_Goose_JSON(t *testing.T) {
	line := `{"session_id":"goose-sess-456"}`
	id := extractSessionID("goose", line)
	if id != "goose-sess-456" {
		t.Errorf("expected goose-sess-456, got %q", id)
	}
}

func TestExtractSessionID_Goose_TextPattern(t *testing.T) {
	line := "session: abc123def"
	id := extractSessionID("goose", line)
	if id != "abc123def" {
		t.Errorf("expected abc123def, got %q", id)
	}
}

func TestExtractSessionID_Goose_TextPatternCase(t *testing.T) {
	line := "Session: XYZ-789"
	id := extractSessionID("goose", line)
	if id != "XYZ-789" {
		t.Errorf("expected XYZ-789, got %q", id)
	}
}

func TestExtractSessionID_Goose_NoMatch(t *testing.T) {
	id := extractSessionID("goose", "just some output text")
	if id != "" {
		t.Errorf("expected empty, got %q", id)
	}
}

func TestExtractSessionID_Codex(t *testing.T) {
	line := `{"session_id":"sess_codex_abc","status":"running"}`
	id := extractSessionID("codex", line)
	if id != "sess_codex_abc" {
		t.Errorf("expected sess_codex_abc, got %q", id)
	}
}

func TestExtractSessionID_Cursor(t *testing.T) {
	line := `{"chatId":"chat_xyz789","type":"start"}`
	id := extractSessionID("cursor", line)
	if id != "chat_xyz789" {
		t.Errorf("expected chat_xyz789, got %q", id)
	}
}

func TestExtractSessionID_UnknownAgent(t *testing.T) {
	id := extractSessionID("unknown", `{"session_id":"xxx"}`)
	if id != "" {
		t.Errorf("expected empty for unknown agent, got %q", id)
	}
}

// --- normalizeAgent tests ---

func TestNormalizeAgent(t *testing.T) {
	cases := []struct {
		input, expected string
	}{
		{"claude", "claude"},
		{"claude-code", "claude"},
		{"cursor", "cursor"},
		{"cursor-agent", "cursor"},
		{"goose", "goose"},
		{"codex", "codex"},
		{"custom-bot", "custom-bot"},
	}
	for _, tc := range cases {
		got := normalizeAgent(tc.input)
		if got != tc.expected {
			t.Errorf("normalizeAgent(%q) = %q, want %q", tc.input, got, tc.expected)
		}
	}
}

// --- buildAgentCommand tests ---

func TestBuildAgentCommand_Claude_FirstMessage(t *testing.T) {
	req := &MessageRequest{Agent: "claude", Message: "hello"}
	sess := &agentSession{}
	bin, args := buildAgentCommand(req, sess)
	if bin != "claude" {
		t.Errorf("expected bin=claude, got %q", bin)
	}
	if args[0] != "--print" || args[1] != "hello" {
		t.Errorf("expected --print hello, got %v", args)
	}
	// Should NOT have --continue or --resume
	for _, arg := range args {
		if arg == "--continue" || arg == "--resume" {
			t.Errorf("first message should not have %s", arg)
		}
	}
}

func TestBuildAgentCommand_Claude_Continue(t *testing.T) {
	req := &MessageRequest{Agent: "claude", Message: "follow up"}
	sess := &agentSession{messageCount: 1}
	_, args := buildAgentCommand(req, sess)

	found := false
	for _, arg := range args {
		if arg == "--continue" {
			found = true
		}
	}
	if !found {
		t.Error("expected --continue for messageCount > 0 without sessionID")
	}
}

func TestBuildAgentCommand_Claude_Resume(t *testing.T) {
	req := &MessageRequest{Agent: "claude", Message: "after restart"}
	sess := &agentSession{messageCount: 3, sessionID: "sess-123"}
	_, args := buildAgentCommand(req, sess)

	foundResume := false
	foundID := false
	for i, arg := range args {
		if arg == "--resume" {
			foundResume = true
			if i+1 < len(args) && args[i+1] == "sess-123" {
				foundID = true
			}
		}
	}
	if !foundResume || !foundID {
		t.Errorf("expected --resume sess-123, got %v", args)
	}
}

func TestBuildAgentCommand_Goose_FirstMessage(t *testing.T) {
	req := &MessageRequest{Agent: "goose", Message: "start"}
	sess := &agentSession{}
	bin, args := buildAgentCommand(req, sess)
	if bin != "goose" {
		t.Errorf("expected bin=goose, got %q", bin)
	}
	if len(args) < 2 || args[0] != "run" || args[1] != "--text" {
		t.Errorf("expected goose run --text, got %v", args)
	}
}

func TestBuildAgentCommand_Goose_Resume(t *testing.T) {
	req := &MessageRequest{Agent: "goose", Message: "continue"}
	sess := &agentSession{messageCount: 1, sessionID: "goose-sess-1"}
	bin, args := buildAgentCommand(req, sess)
	if bin != "goose" {
		t.Errorf("expected bin=goose, got %q", bin)
	}
	if len(args) < 4 || args[0] != "session" || args[1] != "resume" || args[2] != "goose-sess-1" {
		t.Errorf("expected goose session resume <id>, got %v", args)
	}
}

func TestBuildAgentCommand_Codex_FirstMessage(t *testing.T) {
	req := &MessageRequest{Agent: "codex", Message: "code this"}
	sess := &agentSession{}
	bin, args := buildAgentCommand(req, sess)
	if bin != "codex" {
		t.Errorf("expected bin=codex, got %q", bin)
	}
	if len(args) < 3 || args[0] != "exec" || args[1] != "--json" {
		t.Errorf("expected codex exec --json, got %v", args)
	}
}

func TestBuildAgentCommand_Codex_Resume(t *testing.T) {
	req := &MessageRequest{Agent: "codex", Message: "more"}
	sess := &agentSession{messageCount: 1, sessionID: "codex-sess-1"}
	bin, args := buildAgentCommand(req, sess)
	if bin != "codex" {
		t.Errorf("expected bin=codex, got %q", bin)
	}
	if len(args) < 2 || args[0] != "resume" || args[1] != "codex-sess-1" {
		t.Errorf("expected codex resume <id>, got %v", args)
	}
}

func TestBuildAgentCommand_Cursor_FirstMessage(t *testing.T) {
	req := &MessageRequest{Agent: "cursor", Message: "build this"}
	sess := &agentSession{}
	bin, args := buildAgentCommand(req, sess)
	if bin != "cursor-agent" {
		t.Errorf("expected bin=cursor-agent, got %q", bin)
	}
	if len(args) < 2 || args[0] != "--message" {
		t.Errorf("expected --message, got %v", args)
	}
}

func TestBuildAgentCommand_Cursor_Resume(t *testing.T) {
	req := &MessageRequest{Agent: "cursor", Message: "next"}
	sess := &agentSession{messageCount: 1, sessionID: "chat-abc"}
	_, args := buildAgentCommand(req, sess)

	foundResume := false
	for i, arg := range args {
		if arg == "--resume" && i+1 < len(args) && args[i+1] == "chat-abc" {
			foundResume = true
		}
	}
	if !foundResume {
		t.Errorf("expected --resume chat-abc, got %v", args)
	}
}

func TestBuildAgentCommand_Continuation(t *testing.T) {
	req := &MessageRequest{Agent: "claude", Message: "next step"}
	sess := &agentSession{
		messageCount:    2,
		sessionID:       "sess-456",
		wasGenerating:   true,
		interruptReason: "skills_update",
	}

	_, args := buildAgentCommand(req, sess)

	// The message should contain the continuation prefix
	message := args[1] // args[0] is --print, args[1] is message
	if !strings.Contains(message, "interrupted to apply configuration changes") {
		t.Error("expected continuation message prefix")
	}
	if !strings.Contains(message, "skills_update") {
		t.Error("expected interrupt reason in continuation")
	}
	if !strings.Contains(message, "next step") {
		t.Error("expected original message after continuation")
	}

	// Flags should be cleared after use
	if sess.wasGenerating {
		t.Error("expected wasGenerating to be cleared")
	}
	if sess.interruptReason != "" {
		t.Error("expected interruptReason to be cleared")
	}
}

func TestBuildAgentCommand_Continuation_EmptyMessage(t *testing.T) {
	req := &MessageRequest{Agent: "claude", Message: ""}
	sess := &agentSession{
		messageCount:    1,
		sessionID:       "sess-789",
		wasGenerating:   true,
		interruptReason: "agent_change",
	}

	_, args := buildAgentCommand(req, sess)

	message := args[1]
	if !strings.Contains(message, "interrupted to apply configuration changes") {
		t.Error("expected continuation message when original is empty")
	}
	if strings.Contains(message, "\n\n") {
		t.Error("should not have double newline when original message is empty")
	}
}

func TestBuildAgentCommand_NoContinuation_WhenNotGenerating(t *testing.T) {
	req := &MessageRequest{Agent: "claude", Message: "hello"}
	sess := &agentSession{
		messageCount:    1,
		wasGenerating:   false,
		interruptReason: "skills_update",
	}

	_, args := buildAgentCommand(req, sess)

	message := args[1]
	if strings.Contains(message, "interrupted") {
		t.Error("should not inject continuation when wasGenerating is false")
	}
}

func TestBuildAgentCommand_NoContinuation_WhenNoReason(t *testing.T) {
	req := &MessageRequest{Agent: "claude", Message: "hello"}
	sess := &agentSession{
		messageCount:  1,
		wasGenerating: true,
		// interruptReason is empty
	}

	_, args := buildAgentCommand(req, sess)

	message := args[1]
	if strings.Contains(message, "interrupted") {
		t.Error("should not inject continuation when interruptReason is empty")
	}
}

func TestBuildAgentCommand_WithModel(t *testing.T) {
	req := &MessageRequest{Agent: "claude", Message: "hello", Model: "opus"}
	sess := &agentSession{}
	_, args := buildAgentCommand(req, sess)

	foundModel := false
	for i, arg := range args {
		if arg == "--model" && i+1 < len(args) && args[i+1] == "opus" {
			foundModel = true
		}
	}
	if !foundModel {
		t.Errorf("expected --model opus, got %v", args)
	}
}

// --- auto mode tests ---

func TestBuildAgentCommand_AutoMode_Claude(t *testing.T) {
	autoMode = true
	permissions = "allow_all"
	defer func() { autoMode = false; permissions = "" }()

	req := &MessageRequest{Agent: "claude", Message: "hello"}
	sess := &agentSession{}
	_, args := buildAgentCommand(req, sess)

	found := false
	for _, arg := range args {
		if arg == "--dangerously-skip-permissions" {
			found = true
		}
	}
	if !found {
		t.Errorf("expected --dangerously-skip-permissions in auto mode, got %v", args)
	}
}

func TestBuildAgentCommand_AutoMode_Codex(t *testing.T) {
	autoMode = true
	permissions = "allow_all"
	defer func() { autoMode = false; permissions = "" }()

	req := &MessageRequest{Agent: "codex", Message: "code this"}
	sess := &agentSession{}
	_, args := buildAgentCommand(req, sess)

	found := false
	for _, arg := range args {
		if arg == "--full-auto" {
			found = true
		}
	}
	if !found {
		t.Errorf("expected --full-auto in auto mode, got %v", args)
	}
}

func TestBuildAgentCommand_AutoMode_Cursor(t *testing.T) {
	autoMode = true
	permissions = "allow_all"
	defer func() { autoMode = false; permissions = "" }()

	req := &MessageRequest{Agent: "cursor", Message: "build this"}
	sess := &agentSession{}
	_, args := buildAgentCommand(req, sess)

	foundForce := false
	foundTrust := false
	foundMcps := false
	for _, arg := range args {
		switch arg {
		case "--force":
			foundForce = true
		case "--trust":
			foundTrust = true
		case "--approve-mcps":
			foundMcps = true
		}
	}
	if !foundForce || !foundTrust || !foundMcps {
		t.Errorf("expected --force --trust --approve-mcps in auto mode, got %v", args)
	}
}

func TestBuildAgentCommand_NoAutoMode(t *testing.T) {
	autoMode = false

	req := &MessageRequest{Agent: "claude", Message: "hello"}
	sess := &agentSession{}
	_, args := buildAgentCommand(req, sess)

	for _, arg := range args {
		if arg == "--dangerously-skip-permissions" {
			t.Error("should not have --dangerously-skip-permissions when auto mode is off")
		}
	}

	req2 := &MessageRequest{Agent: "codex", Message: "code"}
	_, args2 := buildAgentCommand(req2, sess)
	for _, arg := range args2 {
		if arg == "--full-auto" {
			t.Error("should not have --full-auto when auto mode is off")
		}
	}

	req3 := &MessageRequest{Agent: "cursor", Message: "build"}
	_, args3 := buildAgentCommand(req3, sess)
	for _, arg := range args3 {
		if arg == "--force" || arg == "--trust" || arg == "--approve-mcps" {
			t.Errorf("should not have auto mode flags when auto mode is off, got %v", args3)
			break
		}
	}
}

func TestDetectGooseProviderFromEnv_Priority(t *testing.T) {
	env := map[string]string{
		"OPENAI_API_KEY":    "openai-key",
		"ANTHROPIC_API_KEY": "anthropic-key",
		"GOOGLE_API_KEY":    "google-key",
		"GROQ_API_KEY":      "groq-key",
	}

	p := detectGooseProviderFromEnv(env)
	if p == nil {
		t.Fatal("expected provider detection, got nil")
	}
	if p.Provider != "anthropic" {
		t.Fatalf("expected anthropic priority, got %q", p.Provider)
	}
	if p.Model != "claude-sonnet-4-5-20250929" {
		t.Fatalf("expected anthropic default model, got %q", p.Model)
	}
}

func TestDetectGooseProviderFromEnv_EmptyValuesIgnored(t *testing.T) {
	env := map[string]string{
		"OPENAI_API_KEY": "   ",
	}

	if p := detectGooseProviderFromEnv(env); p != nil {
		t.Fatalf("expected nil provider when key value is empty, got %+v", p)
	}
}

func TestIsGooseProviderConfigured(t *testing.T) {
	dir := t.TempDir()
	path := dir + "/config.yaml"

	prev := gooseConfigPath
	gooseConfigPath = path
	defer func() { gooseConfigPath = prev }()

	if isGooseProviderConfigured() {
		t.Fatal("expected false when config does not exist")
	}

	if err := os.WriteFile(path, []byte("extensions:\n  foo: {}\n"), 0644); err != nil {
		t.Fatalf("write config without provider: %v", err)
	}
	if isGooseProviderConfigured() {
		t.Fatal("expected false when GOOSE_PROVIDER is absent")
	}

	if err := os.WriteFile(path, []byte("GOOSE_PROVIDER: \"openai\"\nextensions:\n  foo: {}\n"), 0644); err != nil {
		t.Fatalf("write config with provider: %v", err)
	}
	if !isGooseProviderConfigured() {
		t.Fatal("expected true when GOOSE_PROVIDER is present")
	}
}
