/// Utility functions for host path handling across platforms.
///
/// On Unix, paths are used as-is. On Windows, forward slashes in paths
/// received from C callers are converted to backslashes so that the host
/// filesystem layer always sees native Windows paths.

/// Normalize a host filesystem path received from a C caller.
///
/// On Unix this is a no-op. On Windows it converts any forward slashes
/// to backslashes, which allows callers that pass mixed-separator paths
/// (e.g. `C:/Users/foo`) to work correctly with Win32 filesystem APIs.
#[cfg(unix)]
pub fn normalize_host_path(path: &str) -> String {
    path.to_string()
}

/// Normalize a host filesystem path received from a C caller.
///
/// On Windows, converts forward slashes to backslashes so that paths
/// like `C:/Users/foo` become `C:\Users\foo`. Paths that are already
/// using backslashes (including UNC paths `\\server\share` and long-path
/// prefixed paths `\\?\C:\...`) pass through unchanged.
#[cfg(target_os = "windows")]
pub fn normalize_host_path(path: &str) -> String {
    path.replace('/', "\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_no_slashes() {
        // A path without any forward slashes should remain unchanged on
        // all platforms.
        let input = if cfg!(unix) {
            "/home/user/rootfs"
        } else {
            "C:\\Users\\user\\rootfs"
        };
        assert_eq!(normalize_host_path(input), input);
    }

    #[cfg(unix)]
    #[test]
    fn test_normalize_unix_noop() {
        let input = "/home/user/rootfs";
        assert_eq!(normalize_host_path(input), input);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_normalize_forward_slashes() {
        assert_eq!(
            normalize_host_path("C:/Users/user/rootfs"),
            "C:\\Users\\user\\rootfs"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_normalize_mixed_slashes() {
        assert_eq!(
            normalize_host_path("C:\\Users/user\\rootfs"),
            "C:\\Users\\user\\rootfs"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_normalize_unc_path() {
        // A UNC path that already uses backslashes should be unchanged.
        let unc = "\\\\server\\share\\path";
        assert_eq!(normalize_host_path(unc), unc);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_normalize_long_path_prefix() {
        let long_path = "\\\\?\\C:\\very\\long\\path";
        assert_eq!(normalize_host_path(long_path), long_path);
    }
}
