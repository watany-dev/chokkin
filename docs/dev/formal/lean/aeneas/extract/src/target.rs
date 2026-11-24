//! Copy of the fingerprint part encoder (sync-checked).

/// A path or member id with `%` and `:` percent-encoded, so that `:` only
/// ever separates the parts of a target: member `a` with `b.py` and a
/// root-level `a:b.py` would otherwise share a fingerprint (#1108). Names
/// and dotted modules never hold `:`, and other paths keep their form.
pub fn target_part(raw: &str) -> String {
    if raw.contains(['%', ':']) {
        raw.replace('%', "%25").replace(':', "%3A")
    } else {
        raw.to_owned()
    }
}
