//! Copies of the root-relative path normalizers (sync-checked).

/// A path the user typed, in the `/`-separated form discovered paths use.
/// Used for both `entry` and `--trace` targets.
pub fn normalize_user_path(path: &str) -> String {
    let path = path.replace('\\', "/");
    let rest = path.trim_start_matches('/');
    let mut out = path[..path.len() - rest.len()].to_owned();
    for part in rest.split('/').filter(|part| !matches!(*part, "" | ".")) {
        if !out.is_empty() && !out.ends_with('/') {
            out.push('/');
        }
        out.push_str(part);
    }
    out
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
