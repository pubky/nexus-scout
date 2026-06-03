//! Compile-time assertions that the `SanitizedQuery` typestate cannot be forged.
//!
//! The strongest seal is structural: `cypher-guard` has no `serde` dependency,
//! so `SanitizedQuery` *cannot* implement `Deserialize` (you could not even name
//! the trait here). What remains to pin is that the type stays `Send`/`Sync` for
//! the gateway and exposes no `Default` (which would mint an empty query). These
//! assertions fail to compile if someone derives `Default` on it later.

use cypher_guard::SanitizedQuery;
use static_assertions::{assert_impl_all, assert_not_impl_any};

assert_impl_all!(SanitizedQuery: Send, Sync, Clone);
assert_not_impl_any!(SanitizedQuery: Default);

#[test]
fn typestate_seal_holds() {
    // The assertions above are checked at compile time; this test exists so the
    // file is part of the test binary and the seal is exercised by `cargo test`.
}
