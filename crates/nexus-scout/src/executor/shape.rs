//! Pure result-shaping: apply the row and byte caps to a stream of already
//! converted JSON rows. This is the unit-test target for all the off-by-one and
//! truncation edge cases, with no `neo4rs` types in its signature.

use serde_json::{Map, Value};

/// Why a result set was truncated, if at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Truncation {
    /// The full result was returned.
    None,
    /// More rows existed than the row budget allowed.
    RowCap,
    /// The serialized byte size reached the byte cap.
    ByteCap,
}

impl Truncation {
    /// Whether any truncation occurred (the wire `truncated` flag).
    pub(crate) fn occurred(self) -> bool {
        !matches!(self, Truncation::None)
    }
}

/// Accumulates shaped rows under a row budget and a byte cap.
///
/// The caller pushes each converted row; [`RowShaper::push`] returns `false`
/// once no further rows should be read, so the executor can stop pulling from
/// the stream. `budget` is the number of rows to return; the executor should
/// read up to `budget + 1` so a row-cap truncation can be detected.
pub(crate) struct RowShaper {
    rows: Vec<Map<String, Value>>,
    budget: usize,
    byte_cap: usize,
    bytes: usize,
    truncation: Truncation,
}

impl RowShaper {
    pub(crate) fn new(budget: usize, byte_cap: usize) -> Self {
        Self {
            rows: Vec::new(),
            budget,
            byte_cap,
            bytes: 0,
            truncation: Truncation::None,
        }
    }

    /// Offers one row. Returns `true` if more rows are wanted, `false` if the
    /// shaper is full (the caller should stop reading).
    pub(crate) fn push(&mut self, row: Map<String, Value>) -> bool {
        if self.rows.len() >= self.budget {
            self.truncation = Truncation::RowCap;
            return false;
        }
        let row_bytes = serialized_len(&row);
        if self.bytes + row_bytes > self.byte_cap && !self.rows.is_empty() {
            self.truncation = Truncation::ByteCap;
            return false;
        }
        self.bytes += row_bytes;
        self.rows.push(row);
        true
    }

    /// Consumes the shaper, returning the rows and how (if at all) it truncated.
    pub(crate) fn finish(self) -> (Vec<Map<String, Value>>, Truncation) {
        (self.rows, self.truncation)
    }
}

fn serialized_len(row: &Map<String, Value>) -> usize {
    serde_json::to_vec(row).map_or(0, |v| v.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(n: i64) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("n".into(), json!(n));
        m
    }

    fn drain(mut shaper: RowShaper, count: i64) -> (Vec<Map<String, Value>>, Truncation) {
        for i in 0..count {
            if !shaper.push(row(i)) {
                break;
            }
        }
        shaper.finish()
    }

    #[test]
    fn under_budget_keeps_all_rows() {
        let (rows, t) = drain(RowShaper::new(10, 1 << 20), 3);
        assert_eq!(rows.len(), 3);
        assert_eq!(t, Truncation::None);
        assert!(!t.occurred());
    }

    #[test]
    fn over_budget_truncates_at_budget() {
        // Caller offers budget+1 rows; the extra one trips the row cap.
        let (rows, t) = drain(RowShaper::new(2, 1 << 20), 3);
        assert_eq!(rows.len(), 2);
        assert_eq!(t, Truncation::RowCap);
        assert!(t.occurred());
    }

    #[test]
    fn byte_cap_truncates_but_keeps_at_least_one_row() {
        // Cap below a single row's size: the first row is always kept, the
        // second trips the byte cap.
        let (rows, t) = drain(RowShaper::new(100, 1), 3);
        assert_eq!(rows.len(), 1);
        assert_eq!(t, Truncation::ByteCap);
    }

    #[test]
    fn exact_budget_is_not_truncated() {
        let (rows, t) = drain(RowShaper::new(3, 1 << 20), 3);
        assert_eq!(rows.len(), 3);
        assert_eq!(t, Truncation::None);
    }
}
