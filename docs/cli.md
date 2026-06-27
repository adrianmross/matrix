# Matrix CLI

The `matrix` CLI publishes, queries, gates, and inspects compatibility facts
from a configured construct.

## Output

Matrix defaults to human-friendly terminal output. Use `-o` / `--out` when a
script or another tool needs a structured format:

```bash
matrix doctor
matrix doctor -o json
matrix list -o yaml
matrix query 'select zone, count(*) as facts from facts group by zone' -o csv
matrix query 'select id, zone, status from facts limit 10' -o table
```

Supported output formats are `human`, `json`, `yaml`, `table`, and `csv`.
For tabular query results, `human` emits compact readable records, `json` and
`yaml` emit row arrays, and `table` / `csv` use the result columns for
rendering. Nested arrays are converted to readable cell text in
terminal-oriented formats.

`MATRIX_OUTPUT` can set the default output format for a shell or CI step. `-o`
/ `--out` is a global option, so it works before or after commands;
command-local placement is recommended for readability.

## Hosted Constructs

Configure the hosted Red Wiz compatibility construct with the built-in profile:

```bash
wiz auth login
matrix config use red-wiz
matrix doctor -o json
```

That stores `https://platform-api.red-wiz.stream` with the `/v1/compatibility`
API prefix, so the CLI talks to platform-api as the public construct while the
internal compatibility-service and MySQL ledger stay behind it.
It also configures `wiz auth token --audience platform-api --format json` as
the credential command and clears saved Matrix token/token-file values so old
bearer tokens do not override the profile handoff. `wiz tool setup matrix` is
the beginner wrapper for setup and smoke guidance; Matrix stores the narrower
credential command for per-request auth.

Token commands inherit the Matrix process environment. If you set
`XDG_CONFIG_HOME` to isolate Matrix during a smoke test, that also changes where
`wiz auth token` looks for its login config. Either run `wiz auth login` inside
the same temporary config home, or override the command for that test:

```bash
MATRIX_TOKEN_COMMAND='env -u XDG_CONFIG_HOME wiz auth token --audience platform-api --format json' \
  XDG_CONFIG_HOME="$(mktemp -d)/config" \
  matrix doctor
```

For non-profile deployments, provide a bearer token with one of these sources:

```bash
MATRIX_TOKEN=... matrix upload facts.json
MATRIX_TOKEN_FILE=~/.config/matrix/red-wiz.token matrix capabilities
matrix config set token-file ~/.config/matrix/red-wiz.token
matrix config set token-command 'op read op://platform/matrix/token'
```

Token values are used only for the outgoing `Authorization: Bearer` header and
are not printed by `matrix config list` or `matrix doctor`.

## Compatibility Graph Commands

For common compatibility questions, use the answer-oriented graph commands
first. Matrix infers known paths from `requires`, `provides`, and tuple
`members`; you do not need to spell out an intermediate bundle such as EOS when
the facts already describe that path.

```bash
matrix path aphrodite eunomia
matrix works-with putto aphrodite
matrix compatible aphrodite putto
matrix versions eunomia --for aphrodite
matrix why aphrodite eunomia
matrix status aphrodite
matrix resolve aphrodite
```

Examples of the questions these answer:

```bash
# What version of Eunomia is Aphrodite using?
matrix versions eunomia --for aphrodite

# Which Putto facts connect to Aphrodite?
matrix works-with putto aphrodite

# Why does Matrix think Aphrodite and Eunomia are connected?
matrix why aphrodite eunomia

# What is connected to Aphrodite right now?
matrix status aphrodite

# What did Matrix actually match when I typed Aphrodite?
matrix resolve aphrodite
```

For agents and scripts, every command supports structured output:

```bash
matrix path aphrodite eunomia -o json
matrix works-with putto aphrodite -o json
matrix versions eunomia --for aphrodite -o json
```

Matrix also accepts a small GraphQL-style query surface for agent promptability
and script readability. Selection sets are accepted so the command can look
like GraphQL, while Matrix still returns the standard JSON answer shape.

