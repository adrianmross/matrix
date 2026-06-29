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

## Start With Examples

```bash
matrix examples list
matrix examples show version-for
matrix examples run version-for -o json
matrix examples run aphrodite-eunomia-path -o json
matrix examples run producer-coverage -o json
matrix examples run resolve-component -o json
```

## Query The Construct

```bash
matrix list
matrix scopes
matrix scope odin/native-askar
matrix view runtime
matrix current --zone runtime --level preview
matrix gate --zone runtime --level stage
matrix trace --zone runtime --subject payments-api
```

## Answer Compatibility Questions

```bash
matrix path aphrodite eunomia
matrix works-with putto aphrodite
matrix compatible aphrodite putto
matrix versions eunomia --for aphrodite
matrix why aphrodite eunomia
matrix status aphrodite
matrix resolve aphrodite
matrix graphql -f examples/queries/resolve-component.graphql --var name=red-wiz/eunomia -o json
```

For agents and scripts:

```bash
matrix path aphrodite eunomia -o json
matrix works-with putto aphrodite -o json
matrix versions eunomia --for aphrodite -o json
matrix graphql --schema
matrix graphql '{ path(from:"aphrodite", to:"eunomia") { status paths { nodes { component version } } } }' -o json
matrix graphql -f examples/queries/aphrodite-eunomia-path.graphql --var from=aphrodite --var to=eunomia -o json
matrix graphql 'query VersionFor($component:String!,$for:String!) { versions(component:$component, for:$for) { versions } }' --var component=eunomia --var for=aphrodite -o json
```

## Work Fast With A Local Snapshot

```bash
matrix sync --max-facts 10000
matrix cache status
matrix query 'select id, zone, status from facts limit 20' --offline
matrix path aphrodite eunomia --offline
matrix graphql -f examples/queries/aphrodite-eunomia-path.graphql --var from=aphrodite --var to=eunomia --offline -o json
```

Use this when you are iterating on saved SQL/GraphQL files, demoing without a
network dependency, or giving an agent a stable SQLite fact cache. Run
`matrix sync` again when you want fresh facts. Normal SQL and graph commands use
fresh local cache hits automatically and refresh on miss or stale cache; force a
network refresh with `--refresh-cache` or configure the default with
`matrix config set cache-policy auto`.

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
matrix why payments-api ledger-service
```

## Answer Compatibility Questions

Use these before writing SQL:

```bash
matrix path aphrodite eunomia
matrix works-with putto aphrodite
matrix versions eunomia --for aphrodite
matrix why aphrodite eunomia
matrix producers --zone odin
matrix producers --zone odin --audit -o json
matrix producers --readback --repo red-wiz/aphrodite --audit -o json
```

Graph answers rank paths and show confidence. Add `-o json` for agents and
scripts.

## Publish Producer Evidence

```bash
matrix upload facts.json
matrix publish facts.json
matrix ingest tox --file tox-result.json
matrix ingest tox --file tox-result.json --junit-glob '.tox/*/junit.xml'
matrix ingest nox --file nox-result.json
matrix ingest nox --file nox-result.json --junit-file reports/junit.xml
matrix ingest junit --file junit.xml
matrix ingest sbom --file bom.cdx.json
matrix ingest k6 --file summary.json --zone stage
matrix ingest microcks --file test-result.json --zone stage
matrix ingest sbom --file bom.cdx.json --upload
matrix upload examples/producers/fact-batch.json
```

## SQL

```bash
matrix query 'select id, zone, status from facts limit 20'
matrix query --zone runtime 'select * from zone where type==service and status==valid'
matrix query --repo example/payments-api 'select * from upstream'
matrix query --repo example/auth-service 'select * from downstream'
matrix query 'select alias, identity from identity_aliases order by alias limit 25'
matrix query -f examples/queries/current-runtime.sql -o json
```

## Fact Bundles

```bash
matrix get release-bundle.api.1.0.0
matrix get release-bundle.api.1.0.0 --relative -1
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
