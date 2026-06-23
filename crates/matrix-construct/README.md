# matrix-construct

`matrix-construct` is a generic compatibility matrix construct service. It stores
facts, lists zones, evaluates simple gates, and serves facts for SQL-style
clients such as the `matrix` CLI.

## API

- `GET /healthz`
- `GET /readyz`
- `GET /openapi.json`
- `GET /v1/matrix`
- `GET /v1/matrix/openapi.json`
- `GET /v1/matrix/zones/{zone}`
- `GET /v1/matrix/zones/{zone}/gates/{level}`
- `GET /v1/matrix/zones/{zone}/candidates/{level}`
- `GET /v1/matrix/facts`
- `GET /v1/matrix/facts/latest`
- `GET /v1/matrix/facts/{id}`
- `GET /v1/matrix/facts/{id}/history`
- `POST /v1/matrix/facts`

Compatibility aliases under `/v1/compatibility` are available for adapters
migrating from track-based APIs.

## Operations Contract

`GET /healthz` is a liveness probe. It does not touch storage and should be
used to tell whether the process is alive.

`GET /readyz` is a readiness probe. It runs a small SQLite query and returns
`200` only when the construct can use its configured store.

All API errors use a stable JSON envelope:

```json
{
  "error": {
    "code": "bad_request",
    "message": "cursor is invalid",
    "status": 400
  }
}
```

Fact and history list endpoints are paginated. The default fact page size is
`100`, the default history page size is `25`, and the maximum page size is
`200`. Requests above the maximum are bounded to `200`; responses include
`page.maxLimit` so clients can adapt without out-of-band configuration.

The OpenAPI document is available at `/openapi.json`,
`/v1/matrix/openapi.json`, and `/v1/compatibility/openapi.json`. CI validates
that the spec is parseable and includes the core production paths.

## Authentication

`matrix-construct` is unauthenticated by default for local development, demos,
and test fixtures.

Set `MATRIX_CONSTRUCT_TOKEN` to require authenticated writes. When configured,
`POST /v1/matrix/facts` and the `/v1/compatibility/facts` alias require:

```http
Authorization: Bearer <MATRIX_CONSTRUCT_TOKEN>
```

Read endpoints remain unauthenticated. Production deployments that need
read-level authorization should place the construct behind an API gateway,
service mesh, or platform API that enforces organization-specific policy. The
OSS construct intentionally does not encode internal identity-provider,
repository, or zone ownership rules.

## Fact History

`POST /v1/matrix/facts` treats `fact.id` as the stable logical fact key. When a
producer submits the same fact ID with changed content, the current `facts` row
is updated and the construct appends an immutable event to `fact_events`.
Submitting the same fact ID with identical content is idempotent: no new event
is appended and the upload response increments `duplicates`.

The current or selected fact body is exposed through the standard fact getter:

```bash
curl http://127.0.0.1:8080/v1/matrix/facts/example
curl 'http://127.0.0.1:8080/v1/matrix/facts/example?relative=-1'
curl 'http://127.0.0.1:8080/v1/matrix/facts/example?revision=3&relative=-1'
```

Full history is exposed through:

```bash
curl http://127.0.0.1:8080/v1/matrix/facts/example/history
curl 'http://127.0.0.1:8080/v1/matrix/facts/example/history?relative=-1'
curl 'http://127.0.0.1:8080/v1/matrix/facts/example/history?asOf=2026-06-19'
```

The response shape is:

```json
{
  "factId": "example",
  "events": [
    {
      "eventId": "event...",
      "factId": "example",
      "revision": 2,
      "acceptedAt": "2026-06-19T16:00:00Z",
      "contentHash": "sha256...",
      "sourceRepository": "example/repo",
      "sourceSha": "abc123",
      "sourceRef": "v1.0.0",
      "status": "current",
      "current": true,
      "supersededBy": null,
      "supersededAt": null,
      "fact": {}
    }
  ],
  "page": {
    "limit": 25,
    "total": 1,
    "nextCursor": null
  }
}
```

Selectors return a one-event `events` page:

- `revision=2`: exact revision number.
- `eventId=event...`: exact immutable event ID.
- `relative=-1`: offset from the current revision.
- `revision=3&relative=-1`: offset from a specific base revision.
- `eventId=event...&relative=-1`: offset from a specific base event.
- `asOf=2026-06-19` or `asOf=2026-06-19T16:00:00Z`: latest revision accepted
  at or before a date or timestamp.

Producers should use stable fact IDs for logical records that can change over
time, such as a release tuple or compatibility assertion. The construct assigns
immutable `eventId` values for each accepted body. Do not reuse event IDs as
producer input.

Existing SQLite stores are migrated in place on startup. Current `facts` rows
are backfilled as revision `1` events using their existing JSON body,
`observed_at` as the best available accepted time, and a content hash derived
from the stored JSON. Future writes append new revisions before updating the
current row.

## Storage

The current durable storage mode is SQLite. Configure it with:

```bash
MATRIX_CONSTRUCT_DB=/var/lib/matrix-construct/matrix.sqlite
```

When `MATRIX_CONSTRUCT_DB` is unset, the construct uses in-memory SQLite. That
mode is useful for tests and demos only.

Schema migrations are additive and run on startup. Existing rows are preserved,
missing columns are added, and historical events are backfilled from current
fact rows when needed. External databases are intentionally outside the current
OSS construct contract; deployments that need MySQL/PostgreSQL-backed policy
or multi-tenant authorization should front or extend the generic API rather
than hard-coding organization-specific behavior here.

## Run

```bash
cargo run -p matrix-construct
```

Configuration:

- `MATRIX_CONSTRUCT_ADDR`, default `0.0.0.0:8080`
- `MATRIX_CONSTRUCT_DB`, default in-memory SQLite
- `MATRIX_CONSTRUCT_TOKEN`, optional bearer token required for fact writes

## Upload

```bash
curl -X POST http://127.0.0.1:8080/v1/matrix/facts \
  -H 'content-type: application/json' \
  -d '{"fact":{"id":"example","zone":"sdk","status":"passed"}}'
```

Then:

```bash
matrix --construct http://127.0.0.1:8080 list
matrix --construct http://127.0.0.1:8080 query 'select id, zone, status from facts'
```

## Container

Build from the repository root:

```bash
docker build -f crates/matrix-construct/Dockerfile -t matrix-construct .
docker run --rm -p 8080:8080 matrix-construct
```

Release validation builds and smokes the image with `matrix-construct
--version`. Publishing releases push the image to:

```text
ghcr.io/adrianmross/matrix-construct:<version>
```
