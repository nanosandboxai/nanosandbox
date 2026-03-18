package main

import (
	"bytes"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/nanosandboxai/agent-gateway/mcp"
	"github.com/nanosandboxai/agent-gateway/skills"
)

// newTestServer creates a test HTTP server using the same mux as production.
func newTestServer() *httptest.Server {
	// Empty config YAML — starts with no servers and no agents.
	emptyYAML := []byte("servers: {}\nagents: {}\n")
	mcpMgr, err := mcp.NewManagerFromBytes(emptyYAML)
	if err != nil {
		panic("failed to create empty MCP manager: " + err.Error())
	}
	skillsMgr := skills.NewManager()
	mux := setupMux(mcpMgr, skillsMgr)
	return httptest.NewServer(mux)
}

// --------------------------------------------------------------------------
// Skills API Integration Tests
// --------------------------------------------------------------------------

func TestIntegration_SkillsAddAndList(t *testing.T) {
	ts := newTestServer()
	defer ts.Close()

	// List should be empty initially
	resp, err := http.Get(ts.URL + "/api/v1/skills")
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(resp.Body)
	if string(body) != "{}\n" && string(body) != "null\n" {
		t.Fatalf("expected empty skills map, got: %s", body)
	}

	// Add a skill
	skillJSON := `{"name":"tdd","description":"Test-driven development","content":"# TDD\nRed-green-refactor.","version":"1.0"}`
	resp, err = http.Post(ts.URL+"/api/v1/skills", "application/json", bytes.NewBufferString(skillJSON))
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusCreated {
		body, _ = io.ReadAll(resp.Body)
		t.Fatalf("expected 201, got %d: %s", resp.StatusCode, body)
	}

	// List should now have one skill
	resp, err = http.Get(ts.URL + "/api/v1/skills")
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	var skillsMap map[string]*skills.SkillDef
	if err := json.NewDecoder(resp.Body).Decode(&skillsMap); err != nil {
		t.Fatal(err)
	}
	if len(skillsMap) != 1 {
		t.Fatalf("expected 1 skill, got %d", len(skillsMap))
	}
	if s, ok := skillsMap["tdd"]; !ok || s.Description != "Test-driven development" {
		t.Fatalf("unexpected skill: %+v", skillsMap)
	}
}

func TestIntegration_SkillsGetByName(t *testing.T) {
	ts := newTestServer()
	defer ts.Close()

	// Add a skill first
	skillJSON := `{"name":"git-workflow","description":"Git best practices","content":"# Git\nConventional commits."}`
	resp, err := http.Post(ts.URL+"/api/v1/skills", "application/json", bytes.NewBufferString(skillJSON))
	if err != nil {
		t.Fatal(err)
	}
	resp.Body.Close()

	// Get by name
	resp, err = http.Get(ts.URL + "/api/v1/skills/git-workflow")
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("expected 200, got %d", resp.StatusCode)
	}
	var skill skills.SkillDef
	if err := json.NewDecoder(resp.Body).Decode(&skill); err != nil {
		t.Fatal(err)
	}
	if skill.Name != "git-workflow" || skill.Description != "Git best practices" {
		t.Fatalf("unexpected skill: %+v", skill)
	}
}

func TestIntegration_SkillsGetNotFound(t *testing.T) {
	ts := newTestServer()
	defer ts.Close()

	resp, err := http.Get(ts.URL + "/api/v1/skills/nonexistent")
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusNotFound {
		t.Fatalf("expected 404, got %d", resp.StatusCode)
	}
}

func TestIntegration_SkillsDelete(t *testing.T) {
	ts := newTestServer()
	defer ts.Close()

	// Add then delete
	skillJSON := `{"name":"deleteme","description":"to be deleted","content":"bye"}`
	resp, err := http.Post(ts.URL+"/api/v1/skills", "application/json", bytes.NewBufferString(skillJSON))
	if err != nil {
		t.Fatal(err)
	}
	resp.Body.Close()

	req, _ := http.NewRequest("DELETE", ts.URL+"/api/v1/skills/deleteme", nil)
	resp, err = http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("expected 200, got %d", resp.StatusCode)
	}

	// Verify deleted
	resp, err = http.Get(ts.URL + "/api/v1/skills/deleteme")
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusNotFound {
		t.Fatalf("expected 404 after delete, got %d", resp.StatusCode)
	}
}