```bash
matrix graphql '{ path(from:"aphrodite", to:"eunomia") { status paths { nodes { component version } } } }' -o json
matrix graphql '{ worksWith(left:"putto", right:"aphrodite") { status paths { edges { capability } } } }' -o json
matrix graphql '{ versions(component:"eunomia", for:"aphrodite") { versions } }' -o json
matrix graphql -f queries/aphrodite-path.graphql -o json
matrix graph 'aphrodite -> eunomia' -o json
```

Use the lower-level API projection commands when you need exact construct
objects rather than an inferred answer:

When the configured construct exposes the `/v1/compatibility` API, Matrix can
read the graph projections directly:

```bash
matrix capabilities
matrix scopes
matrix scope odin/native-askar
matrix providers smart-contract-tuple:vdr
matrix artifacts --track odin --subject-type smart-contract-tuple
matrix validations --track odin --status failed
matrix requirements smart-contract-tuple.vdr.0.1.1
matrix consumers smart-contract-tuple.vdr.0.1.1
matrix blockers odin --environment stage
matrix eligibility odin stage
```

`matrix upload facts.json` and `matrix ingest <adapter> --upload` still submit
normalized facts to `POST /facts` under the configured API prefix. With the Red
Wiz profile that resolves to `POST /v1/compatibility/facts`.

## Local Fact Cache

Use `matrix sync` when you want fast repeated exploration, offline-friendly
agent runs, or a stable local SQLite database while you iterate on SQL and graph
queries:

```bash
matrix sync --max-facts 10000
matrix cache status
matrix query 'select id, zone, status from facts limit 20' --offline
matrix query -f queries/current-runtime.sql --offline -o json
matrix path aphrodite eunomia --offline
matrix works-with putto aphrodite --offline
matrix why aphrodite eunomia --offline
matrix resolve aphrodite --offline
matrix graphql -f queries/aphrodite-path.graphql --offline -o json
matrix cache clear
```

The cache is a SQLite database stored under Matrix's OS cache directory and is
keyed by the active profile, construct URL, and API prefix. Cache metadata
records the construct, API prefix, profile, schema version, fetch time, fact
count, and `--max-facts` used to populate the database. `matrix cache status -o
json` reports the cache path, file size, age, and whether the database is older
than Matrix's freshness hint.

Online local SQL and graph commands fetch from the construct and refresh the
cache. Add `--offline` to open only the persisted SQLite database. Add
`--refresh-cache` when you want the command invocation to make the refresh intent
explicit.

## Context Queries

`matrix query` and `matrix enter` detect the current git repository, branch,
tag, and SHA. Override that context when needed:

```bash
matrix query --zone runtime --repo example/payments-api \
  'select * from zone where type==service and status!=failed'

matrix query -f queries/current-runtime.sql -o json
matrix query -f queries/current-runtime.sql --offline -o json

matrix enter --zone runtime --repo example/ledger-service
```

The local query engine keeps the raw `facts` table and adds Matrix-native
shortcuts:

- `zone`: facts for the active or inferred zone.
- SQL-safe zone names, such as `runtime`: facts for that zone.
- `active`: facts matching the detected or overridden repo/component context.
- `current`: alias of `active` for context-aware SQL.
- `upstream`: capabilities required by the current context, joined to providers.
- `downstream`: facts that require capabilities provided by the current context.
- `compatible_with_current`: valid downstream facts compatible with the current
  context.
- `components`: flattened component facts.
- `identities`: canonical subject identities derived from facts.
- `identity_aliases`: aliases that resolve to canonical subject identities.
- `valid_facts`: raw facts whose status is compatible/passed/observed/candidate.
- `invalid_facts`: raw facts whose status is incompatible/failed/invalid/blocked.
- `requirements`: expanded `requires` capability edges.
- `capabilities`: expanded `provides` capability edges.
- `members`: expanded tuple/member entries from facts with `members[]`.
- `deref`: a rolled-up dereference view combining `members`, `requires`, and
  `provides` edges.
