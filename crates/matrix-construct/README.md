# matrix-construct

`matrix-construct` is a generic compatibility matrix construct service. It stores
facts, lists zones, evaluates simple gates, and serves facts for SQL-style
clients such as the `matrix` CLI.

## API

- `GET /healthz`
- `GET /v1/matrix`
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

## Fact History

`POST /v1/matrix/facts` treats `fact.id` as the stable logical fact key. When a
producer submits the same fact ID again, the current `facts` row is updated and
the construct appends an immutable event to `fact_events`.

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

## Run

```bash
cargo run -p matrix-construct
```

Configuration:

- `MATRIX_CONSTRUCT_ADDR`, default `0.0.0.0:8080`
- `MATRIX_CONSTRUCT_DB`, default in-memory SQLite

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
