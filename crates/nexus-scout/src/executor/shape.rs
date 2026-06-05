//! Pure result-shaping: apply the row and byte caps to a stream of converted JSON rows.

use serde_json::{Map, Value};

/// Accumulates shaped rows under a row budget and a byte cap. [`RowShaper::push`]
/// returns `false` once no further rows should be read. The executor should read
/// up to `budget + 1` so a row-cap truncation can be detected.
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
        // saturating: serialized_len returns usize::MAX on its (unreachable) failure path; must not overflow.
        if self.bytes.saturating_add(row_bytes) > self.byte_cap {
            // A row that would push the summed payload bytes over the cap is dropped, even if it is the first row.
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
        let (rows, truncated) = drain(RowShaper::new(2, 1 << 20), 3);
        assert_eq!(rows.len(), 2);
        assert!(truncated);
    }

    #[test]
    fn byte_cap_truncates_on_multi_row_accumulation() {
        let (rows, truncated) = drain(RowShaper::new(100, 10), 3);
        assert_eq!(rows.len(), 1);
        assert!(truncated);
    }

    #[test]
    fn lone_oversized_row_is_dropped_and_flagged_truncated() {
        let (rows, truncated) = drain(RowShaper::new(100, 1), 1);
        assert!(rows.is_empty(), "the oversized row must not be returned");
        assert!(truncated, "and it is flagged truncated");
    }

    #[test]
    fn byte_cap_bounds_row_payloads_not_the_full_response() {
        use crate::response::QueryResponse;
        let cap = 50;
        let mut shaper = RowShaper::new(1000, cap);
        let mut n = 0;
        while shaper.push(row(n)) {
            n += 1;
        }
        let (rows, truncated) = shaper.finish();
        assert!(truncated, "the byte cap should engage");
        // The summed row payloads stay within the cap, but the full serialized
        // response (envelope + commas) can exceed it.
        let payload: usize = rows.iter().map(crate::params::serialized_len).sum();
        assert!(payload <= cap, "row payload sum {payload} must be within the cap {cap}");
        let response_len = serde_json::to_vec(&QueryResponse::new(rows, truncated)).unwrap().len();
        assert!(
            response_len > cap,
            "the response envelope pushes the size ({response_len}) past the cap ({cap})"
        );
    }

    #[test]
    fn exact_budget_is_not_truncated() {
        let (rows, truncated) = drain(RowShaper::new(3, 1 << 20), 3);
        assert_eq!(rows.len(), 3);
        assert!(!truncated);
    }
}
