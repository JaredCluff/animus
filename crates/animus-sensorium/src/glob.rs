/// Simple glob matching for paths.
/// Supports: `**/suffix` (match filename anywhere), `prefix/**` (match directory prefix),
/// `prefix*suffix` (simple wildcard), and exact match.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    if let Some(suffix) = pattern.strip_prefix("**/") {
        // Match at a path component boundary: the suffix must be preceded by '/'
        // or be the entire path. Using `ends_with(suffix)` alone is wrong because
        // it would match "not_etc/passwd" for pattern "**/etc/passwd".
        path == suffix || path.ends_with(&format!("/{suffix}"))
    } else if let Some(prefix) = pattern.strip_suffix("/**") {
        path.starts_with(prefix)
    } else if pattern.contains('*') {
        let parts: Vec<&str> = pattern.split('*').collect();
        if parts.len() == 2 {
            path.starts_with(parts[0]) && path.ends_with(parts[1])
        } else {
            path == pattern
        }
    } else {
        path == pattern
    }
}
