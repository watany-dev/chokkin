//! Property-based tests for cache key hashing, fingerprints, and CLI parsing.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use chokkin::cache::{CacheKeyHasher, stable_hex_hash, stable_list_hash};
use chokkin::{SourceFingerprint, parse_cli_args};
use proptest::prelude::*;
use tempfile::TempDir;

proptest! {
    /// Hashing is deterministic and always 16 lowercase hex digits.
    #[test]
    fn hash_is_deterministic_hex(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
        let a = stable_hex_hash(&bytes);
        prop_assert_eq!(&a, &stable_hex_hash(&bytes));
        prop_assert_eq!(a.len(), 16);
        prop_assert!(a.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    /// Length-prefixed fields: moving a byte across a field boundary changes
    /// the hash (no `("ab","c") == ("a","bc")` collisions).
    #[test]
    fn field_boundaries_are_part_of_the_hash(
        left in "[a-z]{1,6}",
        right in "[a-z]{1,6}",
    ) {
        let joined = format!("{left}{right}");
        for split in 1..joined.len() {
            if split == left.len() {
                continue;
            }
            let mut a = CacheKeyHasher::new();
            a.field_str(&left).field_str(&right);
            let mut b = CacheKeyHasher::new();
            b.field_str(&joined[..split]).field_str(&joined[split..]);
            prop_assert_ne!(a.finish(), b.finish());
        }
    }

    /// List hashing is order sensitive and element-boundary sensitive.
    #[test]
    fn list_hash_is_order_sensitive(values in prop::collection::vec("[a-z]{0,4}", 2..6)) {
        let mut reversed = values.clone();
        reversed.reverse();
        prop_assume!(reversed != values);
        prop_assert_ne!(stable_list_hash(&values), stable_list_hash(&reversed));
    }

    /// Splitting one element into two never collides with the original list.
    #[test]
    fn list_hash_distinguishes_split_elements(value in "[a-z]{2,8}", at in 1usize..7) {
        prop_assume!(at < value.len());
        let whole = stable_list_hash([value.as_str()]);
        let split = stable_list_hash([&value[..at], &value[at..]]);
        prop_assert_ne!(whole, split);
    }

    /// Fingerprints change whenever the file bytes change, even when the size
    /// is preserved.
    #[test]
    fn fingerprint_tracks_content(
        first in prop::collection::vec(any::<u8>(), 1..64),
        flip in any::<prop::sample::Index>(),
    ) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("m.py");
        std::fs::write(&path, &first).expect("write");
        let before = SourceFingerprint::from_root_relative(dir.path(), "m.py").expect("fp");
        let mut second = first;
        let i = flip.index(second.len());
        second[i] ^= 0xff;
        std::fs::write(&path, &second).expect("write");
        let after = SourceFingerprint::from_root_relative(dir.path(), "m.py").expect("fp");
        prop_assert_ne!(before, after);
    }

    /// Fingerprints of identical content are equal and use `/` paths.
    #[test]
    fn fingerprint_is_stable_for_same_content(
        bytes in prop::collection::vec(any::<u8>(), 0..64),
        dir_name in "[a-z]{1,5}",
    ) {
        let dir = TempDir::new().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(&dir_name)).expect("mkdir");
        std::fs::write(dir.path().join(&dir_name).join("m.py"), &bytes).expect("write");
        let rel = format!("{dir_name}/m.py");
        let a = SourceFingerprint::from_root_relative(dir.path(), &rel).expect("fp");
        let b = SourceFingerprint::from_root_relative(dir.path(), &rel).expect("fp");
        prop_assert_eq!(&a, &b);
        prop_assert_eq!(a.path, rel);
        prop_assert_eq!(a.size, bytes.len() as u64);
    }

    /// Arbitrary argv never panics the CLI parser.
    #[test]
    fn cli_parser_is_total(args in prop::collection::vec("(--)?[a-z-]{0,12}(=[a-z,]{0,5})?", 0..6)) {
        let _ = parse_cli_args(args);
    }

    /// `--fix` sub-flags are rejected without `--fix`.
    #[test]
    fn fix_subflags_require_fix(flag in prop::sample::select(vec!["--dry-run", "--allow-remove-files", "--add-missing"])) {
        prop_assert!(parse_cli_args(vec![flag.to_owned()]).is_err());
        prop_assert!(parse_cli_args(vec!["--fix".to_owned(), flag.to_owned()]).is_ok());
    }

    /// `--include`/`--exclude` split on commas and round-trip the items.
    #[test]
    fn include_splits_on_commas(items in prop::collection::vec("[a-z*./]{1,6}", 1..5)) {
        let cli = parse_cli_args(vec![format!("--include={}", items.join(","))]).expect("parse");
        prop_assert_eq!(cli.include, Some(items));
    }
}
