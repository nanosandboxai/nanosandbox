//! C FFI bindings for the Nanosandbox SDK.
//!
//! Exposes sandbox lifecycle and image management operations through a C-compatible
//! ABI so that other language SDKs (Python, Node, Go, etc.) can bind to them.
//!
//! # Memory Management
//!
//! - Strings returned by FFI functions are heap-allocated C strings.
//!   The caller **must** free them with [`free_string`].
//! - Opaque [`CSandbox`] handles must be freed with [`sandbox_free`].
//! - Null pointers indicate errors; call [`last_error`] for details.
//!
//! # Threading
//!
//! All functions are safe to call from any thread. An internal tokio runtime
//! handles async operations.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::ptr;
use std::sync::Mutex;

use runtime::{ImageManager, Sandbox, SandboxConfig};

// ── Thread-local error storage ──────────────────────────────────────

thread_local! {
    static LAST_ERROR: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

fn set_error(msg: String) {
    LAST_ERROR.with(|e| *e.borrow_mut() = Some(msg));
}

fn to_cstring(s: &str) -> *mut c_char {
    CString::new(s).map(CString::into_raw).unwrap_or(ptr::null_mut())
}

fn from_cstr(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(ptr) }.to_str().ok().map(String::from)
}

/// Get a handle to the shared tokio runtime for blocking on async operations.
fn tokio_rt() -> &'static tokio::runtime::Runtime {
    use std::sync::OnceLock;
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to create tokio runtime")
    })
}

// ── Opaque handle ───────────────────────────────────────────────────

/// Opaque sandbox handle exposed to FFI consumers.
///
/// Wraps a `runtime::Sandbox` behind a mutex for thread-safe access.
pub struct CSandbox {
    inner: Mutex<Sandbox>,
}

// ── Error Retrieval ─────────────────────────────────────────────────

/// Retrieve the last error message for the calling thread.
///
/// Returns a heap-allocated C string that the caller must free with [`free_string`],
/// or `NULL` if no error has been recorded.
#[no_mangle]
pub extern "C" fn last_error() -> *mut c_char {
    LAST_ERROR.with(|e| {
        e.borrow()
            .as_deref()
            .map(to_cstring)
            .unwrap_or(ptr::null_mut())
    })
}

// ── Sandbox Lifecycle ───────────────────────────────────────────────

/// Create a new sandbox from a JSON configuration string.
///
/// `config_json` must be a valid JSON object matching the `SandboxConfig` schema.
/// Returns an opaque [`CSandbox`] handle, or `NULL` on error.
/// The caller must eventually free the handle with [`sandbox_free`].
#[no_mangle]
pub extern "C" fn sandbox_create(config_json: *const c_char) -> *mut CSandbox {
    let json = match from_cstr(config_json) {
        Some(s) => s,
        None => {
            set_error("config_json is null or invalid UTF-8".into());
            return ptr::null_mut();
        }
    };

    let config: SandboxConfig = match serde_json::from_str(&json) {
        Ok(c) => c,
        Err(e) => {
            set_error(format!("failed to parse config JSON: {e}"));
            return ptr::null_mut();
        }
    };

    match tokio_rt().block_on(Sandbox::create(config)) {
        Ok(sandbox) => Box::into_raw(Box::new(CSandbox {
            inner: Mutex::new(sandbox),
        })),
        Err(e) => {
            set_error(format!("sandbox_create failed: {e}"));
            ptr::null_mut()
        }
    }
}

/// Start a sandbox.
///
/// Returns 0 on success, -1 on error (call [`last_error`] for details).
#[no_mangle]
pub extern "C" fn sandbox_start(sandbox: *mut CSandbox) -> i32 {
    let sb = match unsafe { sandbox.as_ref() } {
        Some(s) => s,
        None => {
            set_error("sandbox pointer is null".into());
            return -1;
        }
    };
    let mut guard = sb.inner.lock().unwrap();
    match tokio_rt().block_on(guard.start()) {
        Ok(()) => 0,
        Err(e) => {
            set_error(format!("sandbox_start failed: {e}"));
            -1
        }
    }
}

/// Stop a running sandbox.
///
/// Returns 0 on success, -1 on error.
#[no_mangle]
pub extern "C" fn sandbox_stop(sandbox: *mut CSandbox) -> i32 {
    let sb = match unsafe { sandbox.as_ref() } {
        Some(s) => s,
        None => {
            set_error("sandbox pointer is null".into());
            return -1;
        }
    };
    let mut guard = sb.inner.lock().unwrap();
    match tokio_rt().block_on(guard.stop()) {
        Ok(()) => 0,
        Err(e) => {
            set_error(format!("sandbox_stop failed: {e}"));
            -1
        }
    }
}