- `context`: the detected repo, zone, tag, ref, and SHA.

`component` is the short component key, so `@example/ledger-service` can be
queried as `component==ledger-service`. Use `subject_name` when you need the
exact package or module name. Use `identity` and `identity_aliases` when you
need canonical matching across repos, package names, and aliases.

Bare values in common filters can be left unquoted, so `type==service` is
normalized to `type = 'service'`. `status==valid` expands to the compatible
status set: `compatible`, `passed`, `observed`, `candidate`, `valid`, and
`ready`.

## SQL Examples

```sql
select * from zone where type==service and status==valid;

select *
from runtime
where component==ledger-service and repo==example/ledger-service and status==failed;

select id, component, repo, status, observed_at, accepted_at
from facts
order by accepted_at desc
limit 25;

select * from current;

select current_version, capability, component, version, status
from upstream;

select current_version, capability, component, version, status
from downstream;

select component, version, status
from compatible_with_current;

select alias, identity, alias_kind
from identity_aliases
where alias==ledger-service;

select service.id, service.component, service.version
from runtime service
where service.repo==example/ledger-service
  and exists (
    select 1
    from requirements r
    where r.fact_id = service.id
      and r.capability in (
        select p.capability
        from capabilities p
        where p.repo==example/auth-service and p.status!=failed
        order by p.version desc
        limit 1
      )
  );
```

## Fact Dereferencing

Some facts describe a tuple, bundle, or aggregate of other members. Use
`members` and `deref` to inspect those records:

```bash
matrix get release-bundle.api.1.0.0
matrix members release-bundle.api.1.0.0
matrix deref release-bundle.api.1.0.0
matrix history release-bundle.api.1.0.0
```

```sql
select component, version, runtime, platform
from members
where fact_id==release-bundle.api.1.0.0;

select edge, target, target_version, runtime, platform
from deref
where fact_id==release-bundle.api.1.0.0;
```

## Fact History

Facts use stable IDs for logical compatibility records. When a producer submits
the same ID again, the construct keeps the latest body in `facts` and appends an
immutable accepted event. Use `get` to read the current or selected body, and
`history` to inspect the audit list:

```bash
matrix get release-bundle.api.1.0.0
matrix get release-bundle.api.1.0.0 --relative -1
matrix history release-bundle.api.1.0.0
matrix supersedes release-bundle.api.1.0.0 -o json
```

The default output shows revision number, accepted time, content hash, source
repository/SHA/ref, and whether each revision is `current` or `superseded`.
Structured output returns the construct event objects, including `eventId`,
`factId`, `revision`, `acceptedAt`, `contentHash`, `supersededBy`,
`supersededAt`, and the preserved `fact` body.

Select one revision when you do not need the full audit list:

```bash
matrix history release-bundle.api.1.0.0 --revision 2
matrix history release-bundle.api.1.0.0 --relative -1
matrix history release-bundle.api.1.0.0 --revision 3 --relative -1
matrix history release-bundle.api.1.0.0 --event event.abc123 --relative -1
matrix history release-bundle.api.1.0.0 --event event.abc123
matrix history release-bundle.api.1.0.0 --as-of 2026-06-19
matrix history release-bundle.api.1.0.0 --as-of 2026-06-19T16:00:00Z
```

`--relative -1` means one revision before current by default. Use
`--revision` or `--event` with `--relative` to make the offset relative to a
different base revision.

Producer guidance:

- Reuse `fact.id` for the same logical assertion or tuple across updates.
- Change `fact.id` when the assertion is a new logical record.
- Treat construct `eventId` values as immutable audit IDs assigned by the
  server, not as producer-supplied IDs.

## Producer Ingest

`matrix ingest` converts common producer outputs into normalized Matrix facts.
By default the command prints a fact batch; add `--upload` to submit the facts
to the configured construct.
For custom capability, requirement, consumer, or provenance facts, use the
[producer onboarding guide](producer-onboarding.md).

