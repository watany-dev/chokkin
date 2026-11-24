//! Copies from `src/parser/version_guard.rs` (sync-checked), with the
//! `ruff_python_ast::CmpOp` variants the copies match on.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    NotEq,
    Lt,
    LtE,
    Gt,
    GtE,
    Is,
    IsNot,
    In,
    NotIn,
}

pub type Minors = u128;

const LAST_MINOR: u32 = Minors::BITS - 1;

pub struct VersionTest {
    pub op: CmpOp,
    pub major: u32,
    pub minor: Option<u32>,
    /// `sys.version_info` itself, which is longer than any tuple it is
    /// compared with: `(3, 9, 0, "final", 0) > (3, 9)`.
    pub full: bool,
    /// Only the major version is compared (`sys.version_info[0] == 2`).
    pub major_only: bool,
}

impl VersionTest {
    fn holds(&self, minor: u32) -> bool {
        let ordering = 3_u32
            .cmp(&self.major)
            .then_with(|| match self.minor {
                _ if self.major_only => std::cmp::Ordering::Equal,
                Some(bound) => minor.cmp(&bound),
                None => std::cmp::Ordering::Greater,
            })
            .then(if self.full {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            });
        match self.op {
            CmpOp::Lt => ordering.is_lt(),
            CmpOp::LtE => ordering.is_le(),
            CmpOp::Gt => ordering.is_gt(),
            CmpOp::GtE => ordering.is_ge(),
            CmpOp::Eq => ordering.is_eq(),
            _ => ordering.is_ne(),
        }
    }
}

/// `minors` as inclusive runs; a run through the last bit ends at
/// `u32::MAX`.
pub fn minor_runs(minors: Minors) -> Vec<[u32; 2]> {
    let mut runs: Vec<[u32; 2]> = Vec::new();
    for minor in (0..=LAST_MINOR).filter(|minor| minors & (1 << minor) != 0) {
        let high = if minor == LAST_MINOR { u32::MAX } else { minor };
        match runs.last_mut() {
            Some(run) if run[1] + 1 == minor => run[1] = high,
            _ => runs.push([minor, high]),
        }
    }
    runs
}
