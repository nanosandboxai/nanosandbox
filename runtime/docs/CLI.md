# Nanosandbox CLI Reference

`nanosb` is the command-line interface for managing VM-based sandboxes.

## Installation

```bash
# Build from source
cargo build --release --features cli

# Install globally
cargo install --path . --features cli
```

## Global Options

| Option | Description |
|--------|-------------|
| `--format <FORMAT>` | Output format: `text` (default) or `json` |
| `-v, --verbose` | Enable verbose output |
| `-h, --help` | Print help information |
| `-V, --version` | Print version information |

## Commands

### pull

Pull an image from a registry.

```bash
nanosb pull <IMAGE>
```

**Arguments:**
- `IMAGE` - Image reference (e.g., `alpine:3.19`, `ghcr.io/user/image:tag`)

**Examples:**

```bash
# Pull alpine image
nanosb pull alpine:3.19

# Pull from GitHub Container Registry
nanosb pull ghcr.io/devdone-labs/my-image:latest

# Pull with JSON output
nanosb --format json pull python:3.12-slim
```

**Output:**
```
✓ Pulled alpine:3.19 (1 layers, 7.8 MB)
```

---

### images

List cached images.

```bash
nanosb images
```

**Examples:**

```bash
# List all cached images
nanosb images

# List as JSON
nanosb --format json images
```

**Output:**
```
REPOSITORY       TAG          SIZE        PULLED
library/alpine   3.19         7.8 MB      2 hours ago
library/python   3.12-slim    125.3 MB    1 day ago
```

---

### run

Run a command in a new sandbox.

```bash
nanosb run [OPTIONS] <IMAGE> [COMMAND]...
```

**Arguments:**
- `IMAGE` - Image to use
- `COMMAND` - Command and arguments to run

**Options:**
| Option | Default | Description |
|--------|---------|-------------|
| `--name <NAME>` | auto-generated | Name for the sandbox |
| `--cpus <CPUS>` | 2 | CPU cores to allocate |
| `--memory <MEMORY>` | 4096 | Memory in MB |

**Examples:**

```bash
# Run a simple command
nanosb run alpine echo "Hello, World!"

# Run Python with custom resources
nanosb run --cpus 4 --memory 8192 python:3.12 python -c "print('Hello!')"

# Start an interactive sandbox (no command)
nanosb run --name my-sandbox alpine

# Run with JSON output
nanosb --format json run alpine ls -la
```

**Output (with command):**
```
Hello, World!
```

**Output (without command):**
```
✓ Sandbox abc123def456 started
Run commands with: nanosb exec abc123def456 <command>
```

---

### exec

Execute a command in a running sandbox.

```bash
nanosb exec <SANDBOX> <COMMAND>...
```

**Arguments:**
- `SANDBOX` - Sandbox ID or name (prefix matching supported)
- `COMMAND` - Command and arguments to run

**Examples:**

```bash
# Execute in a running sandbox
nanosb exec abc123 ls -la

# Execute using name prefix
nanosb exec my-sandbox python --version
```

> **Note:** The `exec` command requires an active runtime connection. For ephemeral execution, use `nanosb run` instead.

---

### ps

List sandboxes.

```bash
nanosb ps [OPTIONS]
```

**Options:**
| Option | Description |
|--------|-------------|
| `-a, --all` | Show all sandboxes (including stopped) |

**Examples:**

```bash
# List running sandboxes
nanosb ps

# List all sandboxes
nanosb ps -a

# List as JSON
nanosb --format json ps -a
```

**Output:**
```
ID              IMAGE           STATUS      CREATED
abc123def456    alpine:3.19     Running     5 minutes ago
def456abc123    python:3.12     Stopped     1 hour ago
```

---

### stop

Stop a running sandbox.

```bash
nanosb stop <SANDBOX>
```

**Arguments:**
- `SANDBOX` - Sandbox ID or name (prefix matching supported)

**Examples:**

```bash
# Stop by ID prefix
nanosb stop abc123

# Stop by name
nanosb stop my-sandbox
```

**Output:**
```
✓ Stopped abc123def456
```

---

### rm

Remove a sandbox.

```bash
nanosb rm [OPTIONS] <SANDBOX>
```

**Arguments:**
- `SANDBOX` - Sandbox ID or name (prefix matching supported)

**Options:**
| Option | Description |
|--------|-------------|
| `-f, --force` | Force removal (stop if running) |

**Examples:**

```bash
# Remove a stopped sandbox
nanosb rm abc123

# Force remove a running sandbox
nanosb rm -f my-sandbox
```

**Output:**
```
✓ Removed abc123def456
```

---

## JSON Output

All commands support JSON output with `--format json`:

```bash
nanosb --format json images
```

```json
[
  {
    "reference": {
      "registry": "docker.io",
      "repository": "library/alpine",
      "tag": "3.19"
    },
    "size": 8175432,
    "pulled_at": "2024-01-15T10:30:00Z",
    "layers": ["sha256:abc123..."]
  }
]
```

---

## Exit Codes

| Code | Description |
|------|-------------|
| 0 | Success |
| 1 | General error |
| Command exit code | For `run` command, returns the executed command's exit code |

---

## Environment Variables

| Variable | Description |
|----------|-------------|
| `NANOSANDBOX_CACHE_DIR` | Custom cache directory (default: `~/.nanosandbox`) |
| `DOCKER_CONFIG` | Docker config directory for registry credentials |

---

## Examples

### Quick Start

```bash
# Pull an image
nanosb pull alpine:3.19

# Run a command
nanosb run alpine echo "Hello from sandbox!"

# Run Python code
nanosb run python:3.12 python -c "print(2 + 2)"
```

### Development Workflow

```bash
# Start a sandbox
nanosb run --name dev --cpus 4 --memory 8192 node:20

# List running sandboxes
nanosb ps

# Stop when done
nanosb stop dev

# Clean up
nanosb rm dev
```

### Scripting with JSON

```bash
# Get image list as JSON and process with jq
nanosb --format json images | jq '.[].reference.repository'

# Check if sandbox is running
status=$(nanosb --format json ps | jq -r '.[0].status')
```