// --------------------------------------------------------------------------
// Agent API Integration Tests
// --------------------------------------------------------------------------

func TestIntegration_AgentGetSetRoundTrip(t *testing.T) {
	ts := newTestServer()
	defer ts.Close()

	// Initially empty
	resp, err := http.Get(ts.URL + "/api/v1/agent")
	if err != nil {
		t.Fatal(err)
	}
	body, _ := io.ReadAll(resp.Body)
	resp.Body.Close()
	var agentResp struct {
		Name   string `json:"name"`
		Prompt string `json:"prompt"`
	}
	if err := json.Unmarshal(body, &agentResp); err != nil {
		t.Fatal(err)
	}
	if agentResp.Name != "" || agentResp.Prompt != "" {
		t.Fatalf("expected empty agent, got: %+v", agentResp)
	}

	// Set agent
	setJSON := `{"name":"python-dev","prompt":"You are a Python developer."}`
	req, _ := http.NewRequest("POST", ts.URL+"/api/v1/agent", bytes.NewBufferString(setJSON))
	req.Header.Set("Content-Type", "application/json")
	resp, err = http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("expected 200, got %d", resp.StatusCode)
	}

	// Get agent
	resp, err = http.Get(ts.URL + "/api/v1/agent")
	if err != nil {
		t.Fatal(err)
	}
	body, _ = io.ReadAll(resp.Body)
	resp.Body.Close()
	if err := json.Unmarshal(body, &agentResp); err != nil {
		t.Fatal(err)
	}
	if agentResp.Name != "python-dev" || agentResp.Prompt != "You are a Python developer." {
		t.Fatalf("unexpected agent: %+v", agentResp)
	}
}

// --------------------------------------------------------------------------
// Bootstrap Integration Test
// --------------------------------------------------------------------------

