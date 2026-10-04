# SDK FFI Bindings

The Nanosandbox SDK exposes C-compatible FFI bindings that allow other programming languages to create and manage sandboxes.

## Architecture

```
Language SDK (Python / Node / Go / ...)
    |
    | calls extern "C" functions
    v
libnanosandbox_sdk.so / .dylib / .dll
    |
    | Rust sandbox crate (this repo)
    | wraps nanosandbox runtime as dependency
    v
nanosandbox runtime (rlib)
    |
    | libkrun FFI (embedded as rlib)
    v
Hardware Virtualization (KVM / HVF / WHPX)
```

## Building the Shared Library

```bash
# Build release shared library with FFI bindings
cargo build --release -p sandbox --features ffi

# Output:
#   target/release/libsandbox.so      (Linux)
#   target/release/libsandbox.dylib   (macOS)
#   target/release/sandbox.dll        (Windows)
```

### Prerequisites

- Rust 1.70+
- libkrunfw installed (see runtime repo for installation)
- On macOS: Hypervisor.framework entitlement (ad-hoc codesigning)

## API Reference

### Sandbox Lifecycle

| Function | Signature | Description |
|----------|-----------|-------------|
| `sandbox_create` | `(config_json: *const c_char) -> *mut CSandbox` | Create sandbox from JSON config. Returns opaque handle or NULL. |
| `sandbox_start` | `(sandbox: *mut CSandbox) -> i32` | Start the sandbox VM. Returns 0 on success. |
| `sandbox_stop` | `(sandbox: *mut CSandbox) -> i32` | Stop the sandbox VM. Returns 0 on success. |
| `sandbox_destroy` | `(sandbox: *mut CSandbox) -> i32` | Destroy sandbox and free resources. Consumes the handle. |
| `sandbox_free` | `(sandbox: *mut CSandbox)` | Free handle without destroying the VM. |
| `sandbox_status` | `(sandbox: *const CSandbox) -> *mut c_char` | Get status string ("running", "stopped", etc.). |

### Command Execution

| Function | Signature | Description |
|----------|-----------|-------------|
| `sandbox_exec` | `(sandbox, cmd, args_json) -> *mut c_char` | Execute command, return ExecResult JSON. |
| `sandbox_exec_stream` | `(sandbox, cmd, args_json, callback, user_data) -> i32` | Execute with streaming output via callback. |

### Image Management

| Function | Signature | Description |
|----------|-----------|-------------|
| `image_pull` | `(image, callback, user_data) -> i32` | Pull OCI image to local cache. |
| `image_list` | `() -> *mut c_char` | List cached images as JSON array. |
| `image_exists` | `(image) -> bool` | Check if image is cached. |

### Utility

| Function | Signature | Description |
|----------|-----------|-------------|
| `free_string` | `(s: *mut c_char)` | Free a string returned by any FFI function. |
| `last_error` | `() -> *mut c_char` | Get last error message for current thread. |
| `version` | `() -> *const c_char` | Get SDK version (static, do not free). |
| `validate_runtime` | `() -> *mut c_char` | Check runtime prerequisites, return JSON. |

## Memory Management

- All functions returning `*mut c_char` allocate heap memory. The caller **must** free it with `free_string()`.
- The `version()` function returns a static string that must NOT be freed.
- `CSandbox` handles must be freed with either `sandbox_destroy()` (stops VM + frees) or `sandbox_free()` (frees handle only).
- Passing NULL to `free_string()` or `sandbox_free()` is safe.

## Error Handling

- Functions returning pointers return NULL on error.
- Functions returning `i32` return -1 on error, 0 on success.
- Call `last_error()` after a failure to get the error message (thread-local).

## Configuration JSON Schema

The `sandbox_create` function accepts a JSON object:

```json
{
  "name": "my-sandbox",
  "image": "python:3.12-slim",
  "cpus": 2,
  "memory_mb": 1024,
  "mounts": [
    {
      "mount_type": "virtiofs",
      "source": "/host/path",
      "target": "/vm/path"
    }
  ],
  "env": {
    "KEY": "value"
  },
  "network": {
    "mode": "user",
    "scope": "any"
  },
  "workdir": "/workspace",
  "timeout_secs": 300
}
```

## ExecResult JSON

The `sandbox_exec` function returns:

```json
{
  "exit_code": 0,
  "stdout": "Hello, world!\n",
  "stderr": "",
  "duration_ms": 42
}
```

## Example: Python (ctypes)

```python
import ctypes
import json

lib = ctypes.cdll.LoadLibrary("./target/release/libsandbox.dylib")

# Set return types
lib.sandbox_create.restype = ctypes.c_void_p
lib.sandbox_exec.restype = ctypes.c_char_p
lib.sandbox_status.restype = ctypes.c_char_p
lib.last_error.restype = ctypes.c_char_p
lib.version.restype = ctypes.c_char_p

config = json.dumps({
    "name": "test",
    "image": "alpine:latest",
    "cpus": 1,
    "memory_mb": 256,
})

sb = lib.sandbox_create(config.encode())
if not sb:
    print("Error:", lib.last_error().decode())
    exit(1)

lib.sandbox_start(sb)
result = lib.sandbox_exec(sb, b"echo", b'["hello"]')
print(json.loads(result))
lib.sandbox_destroy(sb)
```

## Example: Go (cgo)

```go
/*
#cgo LDFLAGS: -L./target/release -lsandbox
#include <stdlib.h>

extern void* sandbox_create(const char* config_json);
extern int sandbox_start(void* sandbox);
extern char* sandbox_exec(void* sandbox, const char* cmd, const char* args_json);
extern int sandbox_destroy(void* sandbox);
extern void free_string(char* s);
*/
import "C"
import (
    "encoding/json"
    "fmt"
    "unsafe"
)

func main() {
    config, _ := json.Marshal(map[string]any{
        "name": "test", "image": "alpine:latest",
        "cpus": 1, "memory_mb": 256,
    })

    cConfig := C.CString(string(config))
    defer C.free(unsafe.Pointer(cConfig))

    sb := C.sandbox_create(cConfig)
    C.sandbox_start(sb)

    cCmd := C.CString("echo")
    cArgs := C.CString(`["hello"]`)
    result := C.sandbox_exec(sb, cCmd, cArgs)
    fmt.Println(C.GoString(result))
    C.free_string(result)

    C.sandbox_destroy(sb)
}
```

## Threading

- All FFI functions are thread-safe.
- An internal tokio runtime handles async operations.
- `last_error()` is thread-local — each thread has its own error state.
- Create and destroy sandbox handles from any thread.

## Platform Notes

- **macOS**: Binary must be codesigned with `com.apple.security.hypervisor` entitlement.
- **Linux**: Requires KVM access (`/dev/kvm`).
- **Windows**: Requires Containers feature enabled (experimental).
