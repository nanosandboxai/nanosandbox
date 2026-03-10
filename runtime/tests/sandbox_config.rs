//! Integration tests for sandbox.yml config file support.

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
