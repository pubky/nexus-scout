# ADR-0001: Sanitizer is a tokenizer + allow/deny classifier

**Status:** Accepted (2026-06-01)

## Context

The gateway must block 100% of mutation attempts, including obfuscated ones (mixed case, comments,
semicolons, Unicode homoglyphs). A plain keyword scanner looks sufficient for an MVP, but a naive
whitespace scanner is wrong in both directions: it rejects valid queries where a keyword
appears inside a string literal (`WHERE n.bio CONTAINS 'please CREATE...'`), and it can be fooled into
missing a keyword hidden by a comment or a broken string boundary.

## Decision

Build a single-pass, O(n), no-backtracking lexer that classifies the input into typed tokens
(strings, backtick identifiers, comments, numbers, parameters, path ranges, words, punctuation), then
run a pure classifier over the **keyword-position word tokens only**. String/comment/backtick contents
are never examined.

The keyword table is exactly `DENY ∪ ALLOW` (clause/operator keywords). Function names are
deliberately absent, so a bare `f(...)` call lexes as an identifier and takes the allowed bare-call
path. A word is rejected only if it is an explicitly denied clause keyword; namespaced calls
(`a.b(...)`) are rejected by a separate rule; and the first word must be a read-entry clause.

We deliberately do **not** build a full Cypher grammar parser.

## Consequences

- ✅ Keyword-in-string and comment-hidden-keyword attacks are structurally impossible.
- ✅ Small, auditable, fuzzable surface; the lexer cannot blow up on adversarial input.
- ✅ Representative agent queries (including `shortestPath`, list comprehensions, map
  projections) pass, verified by a dedicated acceptance corpus.
- ❌ A bare variable that *spells* a denied keyword (e.g. a node named `create`) is rejected. This is
  an accepted, documented, safe over-rejection: rare, and the agent can rename the variable.
- ⚠️ Forward-compatibility against *new* write clauses rests on the read-entry rule plus defense
  layer 2, not on default-denying arbitrary words (which would reject every variable name).

## Alternatives considered

- **Naive keyword scanner**: fails the 100%-block bar (false positives and false negatives). Rejected.
- **Full Cypher parser**: large, version-coupled attack surface and maintenance burden for no
  additional safety over the token classifier. Rejected for the MVP; a parser could be revisited only
  if the over-rejection in the Consequences ever proves painful.
