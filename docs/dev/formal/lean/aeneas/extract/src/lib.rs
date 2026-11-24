//! Verbatim copies of chokkin's pure functions, for Charon and Aeneas.
//!
//! Every function listed in `../check_sync.py` must stay token-for-token
//! equal to the one in chokkin's `src/`; that script compares them.
//! Where Aeneas has no model of a std API, the copy calls a local stand-in
//! instead; each stand-in is listed in `../README.ja.md` with the std
//! behaviour it reproduces.

#![allow(dead_code)]

pub mod names;
pub mod paths;
pub mod python_range;
pub mod target;
pub mod version_guard;
