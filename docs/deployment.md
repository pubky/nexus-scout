# Deploying nexus-scout

nexus-scout runs as a hosted HTTP service co-located with Neo4j on one box, fronted by a Caddy
reverse proxy that terminates TLS. Only Caddy is public; neither the gateway nor Neo4j publishes a
host port, so the Bolt port and database credentials never leave the box.

```
agents ──HTTPS──▶ Caddy (TLS + per-IP rate limit) ──HTTP, private──▶ nexus-scout ──bolt──▶ Neo4j
```

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

## Hardening checklist

- [ ] `NEXUS_SCOUT_PROFILE=production` (fail-closed on unset cost bounds and on plaintext remote Bolt).
- [ ] All three Neo4j cost bounds set; `/ready` returns 200.
- [ ] Neither `nexus-scout` nor `neo4j` publishes a host port — only Caddy does.
- [ ] Caddy per-IP `rate_limit` enabled.
- [ ] `NEO4J_SCOUT_PASSWORD` / `NEO4J_ADMIN_PASSWORD` set to strong values; treat the box as holding a
      database credential and prefer Docker secrets over inline env where possible.
- [ ] Watch `GET /metrics` (in-flight, shed, 5xx) and the slow-query `WARN` logs for abuse.

## Tuning

All limits are env-configurable (see [`.env.example`](../.env.example)): `HTTP_MAX_BODY_BYTES`,
`HTTP_MAX_CONCURRENCY`, `HTTP_MAX_RPS`, `HTTP_REQUEST_TIMEOUT_MS`, plus the per-request guardrails
(`MAX_RESULT_ROWS`, `MAX_PARAM_*`, `QUERY_TIMEOUT_MS`, ...). The honest scope of what these do and do
not bound is in [ADR-0009](adr/0009-http-service-transport.md) and the README security model.
