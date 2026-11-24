//! Copies of the root-relative path normalizers (sync-checked).

/// Normalize a user-supplied path for graph lookup.
pub fn normalize_trace_path(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches("./").to_owned()
}

/// `\` to `/`, then strip every leading `./` (`././src/app.py` is `src/app.py`).
pub fn normalize_entry_path(path: &str) -> String {
    let path = path.replace('\\', "/");
    let mut rest = path.as_str();
    while let Some(stripped) = rest.strip_prefix("./") {
        rest = stripped.trim_start_matches('/');
    }
    rest.to_owned()
}

/// `./src/`, `src` and `src/` all name `src`; `.` names the root.
pub fn normalize_dir(entry: &str) -> String {
    let entry = entry.replace('\\', "/");
    let mut entry = entry.as_str();
    while let Some(rest) = entry.strip_prefix("./") {
        entry = rest;
    }
    let entry = entry.trim_end_matches('/');
    if entry == "." {
        String::new()
    } else {
        entry.to_owned()
    }
}

/// Root-relative, `/`-separated, no leading `./` or `/`, no trailing `/`;
/// the root itself is `""`.
pub fn normalize_path(raw: &str) -> String {
    let unified = raw.trim().replace('\\', "/");
    let mut path = unified.as_str();
    while let Some(rest) = path.strip_prefix("./") {
        path = rest;
    }
    let path = path.trim_matches('/');
    if path == "." {
        String::new()
    } else {
        path.to_owned()
    }
}

/// `./packages/*/` and `packages\*` both become `packages/*`: a `.` or empty
/// segment left in would never match a walked path.
pub fn normalize_relative_path(path: &str) -> String {
    path.split(['/', '\\'])
        .filter(|part| !matches!(*part, "" | "."))
        .collect::<Vec<_>>()
        .join("/")
}