/// Destroy a sandbox, releasing all resources.
///
/// The [`CSandbox`] handle is consumed and must not be used after this call.
/// Returns 0 on success, -1 on error.
#[no_mangle]
pub extern "C" fn sandbox_destroy(sandbox: *mut CSandbox) -> i32 {
    if sandbox.is_null() {
        set_error("sandbox pointer is null".into());
        return -1;
    }
    let sb = unsafe { Box::from_raw(sandbox) };
    let inner = sb.inner.into_inner().unwrap();
    match tokio_rt().block_on(inner.destroy()) {
        Ok(()) => 0,
        Err(e) => {
            set_error(format!("sandbox_destroy failed: {e}"));
            -1
        }
    }
}

/// Free a sandbox handle without destroying the underlying VM.
///
/// Use this only if you need to release the handle without stopping the sandbox.
/// For normal cleanup, use [`sandbox_destroy`] instead.
#[no_mangle]
pub extern "C" fn sandbox_free(sandbox: *mut CSandbox) {
    if !sandbox.is_null() {
        unsafe { drop(Box::from_raw(sandbox)) };
    }
}

/// Get the current status of a sandbox as a JSON string.
///
/// Returns a heap-allocated C string (free with [`free_string`]), or `NULL` on error.
/// Example: `"running"`, `"stopped"`, `"creating"`.
#[no_mangle]
pub extern "C" fn sandbox_status(sandbox: *const CSandbox) -> *mut c_char {
    let sb = match unsafe { sandbox.as_ref() } {
        Some(s) => s,
        None => {
            set_error("sandbox pointer is null".into());
            return ptr::null_mut();
        }
    };
    let guard = sb.inner.lock().unwrap();
    let status = format!("{:?}", guard.status()).to_lowercase();
    to_cstring(&status)
}

// ── Command Execution ───────────────────────────────────────────────

/// Execute a command inside the sandbox.
///
/// `cmd` is the command to run. `args_json` is a JSON array of string arguments
/// (e.g. `["--version"]`), or `NULL` for no arguments.
///
/// Returns a heap-allocated JSON string representing `ExecResult`
/// (free with [`free_string`]), or `NULL` on error.
#[no_mangle]
pub extern "C" fn sandbox_exec(
    sandbox: *mut CSandbox,
    cmd: *const c_char,
    args_json: *const c_char,
) -> *mut c_char {
    let sb = match unsafe { sandbox.as_ref() } {
        Some(s) => s,
        None => {
            set_error("sandbox pointer is null".into());
            return ptr::null_mut();
        }
    };

    let command = match from_cstr(cmd) {
        Some(s) => s,
        None => {
            set_error("cmd is null or invalid UTF-8".into());
            return ptr::null_mut();
        }
    };

    let args: Vec<String> = match from_cstr(args_json) {
        Some(json) => match serde_json::from_str(&json) {
            Ok(a) => a,
            Err(e) => {
                set_error(format!("failed to parse args_json: {e}"));
                return ptr::null_mut();
            }
        },
        None => Vec::new(),
    };

    let args_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let guard = sb.inner.lock().unwrap();

    match tokio_rt().block_on(guard.exec(&command, &args_refs)) {
        Ok(result) => match serde_json::to_string(&result) {
            Ok(json) => to_cstring(&json),
            Err(e) => {
                set_error(format!("failed to serialize ExecResult: {e}"));
                ptr::null_mut()
            }
        },
        Err(e) => {
            set_error(format!("sandbox_exec failed: {e}"));
            ptr::null_mut()
        }
    }
}

/// Callback type for streaming exec output.
///
/// - `data`: UTF-8 output chunk
/// - `is_stderr`: `true` if the chunk is from stderr, `false` for stdout
/// - `user_data`: opaque pointer passed through from the caller
pub type ExecStreamCallback =
    extern "C" fn(data: *const c_char, is_stderr: bool, user_data: *mut std::ffi::c_void);

/// Execute a command with streaming output.
///
/// The `callback` is invoked for each output chunk. `user_data` is passed
/// through to the callback unchanged.
///
/// Returns the process exit code on success, or -1 on error.
#[no_mangle]
pub extern "C" fn sandbox_exec_stream(
    sandbox: *mut CSandbox,
    cmd: *const c_char,
    args_json: *const c_char,
    callback: ExecStreamCallback,
    user_data: *mut std::ffi::c_void,
) -> i32 {
    let sb = match unsafe { sandbox.as_ref() } {
        Some(s) => s,
        None => {
            set_error("sandbox pointer is null".into());
            return -1;
        }
    };

    let command = match from_cstr(cmd) {
        Some(s) => s,
        None => {
            set_error("cmd is null or invalid UTF-8".into());
            return -1;
        }
    };

    let args: Vec<String> = match from_cstr(args_json) {
        Some(json) => match serde_json::from_str(&json) {
            Ok(a) => a,
            Err(e) => {
                set_error(format!("failed to parse args_json: {e}"));
                return -1;
            }
        },
        None => Vec::new(),
    };

    let args_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let guard = sb.inner.lock().unwrap();

    // Safety: user_data is an opaque pointer managed by the caller.
    let ud = user_data as usize;
    let on_output = move |chunk: runtime::OutputChunk| {
        let is_stderr = chunk.stream == runtime::Stream::Stderr;
        if let Ok(cstr) = CString::new(chunk.data) {
            callback(cstr.as_ptr(), is_stderr, ud as *mut std::ffi::c_void);
        }
    };

    match tokio_rt().block_on(guard.exec_stream(&command, &args_refs, on_output)) {
        Ok(exit_code) => exit_code,
        Err(e) => {
            set_error(format!("sandbox_exec_stream failed: {e}"));
            -1
        }
    }
}

