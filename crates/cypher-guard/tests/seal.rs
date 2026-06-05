//! Compile-time assertions that `SanitizedQuery` stays `Send`/`Sync` and exposes
//! no `Default` (which would mint an empty query). The macros below fail to
//! compile if that changes.

use cypher_guard::SanitizedQuery;
use static_assertions::{assert_impl_all, assert_not_impl_any};

assert_impl_all!(SanitizedQuery: Send, Sync, Clone);
assert_not_impl_any!(SanitizedQuery: Default);

#[test]
fn typestate_seal_holds() {
    // No-op: the real checks are the static-assert macros above; this just lists
    // the seal in `cargo test`.
}
