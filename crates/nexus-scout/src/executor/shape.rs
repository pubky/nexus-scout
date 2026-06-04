//! Pure result-shaping: apply the row and byte caps to a stream of already
//! converted JSON rows. This is the unit-test target for all the off-by-one and
//! truncation edge cases, with no `neo4rs` types in its signature.

use serde_json::{Map, Value};

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
    truncated: bool,
}

impl RowShaper {
    pub(crate) fn new(budget: usize, byte_cap: usize) -> Self {
        Self {
            rows: Vec::new(),
            budget,
            byte_cap,
            bytes: 0,
            truncated: false,
        }
    }

    /// Offers one row. Returns `true` if more rows are wanted, `false` if the
    /// shaper is full (the caller should stop reading).
    pub(crate) fn push(&mut self, row: Map<String, Value>) -> bool {
        if self.rows.len() >= self.budget {
            self.truncated = true;
            return false;
        }
        let row_bytes = crate::params::serialized_len(&row);
        // saturating: `serialized_len` returns usize::MAX on the (unreachable for
        // valid JSON) failure path, which must not overflow this running total.
        if self.bytes.saturating_add(row_bytes) > self.byte_cap {
            // Hard cap: a row that would push the serialized response over the
            // cap is dropped, even if it is the first row, so the response is
            // never larger than the cap. The caller sees `truncated` and should
            // narrow its RETURN (project fewer/smaller fields).
            self.truncated = true;
            return false;
        }
        self.bytes += row_bytes;
        self.rows.push(row);
        true
    }

    /// Consumes the shaper, returning the rows and whether the result truncated.
    pub(crate) fn finish(self) -> (Vec<Map<String, Value>>, bool) {
        (self.rows, self.truncated)
    }
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

    fn drain(mut shaper: RowShaper, count: i64) -> (Vec<Map<String, Value>>, bool) {
        for i in 0..count {
            if !shaper.push(row(i)) {
                break;
            }
        }
        shaper.finish()
    }

    #[test]
    fn under_budget_keeps_all_rows() {
        let (rows, truncated) = drain(RowShaper::new(10, 1 << 20), 3);
        assert_eq!(rows.len(), 3);
        assert!(!truncated);
    }

    #[test]
    fn over_budget_truncates_at_budget() {
        // Caller offers budget+1 rows; the extra one trips the row cap.
        let (rows, truncated) = drain(RowShaper::new(2, 1 << 20), 3);
        assert_eq!(rows.len(), 2);
        assert!(truncated);
    }

    #[test]
    fn byte_cap_truncates_on_multi_row_accumulation() {
        // Rows that each fit but together exceed the cap: the cap acts between
        // whole rows, so the first is kept and the second trips it.
        let (rows, truncated) = drain(RowShaper::new(100, 10), 3);
        assert_eq!(rows.len(), 1);
        assert!(truncated);
    }

    #[test]
    fn lone_oversized_row_is_dropped_and_flagged_truncated() {
        // Hard cap: a single row larger than the whole byte cap is dropped, not
        // returned, so the serialized response never exceeds the cap. The caller
        // sees an empty, truncated result and must narrow its RETURN.
        let (rows, truncated) = drain(RowShaper::new(100, 1), 1);
        assert!(rows.is_empty(), "the oversized row must not be returned");
        assert!(truncated, "and it is flagged truncated");
    }

    #[test]
    fn exact_budget_is_not_truncated() {
        let (rows, truncated) = drain(RowShaper::new(3, 1 << 20), 3);
        assert_eq!(rows.len(), 3);
        assert!(!truncated);
    }
}
