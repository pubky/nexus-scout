// Provision the database user that nexus-scout connects as, on Neo4j COMMUNITY.
//
// Community edition has no role-based access control, so this user CAN write:
// defense layer 2 (a DENY WRITE reader role) is unavailable here, and the
// sanitizer (layer 1) is the SOLE write guard. The only server-side protection
// is the transaction timeout / memory limit below (a resource bound, not a
// write bound). See README "Security model" and docs/adr/0002-defense-in-depth.md.
//
// For RBAC-enforced write denial, use neo4j_reader_setup_enterprise.cypher on
// Neo4j Enterprise instead.
//
// Run against the DBMS as an administrator, e.g.:
//   cat scripts/neo4j_reader_setup_community.cypher | cypher-shell -u neo4j -p <admin-pass>
//
// Adjust the password before running in any real deployment.

CREATE USER nexus_scout_reader IF NOT EXISTS
  SET PASSWORD 'change-me-in-production'
  SET PASSWORD CHANGE NOT REQUIRED;

// --- Server-side resource bounds (set in neo4j.conf; shown here for reference) ---
// On Community these are the ONLY server-side bounds on a runaway read; the
// client timeout only bounds caller liveness. Apply in production:
//   db.transaction.timeout=10s
//   dbms.memory.transaction.total.max=512m
//   db.memory.transaction.max=64m
