//! Integration tests for sandbox.yml config file support.

#[allow(unused_imports)]
use nanosandbox::config::file::{
    expand_env_vars, find_sandbox_file, load_sandbox_file, parse_sandbox_file,
    resolve_sandbox_configs, validate_name,
};

#[test]
fn test_full_config_roundtrip() {
    let yaml = r#"
defaults:
  image: nanosb-claude:latest
  cpus: 2
  memory: 4096
  workdir: /workspace
  timeout: 600
  network:
    enabled: true
    mode: tsi
    scope: any
  mcp:
    github:
      command: npx
      args: ["-y", "@modelcontextprotocol/server-github"]

sandboxes:
  claude:
    name: claude-dev
    mcp:
      filesystem:
        command: npx
        args: ["-y", "@modelcontextprotocol/server-filesystem"]
  codex:
    image: nanosb-codex:latest
    cpus: 4
    mcp:
      github:
        command: uvx
        args: ["custom-github-server"]
"#;

    let file = parse_sandbox_file(yaml).unwrap();
    let configs =
        resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();

    assert_eq!(configs.len(), 2);

    let claude = configs.iter().find(|(k, _)| k == "claude").unwrap();
    assert_eq!(claude.1.name, "claude-dev");
    assert_eq!(claude.1.image, "nanosb-claude:latest"); // inherited
    assert_eq!(claude.1.cpus, 2); // inherited
    assert_eq!(claude.1.mcp_servers.len(), 2); // github inherited + filesystem
    assert_eq!(claude.1.mcp_servers["github"].command, "npx"); // inherited

    let codex = configs.iter().find(|(k, _)| k == "codex").unwrap();
    assert_eq!(codex.1.name, "codex"); // key as name
    assert_eq!(codex.1.image, "nanosb-codex:latest"); // overridden
    assert_eq!(codex.1.cpus, 4); // overridden
    assert_eq!(codex.1.memory_mb, 4096); // inherited
    assert_eq!(codex.1.mcp_servers["github"].command, "uvx"); // overridden
}

#[test]
fn test_multi_repo_config() {
    let yaml = r#"
defaults:
  image: agent:latest

sandboxes:
  frontend:
    project:
      path: /home/user/repos/frontend
      branch: nanosb/feature
  backend:
    project:
      path: /home/user/repos/backend
"#;

    let file = parse_sandbox_file(yaml).unwrap();
    let configs =
        resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();

    let frontend = configs.iter().find(|(k, _)| k == "frontend").unwrap();
    let backend = configs.iter().find(|(k, _)| k == "backend").unwrap();

    assert_eq!(
        frontend.1.project.as_ref().unwrap().path.to_str().unwrap(),
        "/home/user/repos/frontend"
    );
    assert_eq!(
        backend.1.project.as_ref().unwrap().path.to_str().unwrap(),
        "/home/user/repos/backend"
    );
}

#[test]
fn test_yaml_agent_field() {
    let yaml = r#"
defaults:
  image: base:latest
  agent: python-developer
sandboxes:
  test: {}
"#;
    let file = parse_sandbox_file(yaml).unwrap();
    let configs = resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
    assert_eq!(configs.len(), 1);
    assert_eq!(configs[0].1.agent.as_deref(), Some("python-developer"));
}

#[test]
fn test_yaml_skills_field() {
    let yaml = r#"
defaults:
  image: base:latest
sandboxes:
  test:
    skills:
      - tdd
      - git-workflow
"#;
    let file = parse_sandbox_file(yaml).unwrap();
    let configs = resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
    assert_eq!(configs[0].1.skills, vec!["tdd", "git-workflow"]);
}

#[test]
fn test_yaml_agent_per_sandbox_overrides_defaults() {
    let yaml = r#"
defaults:
  image: base:latest
  agent: default-agent
  skills:
    - default-skill
sandboxes:
  custom:
    agent: custom-agent
    skills:
      - custom-skill
"#;
    let file = parse_sandbox_file(yaml).unwrap();
    let configs = resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
    assert_eq!(configs[0].1.agent.as_deref(), Some("custom-agent"));
    assert_eq!(configs[0].1.skills, vec!["custom-skill"]);
}

#[test]
fn test_yaml_agent_inherits_from_defaults() {
    let yaml = r#"
defaults:
  image: base:latest
  agent: default-agent
  skills:
    - default-skill
sandboxes:
  basic: {}
"#;
    let file = parse_sandbox_file(yaml).unwrap();
    let configs = resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
    assert_eq!(configs[0].1.agent.as_deref(), Some("default-agent"));
    assert_eq!(configs[0].1.skills, vec!["default-skill"]);
}