func TestIntegration_BootstrapEndpoint(t *testing.T) {
	ts := newTestServer()
	defer ts.Close()

	bootstrapReq := BootstrapRequest{
		AgentName: "python-developer",
		Prompt:    "You are a senior Python developer.",
		Skills: []skills.SkillDef{
			{
				Name:        "tdd",
				Description: "Test-driven development",
				Content:     "# TDD\nRed-green-refactor cycle.",
				Version:     "1.0",
			},
			{
				Name:        "git-workflow",
				Description: "Git workflow",
				Content:     "# Git\nUse conventional commits.",
				Version:     "1.0",
			},
		},
		McpServers: map[string]*mcp.McpServerDef{
			"server-github": {
				Command: "npx",
				Args:    []string{"-y", "@modelcontextprotocol/server-github"},
				Enabled: true,
			},
		},
	}

	body, _ := json.Marshal(bootstrapReq)
	resp, err := http.Post(ts.URL+"/api/v1/agent/bootstrap", "application/json", bytes.NewBuffer(body))
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusCreated {
		respBody, _ := io.ReadAll(resp.Body)
		t.Fatalf("expected 201, got %d: %s", resp.StatusCode, respBody)
	}

	var bootstrapResp struct {
		Status     string `json:"status"`
		Agent      string `json:"agent"`
		Skills     int    `json:"skills"`
		McpServers int    `json:"mcp_servers"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&bootstrapResp); err != nil {
		t.Fatal(err)
	}
	if bootstrapResp.Status != "bootstrapped" {
		t.Fatalf("unexpected status: %s", bootstrapResp.Status)
	}

	// Verify agent was set
	resp, err = http.Get(ts.URL + "/api/v1/agent")
	if err != nil {
		t.Fatal(err)
	}
	agentBody, _ := io.ReadAll(resp.Body)
	resp.Body.Close()
	var agentResp struct {
		Name   string `json:"name"`
		Prompt string `json:"prompt"`
	}
	json.Unmarshal(agentBody, &agentResp)
	if agentResp.Name != "python-developer" {
		t.Fatalf("expected agent name 'python-developer', got: %s", agentResp.Name)
	}

	// Verify skills were added
	resp, err = http.Get(ts.URL + "/api/v1/skills")
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	var skillsMap map[string]*skills.SkillDef
	json.NewDecoder(resp.Body).Decode(&skillsMap)
	if len(skillsMap) != 2 {
		t.Fatalf("expected 2 skills, got %d", len(skillsMap))
	}
	if _, ok := skillsMap["tdd"]; !ok {
		t.Fatal("expected 'tdd' skill after bootstrap")
	}
	if _, ok := skillsMap["git-workflow"]; !ok {
		t.Fatal("expected 'git-workflow' skill after bootstrap")
	}

	// Verify MCP servers were added
	resp, err = http.Get(ts.URL + "/api/v1/mcp/servers")
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	var mcpServers map[string]*mcp.McpServerDef
	json.NewDecoder(resp.Body).Decode(&mcpServers)
	if _, ok := mcpServers["server-github"]; !ok {
		t.Fatal("expected 'server-github' MCP after bootstrap")
	}
}

func TestIntegration_BootstrapOverwrites(t *testing.T) {
	ts := newTestServer()
	defer ts.Close()

	// First bootstrap
	req1 := BootstrapRequest{
		AgentName: "agent-v1",
		Prompt:    "V1 prompt",
		Skills: []skills.SkillDef{
			{Name: "old-skill", Description: "old", Content: "old content"},
		},
	}
	body, _ := json.Marshal(req1)
	resp, _ := http.Post(ts.URL+"/api/v1/agent/bootstrap", "application/json", bytes.NewBuffer(body))
	resp.Body.Close()

	// Second bootstrap (should overwrite agent name/prompt, add new skill)
	req2 := BootstrapRequest{
		AgentName: "agent-v2",
		Prompt:    "V2 prompt",
		Skills: []skills.SkillDef{
			{Name: "new-skill", Description: "new", Content: "new content"},
		},
	}
	body, _ = json.Marshal(req2)
	resp, _ = http.Post(ts.URL+"/api/v1/agent/bootstrap", "application/json", bytes.NewBuffer(body))
	resp.Body.Close()

	// Agent should be updated
	resp, _ = http.Get(ts.URL + "/api/v1/agent")
	agentBody, _ := io.ReadAll(resp.Body)
	resp.Body.Close()
	var agentResp struct {
		Name string `json:"name"`
	}
	json.Unmarshal(agentBody, &agentResp)
	if agentResp.Name != "agent-v2" {
		t.Fatalf("expected agent-v2, got: %s", agentResp.Name)
	}

	// Skills should now contain both old and new (bootstrap adds, doesn't clear)
	resp, _ = http.Get(ts.URL + "/api/v1/skills")
	var skillsMap map[string]*skills.SkillDef
	json.NewDecoder(resp.Body).Decode(&skillsMap)
	resp.Body.Close()
	if _, ok := skillsMap["new-skill"]; !ok {
		t.Fatal("expected 'new-skill' after second bootstrap")
	}
}

// --------------------------------------------------------------------------
// Restart Endpoint Tests
// --------------------------------------------------------------------------

func TestIntegration_RestartNoSession(t *testing.T) {
	ts := newTestServer()
	defer ts.Close()

	reqBody := `{"agent":"claude","reason":"skills_update"}`
	resp, err := http.Post(ts.URL+"/api/v1/agent/restart", "application/json", bytes.NewBufferString(reqBody))
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()

	var restartResp RestartResponse
	if err := json.NewDecoder(resp.Body).Decode(&restartResp); err != nil {
		t.Fatal(err)
	}
	// No session ID expected since no agent has been run
	if restartResp.SessionID != "" {
		t.Fatalf("expected empty session_id, got: %s", restartResp.SessionID)
	}
	if restartResp.WasGenerating {
		t.Fatal("expected wasGenerating=false for fresh session")
	}
}

func TestIntegration_RestartMissingAgent(t *testing.T) {
	ts := newTestServer()
	defer ts.Close()

	reqBody := `{"reason":"test"}`
	resp, err := http.Post(ts.URL+"/api/v1/agent/restart", "application/json", bytes.NewBufferString(reqBody))
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("expected 400 for missing agent, got %d", resp.StatusCode)
	}
}

// --------------------------------------------------------------------------
// Health Endpoint
// --------------------------------------------------------------------------

func TestIntegration_HealthEndpoint(t *testing.T) {
	ts := newTestServer()
	defer ts.Close()

	resp, err := http.Get(ts.URL + "/health")
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("expected 200, got %d", resp.StatusCode)
	}

	resp, err = http.Get(ts.URL + "/api/v1/health")
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("expected 200, got %d", resp.StatusCode)
	}
}

// --------------------------------------------------------------------------
// Full Workflow: Bootstrap + Add Skill + Get Agent
// --------------------------------------------------------------------------

func TestIntegration_FullWorkflow(t *testing.T) {
	ts := newTestServer()
	defer ts.Close()

	// 1. Bootstrap with initial config
	bootstrapReq := BootstrapRequest{
		AgentName: "rust-developer",
		Prompt:    "You are a Rust developer.",
		Skills: []skills.SkillDef{
			{Name: "tdd", Description: "TDD", Content: "# TDD workflow"},
		},
	}
	body, _ := json.Marshal(bootstrapReq)
	resp, _ := http.Post(ts.URL+"/api/v1/agent/bootstrap", "application/json", bytes.NewBuffer(body))
	resp.Body.Close()

	// 2. Add an additional skill at runtime
	skillJSON := `{"name":"code-review","description":"Code review guidelines","content":"# Code Review\nCheck for bugs."}`
	resp, _ = http.Post(ts.URL+"/api/v1/skills", "application/json", bytes.NewBufferString(skillJSON))
	resp.Body.Close()

	// 3. Verify both skills present
	resp, _ = http.Get(ts.URL + "/api/v1/skills")
	var skillsMap map[string]*skills.SkillDef
	json.NewDecoder(resp.Body).Decode(&skillsMap)
	resp.Body.Close()
	if len(skillsMap) != 2 {
		t.Fatalf("expected 2 skills, got %d: %v", len(skillsMap), skillsMap)
	}

	// 4. Get agent (should still be set from bootstrap)
	resp, _ = http.Get(ts.URL + "/api/v1/agent")
	agentBody, _ := io.ReadAll(resp.Body)
	resp.Body.Close()
	var agentResp struct {
		Name   string `json:"name"`
		Prompt string `json:"prompt"`
	}
	json.Unmarshal(agentBody, &agentResp)
	if agentResp.Name != "rust-developer" {
		t.Fatalf("expected rust-developer, got: %s", agentResp.Name)
	}

	// 5. Remove a skill
	req, _ := http.NewRequest("DELETE", ts.URL+"/api/v1/skills/tdd", nil)
	resp, _ = http.DefaultClient.Do(req)
	resp.Body.Close()

	// 6. Verify only one skill remains
	resp, _ = http.Get(ts.URL + "/api/v1/skills")
	var remainingSkills map[string]*skills.SkillDef
	json.NewDecoder(resp.Body).Decode(&remainingSkills)
	resp.Body.Close()
	if len(remainingSkills) != 1 {
		t.Fatalf("expected 1 skill after removal, got %d", len(remainingSkills))
	}
	if _, ok := remainingSkills["code-review"]; !ok {
		t.Fatal("expected 'code-review' to remain")
	}
}
