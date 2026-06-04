// Provision the read-only database user that nexus-scout connects as, on Neo4j
// ENTERPRISE.
//
// This is defense layer 2: even if the sanitizer (layer 1) had a bug, this user
// cannot modify data, touch the system database, run procedures, or change
// schema. The server-side transaction timeout and memory limit are the real
// bounds on expensive queries (the driver provides no per-query server timeout).
//
// The GRANT ROLE / DENY statements below are ENTERPRISE-only and error on
// Community edition. For Community, use neo4j_reader_setup_community.cypher
// (the sanitizer is the sole write guard there). See docs/adr/0002-defense-in-depth.md.
//
// Run against the DBMS as an administrator, e.g.:
//   cat scripts/neo4j_reader_setup_enterprise.cypher | cypher-shell -u neo4j -p <admin-pass>
//
// Adjust the password before running in any real deployment.

CREATE USER nexus_scout_reader IF NOT EXISTS
  SET PASSWORD 'change-me-in-production'
  SET PASSWORD CHANGE NOT REQUIRED;

GRANT ROLE reader TO nexus_scout_reader;

// Belt-and-suspenders denials on top of the built-in `reader` role.
DENY WRITE ON GRAPH * TO nexus_scout_reader;
DENY ALL ON DATABASE system TO nexus_scout_reader;
DENY EXECUTE PROCEDURE * ON DBMS TO nexus_scout_reader;
DENY EXECUTE BOOSTED PROCEDURE * ON DBMS TO nexus_scout_reader;

// --- Server-side resource bounds (set per-database or in neo4j.conf) ---
// These are the authoritative bounds on runaway reads; the client timeout only
// bounds caller liveness. Apply the matching neo4j.conf settings in production:
//   db.transaction.timeout=10s
//   dbms.memory.transaction.total.max=512m
//   db.memory.transaction.max=64m
