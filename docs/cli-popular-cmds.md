# Popular Matrix Commands

## Setup

```bash
matrix --version
matrix doctor
matrix config list
matrix config set construct https://matrix.example.dev
matrix config set api-prefix /v1/matrix
matrix completion zsh
```

## Query The Construct

```bash
matrix list
matrix view runtime
matrix current --zone runtime --level preview
matrix gate --zone runtime --level stage
matrix trace --zone runtime --subject payments-api
```

## Work In A Repository

Matrix detects the current git repo, ref, tag, and SHA. Override context when
you need to inspect a different component:

```bash
matrix components --zone runtime
matrix versions payments-api --repo example/payments-api
matrix tags --repo example/payments-api
matrix upstream --repo example/payments-api --version v1.6.3
matrix downstream --repo example/auth-service --version v2.1.0
matrix compatible --repo example/payments-api --version v1.6.3
matrix compare example/ledger-service --repo example/payments-api --version v1.6.3
matrix why ledger-service --repo example/payments-api --version v1.6.3
```

## Publish Producer Evidence

```bash
matrix upload facts.json
matrix publish facts.json
matrix ingest tox --file tox-result.json
matrix ingest nox --file nox-result.json
matrix ingest junit --file junit.xml
matrix ingest sbom --file bom.cdx.json
matrix ingest sbom --file bom.cdx.json --upload
```

## SQL

```bash
matrix query 'select id, zone, status from facts limit 20'
matrix query --zone runtime 'select * from zone where type==service and status==valid'
matrix query --repo example/payments-api 'select * from upstream'
matrix query --repo example/auth-service 'select * from downstream'
matrix query 'select alias, identity from identity_aliases order by alias limit 25'
```

## Fact Bundles

```bash
matrix members release-bundle.api.1.0.0
matrix deref release-bundle.api.1.0.0
matrix history release-bundle.api.1.0.0
matrix supersedes release-bundle.api.1.0.0 -o json
matrix history release-bundle.api.1.0.0 --relative -1
matrix history release-bundle.api.1.0.0 --as-of 2026-06-19
matrix query 'select component, version, runtime from members where fact_id==release-bundle.api.1.0.0'
```

## Output Formats

```bash
matrix doctor -o json
matrix list -o yaml
matrix query 'select zone, count(*) as facts from facts group by zone' -o csv
matrix query 'select id, zone, status from facts limit 10' -o table
```