#[test]
fn test_yaml_no_agent_no_skills() {
    let yaml = r#"
defaults:
  image: base:latest
sandboxes:
  simple: {}
"#;
    let file = parse_sandbox_file(yaml).unwrap();
    let configs = resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
    assert!(configs[0].1.agent.is_none());
    assert!(configs[0].1.skills.is_empty());
}

#[test]
fn test_yaml_agent_with_mcp_full_config() {
    let yaml = r#"
defaults:
  image: base:latest
  agent: python-developer
  skills:
    - tdd
    - git-workflow
  mcp:
    github:
      command: npx
      args: ["-y", "@modelcontextprotocol/server-github"]
sandboxes:
  dev:
    mcp:
      filesystem:
        command: npx
        args: ["-y", "@modelcontextprotocol/server-filesystem"]
"#;
    let file = parse_sandbox_file(yaml).unwrap();
    let configs = resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
    let config = &configs[0].1;

    assert_eq!(config.agent.as_deref(), Some("python-developer"));
    assert_eq!(config.skills, vec!["tdd", "git-workflow"]);
    assert_eq!(config.mcp_servers.len(), 2);
    assert!(config.mcp_servers.contains_key("github"));
    assert!(config.mcp_servers.contains_key("filesystem"));
}

#[test]
fn test_yaml_auto_mode_default_false() {
    let yaml = r#"
sandboxes:
  test:
    image: alpine:latest
"#;
    let file = parse_sandbox_file(yaml).unwrap();
    let configs = resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
    assert!(!configs[0].1.auto_mode);
}

#[test]
fn test_yaml_auto_mode_in_defaults() {
    let yaml = r#"
defaults:
  auto_mode: true
sandboxes:
  test:
    image: alpine:latest
"#;
    let file = parse_sandbox_file(yaml).unwrap();
    let configs = resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
    assert!(configs[0].1.auto_mode);
}

#[test]
fn test_yaml_auto_mode_per_sandbox_override() {
    let yaml = r#"
defaults:
  auto_mode: true
sandboxes:
  auto:
    image: alpine:latest
  manual:
    image: alpine:latest
    auto_mode: false
"#;
    let file = parse_sandbox_file(yaml).unwrap();
    let configs = resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
    // "auto" inherits defaults auto_mode: true
    let auto_cfg = configs.iter().find(|(k, _)| k == "auto").unwrap();
    assert!(auto_cfg.1.auto_mode);
    // "manual" overrides to false
    let manual_cfg = configs.iter().find(|(k, _)| k == "manual").unwrap();
    assert!(!manual_cfg.1.auto_mode);
}

#[test]
fn test_yaml_env_file_field() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("secrets.env"),
        "API_KEY=sk-test-123\nDB_URL=postgres://localhost\n",
    )
    .unwrap();

    let yaml = r#"
defaults:
  image: base:latest
  env_file: secrets.env
sandboxes:
  test: {}
"#;
    let file = parse_sandbox_file(yaml).unwrap();
    let configs = resolve_sandbox_configs(&file, dir.path()).unwrap();
    assert_eq!(configs[0].1.env["API_KEY"], "sk-test-123");
    assert_eq!(configs[0].1.env["DB_URL"], "postgres://localhost");
}

#[test]
fn test_yaml_env_file_per_sandbox_overrides_defaults() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("default.env"), "TOKEN=default\n").unwrap();
    std::fs::write(dir.path().join("custom.env"), "TOKEN=custom\nEXTRA=yes\n").unwrap();

    let yaml = r#"
defaults:
  image: base:latest
  env_file: default.env
sandboxes:
  a: {}
  b:
    env_file: custom.env
"#;
    let file = parse_sandbox_file(yaml).unwrap();
    let configs = resolve_sandbox_configs(&file, dir.path()).unwrap();

    let a = configs.iter().find(|(k, _)| k == "a").unwrap();
    assert_eq!(a.1.env["TOKEN"], "default");
    assert!(!a.1.env.contains_key("EXTRA"));

    let b = configs.iter().find(|(k, _)| k == "b").unwrap();
    assert_eq!(b.1.env["TOKEN"], "custom");
    assert_eq!(b.1.env["EXTRA"], "yes");
}

#[test]
fn test_yaml_inline_env_overrides_env_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("base.env"), "KEY=from_file\n").unwrap();

    let yaml = r#"
sandboxes:
  test:
    image: alpine:latest
    env_file: base.env
    env:
      KEY: from_inline
"#;
    let file = parse_sandbox_file(yaml).unwrap();
    let configs = resolve_sandbox_configs(&file, dir.path()).unwrap();
    // Inline env should override env_file
    assert_eq!(configs[0].1.env["KEY"], "from_inline");
}