```bash
matrix ingest junit --file junit.xml
matrix ingest sbom --file bom.cdx.json
matrix ingest tox --file tox-result.json --upload
matrix ingest tox --file tox-result.json --junit-glob '.tox/*/junit.xml' --upload
matrix ingest nox --file nox-result.json --junit-file reports/junit.xml --zone test
matrix ingest k6 --file summary.json --zone stage
matrix ingest microcks --file test-result.json --zone stage
```

Supported adapters:

- `junit`: emits validation facts for JUnit test suites and members for test
  cases.
- `sbom`: emits a root SBOM fact and package/dependency facts for CycloneDX or
  SPDX JSON.
- `tox` / `nox`: emit orchestration facts for environments or sessions. Attach
  JUnit files with `--junit-file` or `--junit-glob` when you want the actual
  test cases in the same upload.
- `k6`: emits load-test evidence and marks failed thresholds as failed facts.
- `microcks`: emits API contract-test evidence from JSON test results.

Use these context flags when the input does not carry enough identity:

```bash
matrix ingest junit --file junit.xml \
  --repo example/payments-api \
  --component payments-api \
  --version 1.2.3 \
  --sha "$GITHUB_SHA" \
  --ref "$GITHUB_REF" \
  --upload
```

For tox/nox, prefer JUnit for the detailed test model and keep tox/nox as the
runner/session layer:

```bash
matrix ingest tox --file tox-result.json \
  --junit-glob '.tox/*/junit.xml' \
  --repo example/payments-api \
  --component payments-api \
  --version 1.2.3 \
  --upload
```

The adapters use `test` as the default zone for test-stage evidence and
`supply-chain` for SBOM evidence. Override `--zone` when those facts belong to
a specific train such as `dev`, `stage`, `production`, or a team-defined
compatibility zone.

## Context-Aware Views

Use built-in commands instead of remembering view names:

```bash
matrix components --zone runtime
matrix versions payments-api --repo example/payments-api
matrix components --repo example/payments-api --all
matrix components --repo example/web-client --type npm-dependency
matrix tags --repo example/payments-api
matrix upstream --repo example/payments-api --version v1.6.3
matrix downstream --repo example/auth-service --version v2.1.0
matrix compatible --repo example/payments-api --version v1.6.3
matrix compare example/ledger-service --repo example/payments-api --version v1.6.3
matrix why payments-api ledger-service
```

By default, component and version browsing hides repo-level `application` SBOM
subjects and dependency-only subjects such as `@scope/core`. Use `--all`,
`--type`, `--include-applications`, or `--include-dependencies` when you need
the full evidence inventory.

`compare` compares the current context to a target component, subject, or repo.
It reports both directions: facts where the current context requires the target,
and facts where the target requires something provided by the current context.
Use `--target-version` to pin the target side.

`why`, `path`, `works-with`, and pair-form `compatible` answer between two
components directly. They infer intermediate bundles from Matrix facts, so a
query such as `matrix why aphrodite eunomia` can show the Aphrodite to EOS to
Eunomia evidence path without a `--through eos` flag.

## SQL Packs

Custom shortcuts can be loaded as SQLite view packs:

```bash
matrix config set sql-init ~/.config/matrix/init.sql
matrix config set sql-pack ~/.config/matrix/packs/release.sql
matrix config set sql-packs ~/.config/matrix/packs/base.sql,~/.config/matrix/packs/release.sql
```

```sql
create view api_bundle as
select component, version, runtime, platform
from members
where fact_id = 'release-bundle.api.1.0.0';
```

`sql-init` is the legacy single-file hook. `sql-pack` sets one reusable pack,
and `sql-packs` sets an ordered comma-separated list. Pack files are applied as
session-local SQLite temp views and may only create views. Use them for project,
team, or org-specific rollups while keeping normal Matrix queries plain SQL. See
`examples/sql-packs/release-bundle.sql` for a small pack.
