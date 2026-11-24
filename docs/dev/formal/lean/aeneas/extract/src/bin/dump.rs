//! Every string over a small alphabet up to a length, through each copied
//! string function, as `function<TAB>input<TAB>output` lines for
//! `lake exe check` (Charon/Aeneas have no model of `str`).

use chokkin_extract::{names, paths, target};

fn all_strings(alphabet: &[char], max_len: usize) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut layer = vec![String::new()];
    for _ in 0..max_len {
        let mut next = Vec::new();
        for s in &layer {
            for &c in alphabet {
                let mut t = s.clone();
                t.push(c);
                next.push(t);
            }
        }
        out.extend(next.iter().cloned());
        layer = next;
    }
    out
}

fn main() {
    let mut out = String::new();
    let mut emit = |name: &str, input: &str, output: &str| {
        out.push_str(name);
        out.push('\t');
        out.push_str(input);
        out.push('\t');
        out.push_str(output);
        out.push('\n');
    };
    for s in all_strings(&['a', '.', '/', '\\'], 6) {
        emit("normalize_user_path", &s, &paths::normalize_user_path(&s));
        emit("normalize_dir", &s, &paths::normalize_dir(&s));
        emit("normalize_path", &s, &paths::normalize_path(&s));
        emit(
            "normalize_relative_path",
            &s,
            &paths::normalize_relative_path(&s),
        );
    }
    for s in all_strings(&['A', 'a', '-', '_', '.'], 6) {
        emit(
            "normalize_distribution_name",
            &s,
            &names::normalize_distribution_name(&s),
        );
        emit("normalized", &s, &names::normalized(s.clone()));
    }
    for s in all_strings(&['a', '%', ':', '3', 'A', '2', '5'], 5) {
        emit("target_part", &s, &target::target_part(&s));
    }
    print!("{out}");
}
