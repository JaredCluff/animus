/// Simple glob matching for paths.
/// Supports: `**/suffix` (match filename anywhere), `prefix/**` (match directory prefix),
/// `prefix*suffix` (simple wildcard), and exact match.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    if let Some(suffix) = pattern.strip_prefix("**/") {
        path.ends_with(suffix) || path.contains(&format!("/{suffix}"))
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
