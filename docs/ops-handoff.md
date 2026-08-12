# DevOps handoff — nexus-scout production hardening

Two deployment-layer items came out of the security audit. Both are **infrastructure**,
not application code — they can't be fixed in this repo's Rust. They are listed in
priority order. Context first, then exactly what to build and how we'll know it's done.

## Why this matters (read once)

nexus-scout is a **public, unauthenticated, read-only** Cypher gateway in front of a
Neo4j **Community** replica. Community edition has no role-based access control, so the
application-level sanitizer is the **sole write guard** — there is no `DENY WRITE`
database role behind it. The security model therefore leans on two infrastructure
guarantees that the app cannot provide itself:

1. the Neo4j the gateway talks to is an **isolated replica** (writes can never reach the
   primary), and
2. that replica is **re-cloned nightly**, so anything that ever slips the sanitizer is
   wiped within a day.

Item 1 (F1) makes that real. Item 2 (F4) keeps one abusive client from taking the public
endpoint down. The application-side bounds (query timeout, row/byte caps, global QPS
shed, body cap, fail-closed cost-bound gate) are already implemented and shipping.

---

## F1 — Nightly replica re-clone + write detection — **HIGH**

**Current state:** the nightly re-clone is described in `README.md`, `docs/deployment.md`
(lines ~14 and ~86), and `docs/adr/0002-defense-in-depth.md`, but it exists **only as
prose**. There is no script, cron job, Compose init, or K8s CronJob in the repo, and
**nothing detects** a write that lands on the replica. If the (currently manual /
external) process is ever forgotten or fails silently, a sanitizer-bypass write persists
and compounds indefinitely — and we'd never know.

**What to build:**

1. **Isolation (verify, don't assume).** Confirm the gateway's Neo4j is a one-way read
   replica or a snapshot restore — never a live bidirectional cluster member. A write
   that reaches it must have **no path** back to the primary. Document the topology.
2. **Automated nightly re-clone.** A scheduled job (cron / systemd timer / K8s CronJob)
   that fully replaces the replica's data with a fresh copy from the primary's
   snapshot/backup — not an incremental sync (an incremental sync would *propagate* a
   bypass write, not wipe it). Replacing the `neo4j-data` volume from a known-good
   snapshot is the intended shape. State the RPO (≤24 h) and the maintenance window.
3. **Monitoring on the job itself.** Alert on failure or a missed run. A re-clone that
   silently stops is the same as not having one.
4. **Write-canary / drift detector.** A cheap periodic check (e.g. every 15–60 min) that
   detects whether the replica was modified since the last clone, and alarms if so. This
   closes the up-to-24-h window between a bypass write and the nightly wipe. Practical
   options: count nodes/relationships and compare to the post-clone baseline; or query
   for any node/rel whose `indexed_at`/creation marker is newer than the clone timestamp;
   or checksum a known-stable subgraph. The gateway is read-only and the replica takes no
   other writers, so **any** detected change is a signal worth paging on.
5. **Make the checklist enforceable.** Wire the "isolated, nightly-recloned replica"
   checkbox in `docs/deployment.md` to the actual automation above so it can't ship
   un-done.

**Done when:**
- the re-clone runs on schedule and is monitored (alert on failure/miss);
- a deliberately injected test write to the replica is **gone after the next clone**, and
- that same test write **fires the canary alarm** before the clone, in staging.

---

## F4 — Caddy per-IP rate limiting — **MEDIUM**

**Current state:** the gateway's rate limiter is **global** (`HTTP_MAX_RPS`, default 50
rps) — a single abusive IP can consume the entire budget and starve every other client.
Per-IP limiting has to live in **Caddy**, because only the reverse proxy sees the real
client IP (the gateway deliberately does not trust `X-Forwarded-For`). The `rate_limit`
block is already written in `docker/Caddyfile` (lines ~9–21) but **commented out**,
because the stock `caddy:2` image doesn't include the plugin.

**What to build:**

1. **Custom Caddy image with the ratelimit plugin:**
   `xcaddy build --with github.com/mholt/caddy-ratelimit` — pin both the Caddy version and
   the plugin version, build, and publish to our registry.
2. **Point the compose `caddy` service at that image** (`docker/docker-compose.prod.yml`,
   the `caddy` service) instead of `caddy:2`.
3. **Enable the block** in `docker/Caddyfile`: uncomment the `rate_limit` block, key on
   `{remote_host}`, and tune `events`/`window` to expected legitimate usage (the existing
   placeholder is 60 events / 1 min per IP — a reasonable starting point).
4. **Verify the real client IP reaches Caddy.** If anything sits in front of Caddy (an L4/L7
   load balancer, CDN, another proxy), configure Caddy's trusted-proxy / real-IP handling
   so the limiter keys on the actual client, not the upstream hop. TLS termination and
   automatic certs must keep working.

**Done when:**
- a single IP exceeding the per-IP rate gets **429 from Caddy**, while a second IP is
  unaffected; and
- the gateway's global QPS shed still works as the backstop behind it.

---

## Out of scope for DevOps (tracked separately, dev/CI track)

For visibility — these came out of the same audit but are **code/CI**, not infrastructure,
and are not asking anything of the DevOps team:

- **F5:** lengthen and PR-trigger the sanitizer fuzz job (`.github/workflows/fuzz.yml`).
- **F6:** cache `/ready`'s `SHOW SETTINGS` call. Keeping `/ready` + `/metrics` off the
  public proxy is now structural: the operational endpoints bind a separate port
  (`METRICS_ADDR`, default loopback) that Caddy never fronts.
- **F8:** refresh the audit docs / error-reason text.

The application-code gaps from the audit (quantified-path cost guard, connection-pool
sizing, parameter tokenization) are **already fixed** in the codebase.
