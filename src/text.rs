//! Small text helpers shared across the app.

use std::path::{Path, PathBuf};

/// At most `n` characters on one line; longer text ends in an ellipsis.
pub fn clip(s: &str, n: usize) -> String {
    let s = s.replace('\n', " ");
    if s.chars().count() > n { format!("{}…", s.chars().take(n).collect::<String>()) } else { s }
}

/// "9s", "2m 10s".
pub fn human_secs(s: u64) -> String {
    if s < 60 { format!("{s}s") } else { format!("{}m {:02}s", s / 60, s % 60) }
}

/// Last path component ("/x/y/README.md" -> "README.md").
pub fn base_name(path: &str) -> &str {
    path.trim().trim_end_matches('/').rsplit('/').next().unwrap_or(path)
}

/// Expand a leading `~/` and make the path absolute against `root`.
pub fn resolve_path(root: &Path, p: &str) -> PathBuf {
    let expanded = match p.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(p),
    };
    if expanded.is_absolute() { expanded } else { root.join(expanded) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(human_secs(9), "9s");
        assert_eq!(human_secs(130), "2m 10s");
        assert_eq!(clip("a\nbcdef", 4), "a bc…");
        assert_eq!(base_name("/x/y/README.md"), "README.md");
        assert_eq!(base_name("/x/y/"), "y");
        assert_eq!(resolve_path(Path::new("/w"), "a/b"), PathBuf::from("/w/a/b"));
    }
}
