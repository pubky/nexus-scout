# ADR-0008: Defer the natural-language → Cypher layer

**Status:** Accepted (2026-06-01)

## Context

The spec describes an optional future `query_graph` tool that translates a natural-language question
into Cypher with an LLM. The original request mentioned a "natural language parser."

## Decision

Do not build the NL→Cypher layer in this MVP. The gateway delivers the read-only Cypher path only
(`query_cypher` + `get_schema`).

## Consequences

- ✅ The gateway stays deterministic, dependency-free of any LLM, and easy to security-audit.
- ✅ Calling agents already have their own LLM and can generate Cypher from `get_schema`.
- ⚠️ If agents consistently struggle to produce good Cypher in practice, an NL layer can be added
  later as a thin tool that emits Cypher into this same validated path - it needs the gateway
  underneath either way.

## Alternatives considered

- **Build NL→Cypher now**: adds an LLM dependency and a large prompt/test surface for a capability
  the calling agents already have. Rejected for the MVP.