// ── Image Management ────────────────────────────────────────────────

/// Callback type for image pull progress.
///
/// - `downloaded`: bytes downloaded so far
/// - `total`: total bytes (0 if unknown)
/// - `user_data`: opaque pointer passed through from the caller
pub type ImagePullCallback =
    extern "C" fn(downloaded: u64, total: u64, user_data: *mut std::ffi::c_void);

/// Pull an OCI image to the local cache.
///
/// `image` is the image reference (e.g. `"python:3.12-slim"`).
/// `callback` is invoked with progress updates (may be called with a null function pointer
/// to skip progress reporting). `user_data` is passed through to the callback.
///
/// Returns 0 on success, -1 on error.
#[no_mangle]
pub extern "C" fn image_pull(
    image: *const c_char,
    _callback: ImagePullCallback,
    _user_data: *mut std::ffi::c_void,
) -> i32 {
    let image_ref = match from_cstr(image) {
        Some(s) => s,
        None => {
            set_error("image is null or invalid UTF-8".into());
            return -1;
        }
    };

    let manager = match ImageManager::with_default_cache() {
        Ok(m) => m,
        Err(e) => {
            set_error(format!("failed to create image manager: {e}"));
            return -1;
        }
    };

    // TODO: Wire progress callback into ImageManager::pull when progress API is available.
    match tokio_rt().block_on(manager.pull(&image_ref)) {
        Ok(_) => 0,
        Err(e) => {
            set_error(format!("image_pull failed: {e}"));
            -1
        }
    }
}

/// List cached images as a JSON array.
///
/// Returns a heap-allocated JSON string (free with [`free_string`]), or `NULL` on error.
#[no_mangle]
pub extern "C" fn image_list() -> *mut c_char {
    let manager = match ImageManager::with_default_cache() {
        Ok(m) => m,
        Err(e) => {
            set_error(format!("failed to create image manager: {e}"));
            return ptr::null_mut();
        }
    };

    match tokio_rt().block_on(manager.list()) {
        Ok(images) => match serde_json::to_string(&images) {
            Ok(json) => to_cstring(&json),
            Err(e) => {
                set_error(format!("failed to serialize image list: {e}"));
                ptr::null_mut()
            }
        },
        Err(e) => {
            set_error(format!("image_list failed: {e}"));
            ptr::null_mut()
        }
    }
}

/// Check whether an image exists in the local cache.
///
/// Returns `true` if the image is cached, `false` otherwise or on error.
#[no_mangle]
pub extern "C" fn image_exists(image: *const c_char) -> bool {
    let image_ref = match from_cstr(image) {
        Some(s) => s,
        None => return false,
    };

    let manager = match ImageManager::with_default_cache() {
        Ok(m) => m,
        Err(_) => return false,
    };

    tokio_rt()
        .block_on(manager.exists(&image_ref))
        .unwrap_or(false)
}

// ── Memory Management ───────────────────────────────────────────────

/// Free a string previously returned by an FFI function.
///
/// Passing `NULL` is safe and does nothing.
#[no_mangle]
pub extern "C" fn free_string(s: *mut c_char) {
    if !s.is_null() {
        unsafe { drop(CString::from_raw(s)) };
    }
}

// ── Runtime Info ────────────────────────────────────────────────────

/// Get the SDK version string.
///
/// Returns a static C string that must NOT be freed.
#[no_mangle]
pub extern "C" fn version() -> *const c_char {
    static VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "\0");
    VERSION.as_ptr() as *const c_char
}

/// Validate that runtime prerequisites are met.
///
/// Returns a heap-allocated JSON string with validation results
/// (free with [`free_string`]), or `NULL` on error.
///
/// Result format:
/// ```json
/// {
///   "ok": true,
///   "errors": [{"check": "...", "message": "...", "fix_hint": "..."}],
///   "warnings": ["..."]
/// }
/// ```
#[no_mangle]
pub extern "C" fn validate_runtime() -> *mut c_char {
    let result =
        tokio_rt().block_on(runtime::runtime::validate_runtime_prerequisites_detailed());

    let errors: Vec<serde_json::Value> = result
        .errors
        .iter()
        .map(|e| {
            serde_json::json!({
                "check": e.check,
                "message": e.message,
                "fix_hint": e.fix_hint,
            })
        })
        .collect();

    let json = serde_json::json!({
        "ok": result.is_ok(),
        "errors": errors,
        "warnings": result.warnings,
    });

    to_cstring(&json.to_string())
}
