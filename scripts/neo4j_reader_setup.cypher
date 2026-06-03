// Provision the read-only database user that nexus-scout connects as.
//
// This is defense layer 2: even if the sanitizer (layer 1) had a bug, this user
// cannot modify data, touch the system database, run procedures, or change
// schema. The server-side transaction timeout and memory limit are the real
// bounds on expensive queries (the driver provides no per-query server timeout).
//
// Run against the DBMS as an administrator, e.g.:
//   cat scripts/neo4j_reader_setup.cypher | cypher-shell -u neo4j -p <admin-pass>
//
// Adjust the password before running in any real deployment.
//
// IMPORTANT: GRANT ROLE / DENY are Neo4j ENTERPRISE features. On COMMUNITY
// edition there is no role-based access control - every user can write - so
// defense layer 2 is unavailable and the sanitizer (layer 1) is the sole write
// guard. See README "Security model". On Community, only the server-side
// transaction timeout / memory bounds below apply.

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
