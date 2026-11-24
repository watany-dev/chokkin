//! Copies of the PEP 503 name normalizers (sync-checked).

/// Normalize a distribution name to lowercase hyphen form (PEP 503):
/// runs of `-`, `_`, and `.` collapse into a single `-`.
#[must_use]
pub fn normalize_distribution_name(name: &str) -> String {
    let mut normalized = String::with_capacity(name.len());
    let mut pending_separator = false;
    for ch in name.chars() {
        if matches!(ch, '-' | '_' | '.') {
            pending_separator = true;
            continue;
        }
        if pending_separator {
            normalized.push('-');
            pending_separator = false;
        }
        normalized.push(ch.to_ascii_lowercase());
    }
    if pending_separator {
        normalized.push('-');
    }
    normalized
}

/// `name` normalized, kept as is when it already is: uv writes names that
/// way, and a monorepo's member lockfiles hold millions of them.
pub fn normalized(name: String) -> String {
    let separator_or_upper = |byte: u8| matches!(byte, b'_' | b'.') || byte.is_ascii_uppercase();
    if name.bytes().any(separator_or_upper) || name.contains("--") {
        normalize_distribution_name(&name)
    } else {
        name
    }
}
