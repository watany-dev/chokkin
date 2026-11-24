// Append to src/manifest/python_range.rs temporarily (not committed), then
//   cargo test --release --lib scratch_dump -- --ignored
// It reads /tmp/diff/cases.tsv (requires-python TAB marker) and writes
// /tmp/diff/rust.out: `true` / `false` from marker_always_holds, `NOREQ`
// when requires-python is not read, `NOMARK` when the marker is not read.

#[cfg(test)]
mod scratch_diff {
    #[test]
    #[ignore]
    fn scratch_dump() {
        let input = std::fs::read_to_string("/tmp/diff/cases.tsv").unwrap();
        let mut out = String::new();
        for line in input.lines() {
            let (req, marker) = line.split_once('\t').unwrap();
            let r = match super::requires_python_range(req) {
                None => "NOREQ".to_owned(),
                Some(s) => match super::marker_range(marker) {
                    None => "NOMARK".to_owned(),
                    Some(_) => super::marker_always_holds(marker, &s).to_string(),
                },
            };
            out.push_str(&r);
            out.push('\n');
        }
        std::fs::write("/tmp/diff/rust.out", out).unwrap();
    }
}
