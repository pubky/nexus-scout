# Deploying nexus-scout

nexus-scout runs as a hosted HTTP service co-located with Neo4j on one box, fronted by a Caddy
reverse proxy that terminates TLS. Only Caddy is public; neither the gateway nor Neo4j publishes a
host port, so the Bolt port and database credentials never leave the box.

```
agents ──HTTPS──▶ Caddy (TLS, optional per-IP rate limit) ──HTTP, private──▶ nexus-scout ──bolt──▶ Neo4j
```

> **Read before deploying - write protection.** This stack runs **Neo4j Community**, which has no
> role-based access control, so the configured reader user *can write*: the **sanitizer is the sole
> write guard**. Protection therefore rests on physical isolation - run the gateway against an
> **isolated, disposable replica re-cloned nightly** (writes never reach the primary; intra-day
> corruption is wiped on the next clone). Treat that isolation as a hard requirement, and re-run the
> sanitizer audit + integration suite on every Neo4j minor-version bump (see
> [`SECURITY_MATRIX.md`](SECURITY_MATRIX.md) and [ADR-0002](adr/0002-defense-in-depth.md)).

## Quick start

1. Create a `.env` next to `docker/docker-compose.prod.yml`:
   ```
   SCOUT_DOMAIN=scout.example.com
   NEO4J_ADMIN_PASSWORD=<strong-admin-password>
   NEO4J_SCOUT_PASSWORD=<strong-reader-password>
   ```
2. Bring up the stack:
   ```sh
   docker compose -f docker/docker-compose.prod.yml up -d
   ```
3. Provision the reader user once Neo4j is healthy:
   ```sh
   docker compose -f docker/docker-compose.prod.yml exec -T neo4j \
     cypher-shell -u neo4j -p "$NEO4J_ADMIN_PASSWORD" \
     < scripts/neo4j_reader_setup_community.cypher
   ```
4. Restart the gateway so it picks up the reader (or let it retry):
   ```sh
   docker compose -f docker/docker-compose.prod.yml restart nexus-scout
   ```

Agents then reach `https://$SCOUT_DOMAIN/v1/query`.

## Server-side cost bounds (required)

The real bound on a single expensive read is Neo4j's per-transaction timeout and memory limits, not
the gateway. The prod compose sets them on the Neo4j service:

```
NEO4J_db_transaction_timeout: 10s
NEO4J_db_memory_transaction_max: 64m
NEO4J_dbms_memory_transaction_total_max: 512m
```

Because the gateway runs with `NEXUS_SCOUT_PROFILE=production`, it **verifies these at startup and
refuses to boot if any is unset**, and `/ready` returns `503` until they are. Do not remove them.

## Per-IP rate limiting (recommended)

The gateway's own QPS shed (`HTTP_MAX_RPS`, default 50) is always on. For per-client fairness, enable
the Caddy `rate_limit` block in `docker/Caddyfile`. It needs the `caddy-ratelimit` plugin, which the
stock `caddy:2` image lacks; build a custom image:

```sh
xcaddy build --with github.com/mholt/caddy-ratelimit
```

point the compose `caddy` service at it, then uncomment the block.

## Transport to Neo4j (Bolt)

The gateway→Neo4j Bolt hop is separate from the public TLS that Caddy terminates. Under
`NEXUS_SCOUT_PROFILE=production` the gateway refuses **plaintext** `bolt://`/`neo4j://` to a
non-loopback host (credentials and query data must not cross a public link in the clear); use
`bolt+s://` for a genuinely remote database. The prod compose instead keeps Neo4j on the **private
Docker network with no published port** and opts into plaintext explicitly with
`NEO4J_ALLOW_INSECURE_TRANSPORT=true`. Only set that flag when the Bolt hop cannot leave a trusted
private network.

## Hardening checklist

- [ ] `NEXUS_SCOUT_PROFILE=production` (fail-closed on unset/unverifiable cost bounds and on plaintext
      remote Bolt unless `NEO4J_ALLOW_INSECURE_TRANSPORT=true` for a private-network DB).
- [ ] All three Neo4j cost bounds set; `/ready` returns 200.
- [ ] Neither `nexus-scout` nor `neo4j` publishes a host port - only Caddy does. The operational
      port (`METRICS_ADDR`, default `9090`) stays on the private network and is never fronted by Caddy.
- [ ] Running against an **isolated, nightly-recloned Neo4j replica** (Community: the sanitizer is the
      sole write guard; isolation is what protects the primary).
- [ ] Caddy per-IP `rate_limit` enabled.
- [ ] `NEO4J_SCOUT_PASSWORD` / `NEO4J_ADMIN_PASSWORD` set to strong values; treat the box as holding a
      database credential and prefer Docker secrets over inline env where possible.
- [ ] Watch `GET /metrics` on the internal operational port (`METRICS_ADDR`, default `9090`) for
      in-flight, shed, and 5xx counts, plus the slow-query `WARN` logs, for abuse.

## Tuning

All limits are env-configurable (see [`.env.example`](../.env.example)): `HTTP_MAX_BODY_BYTES`,
`HTTP_MAX_CONCURRENCY`, `HTTP_MAX_RPS`, `HTTP_REQUEST_TIMEOUT_MS`, plus the per-request guardrails
(`MAX_RESULT_ROWS`, `MAX_PARAM_*`, `QUERY_TIMEOUT_MS`, ...). The honest scope of what these do and do
not bound is in [ADR-0009](adr/0009-http-service-transport.md) and the README security model.
