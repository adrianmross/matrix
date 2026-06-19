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

## Context Queries

`matrix query` and `matrix enter` detect the current git repository, branch,
tag, and SHA. Override that context when needed:

```bash
matrix query --zone runtime --repo example/payments-api \
  'select * from zone where type==service and status!=failed'

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
immutable accepted event. Use history commands to inspect prior accepted bodies:

```bash
matrix history release-bundle.api.1.0.0
matrix supersedes release-bundle.api.1.0.0 -o json
```

The default output shows revision number, accepted time, content hash, source
repository/SHA/ref, and whether each revision is `current` or `superseded`.
Structured output returns the construct event objects, including `eventId`,
`factId`, `revision`, `acceptedAt`, `contentHash`, `supersededBy`,
`supersededAt`, and the preserved `fact` body.

Producer guidance:

- Reuse `fact.id` for the same logical assertion or tuple across updates.
- Change `fact.id` when the assertion is a new logical record.
- Treat construct `eventId` values as immutable audit IDs assigned by the
  server, not as producer-supplied IDs.

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
matrix why ledger-service --repo example/payments-api --version v1.6.3
```

By default, component and version browsing hides repo-level `application` SBOM
subjects and dependency-only subjects such as `@scope/core`. Use `--all`,
`--type`, `--include-applications`, or `--include-dependencies` when you need
the full evidence inventory.

`compare` and `why` compare the current context to a target component, subject,
or repo. They report both directions: facts where the current context requires
the target, and facts where the target requires something provided by the
current context. Use `--target-version` to pin the target side.

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
and `sql-packs` sets an ordered comma-separated list. Pack files are applied to
the local in-memory query database and may only create views. Use them for
project, team, or org-specific rollups while keeping normal Matrix queries plain
SQL. See `examples/sql-packs/release-bundle.sql` for a small pack.
