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
- `POST /v1/matrix/facts`

Compatibility aliases under `/v1/compatibility` are available for adapters
migrating from track-based APIs.

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
