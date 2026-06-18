# matrix

`matrix` is a generic compatibility matrix toolkit for software zones, levels,
facts, gates, and traces.

It is designed for complex release trains where many producers can emit
evidence: test runners, package managers, SBOM tools, supply-chain graph tools,
CI workflows, and custom validators.

## Packages

This repository is a small monorepo with separate distributions:

- `matrix`: the terminal CLI for publishing, querying, gating, and inspecting
  compatibility facts.
- `matrix-construct`: a generic HTTP service that stores facts and serves the
  Matrix API for teams that want to run their own construct.

The CLI and construct are versioned from the same repository, but they should be
packaged and released separately. Producer/CI environments can install only the
CLI, while operators can deploy the construct service independently.

The default CLI build includes the interactive SQL shell (`matrix enter`). For
automation-only environments, build the core CLI without default features to
omit REPL dependencies and hide the interactive command:

```bash
cargo build --release --no-default-features -p matrix
```

## Concepts

- **construct**: the configured Matrix API/server and compatibility graph.
- **zone**: a compatibility domain or train.
- **level**: a promotion lane such as dev, preview, stage, or production.
- **fact**: a normalized compatibility assertion.
- **evidence**: raw proof, output, or links behind a fact.
- **gate**: a policy decision for a zone at a level.
- **trace**: an explanation path through the facts.

## Quickstart

```bash
matrix --version
matrix config set construct https://matrix.example.dev
matrix list
matrix view sdk-runtime
matrix current --zone sdk-runtime --level preview
matrix gate --zone sdk-runtime --level stage
matrix trace --zone sdk-runtime --subject my-package
matrix upload facts.json
matrix query 'select id, zone, status, subject_name from facts limit 20'
matrix query --zone sdk-runtime 'select * from zone where type==chaincode'
matrix completion zsh
```

Run a local construct:

```bash
cargo run -p matrix-construct
matrix --construct http://127.0.0.1:8080 list
```

## Producers

Adapters can feed evidence into Matrix:

```bash
matrix ingest tox --file tox-result.json
matrix ingest nox --file nox-result.json
matrix ingest cibuildwheel --file wheel-report.json
matrix ingest sbom --file bom.cdx.json
```

Add `--upload` to submit the normalized adapter payload to the configured
construct.

## Codex Plugin

This repository can also be installed as a Codex plugin. The plugin does not
bundle a server or credentials; it teaches Codex how to use the local `matrix`
binary against the construct you configure.

```bash
matrix config set construct https://matrix.example.dev
matrix doctor
```

The plugin skill prefers `--json` for automation, uses `matrix query` for
read-only SQL over compatibility facts, and uses `matrix upload` or
`matrix ingest --upload` for producer evidence.

## Context Queries

`matrix query` and `matrix enter` detect the current git repository, branch,
tag, and SHA. Override that context when needed:

```bash
matrix query --zone odin --repo red-wiz/putto \
  'select * from zone where type==chaincode and status!=failed'

matrix enter --zone odin --repo red-wiz/eos
```

The local query engine keeps the raw `facts` table and adds Matrix-native
shortcuts:

- `zone`: facts for the active or inferred zone.
- SQL-safe zone names, such as `odin`: facts for that zone.
- `active`: facts matching the detected or overridden repo/component context.
- `components`: flattened component facts.
- `valid_facts`: raw facts whose status is compatible/passed/observed/candidate.
- `invalid_facts`: raw facts whose status is incompatible/failed/invalid/blocked.
- `requirements`: expanded `requires` capability edges.
- `capabilities`: expanded `provides` capability edges.
- `context`: the detected repo, zone, tag, ref, and SHA.

`component` is the short component key, so `@red-wiz/eos` can be queried as
`component==eos`. Use `subject_name` when you need the exact package or module
name.

Bare values in common filters can be left unquoted, so
`type==chaincode` is normalized to `type = 'chaincode'`.
`status==valid` expands to the compatible status set:
`compatible`, `passed`, `observed`, `candidate`, `valid`, and `ready`.

Examples:

```sql
select * from zone where type==chaincode and status==valid;

select *
from odin
where component==eos and repo==red-wiz/eos and status==failed;

select id, component, repo, status, observed_at, accepted_at
from facts
order by accepted_at desc
limit 25;

select eos.id, eos.component, eos.version
from odin eos
where eos.repo==red-wiz/eos
  and exists (
    select 1
    from requirements r
    where r.fact_id = eos.id
      and r.capability in (
        select p.capability
        from capabilities p
        where p.repo==red-wiz/athena and p.status!=failed
        order by p.version desc
        limit 1
      )
  );
```

## REPL

```bash
matrix enter
```

The REPL is part of the default `interactive` feature. Core automation builds
can omit it with `--no-default-features`; those builds keep commands such as
`query`, `upload`, `publish`, `gate`, `trace`, and `doctor`.

Inside the shell, SQL statements can span multiple lines and execute when they
end with `;`. The REPL keeps a local fact cache for the session, persists
command history, offers tab completion, and uses light SQL highlighting when the
terminal supports it.

```text
matrix> select id, zone, status from facts limit 10;
matrix> .context
matrix> .context set repo red-wiz/putto
matrix> .component eos
matrix> .versions
matrix> .use 1
matrix> .zones
matrix> .describe facts
matrix> .mode json
matrix> .refresh
matrix> blue
matrix> red
```

Useful commands:

- `.help` or `/help`: show REPL commands.
- `.status` or `/status`: show construct, cache, output mode, and timing state.
- `.tables`, `.schema [table]`, `.describe [table]`: inspect the local query
  model.
- `.mode table|json|csv`: change result rendering.
- `.x`: toggle expanded records.
- `.timing`: toggle query timings.
- `.limit <n>`: change the fact fetch limit and refresh the cache.
- `.refresh`: reload facts from the construct.
- `.context`: show the active zone, repo, component, version, tag, ref, and SHA.
- `.context <field> <value>`: set `zone`, `repo`, `component`, `version`,
  `tag`, `sha`, or `ref` without leaving the REPL.
- `.context set <field> <value>`: alias for `.context <field> <value>`.
- `.context auto`: reset to the current git repo/tag/ref/SHA.
- `.context clear [field]`: clear one context field, or all fields.
- `.zone`, `.repo`, `.component`, `.version`, `.tag`, `.sha`, `.ref`: shortcut
  setters for context fields.
- `.components`, `.versions [component]`, `.tags`: list selectable context
  values.
- `.use <pick>`: focus the numbered value from the last `.components`,
  `.versions`, or `.tags` output.
- `.zones`, `.subjects`, `.trace <subject>`: Matrix-native inspection helpers.
- `.gate <zone> [level]`: fetch a gate decision from the construct.
- `.explain <sql>`: run `EXPLAIN QUERY PLAN`.

`red` exits. `blue` clears the current local session context.

## Configuration

```bash
matrix config list
matrix config set construct https://matrix.example.dev
matrix config set api-prefix /v1/matrix
```

Environment overrides:

- `MATRIX_CONSTRUCT_URL`
- `MATRIX_API_PREFIX`
- `MATRIX_TOKEN`

## Releases

`matrix --version` prints the installed CLI version. `matrix-construct
--version` prints the service binary version.

Tags drive releases:

```bash
git tag -a v0.3.1 -m "Release v0.3.1"
git push origin v0.3.1
```

The `Release` workflow builds `matrix` and `matrix-construct` for Linux x64,
macOS Intel, and macOS Apple Silicon, publishes tarballs, and uploads SHA-256
checksums to the GitHub Release.

The `Tag Release` workflow is the preferred path for normal releases. Run it
with `version=0.3.1` after bumping both Cargo package versions. It validates
formatting, tests, clippy, version alignment, and tag uniqueness before pushing
the annotated tag.

## Design Notes

`matrix` follows a terminal-first Rust CLI shape similar to modern local
developer tools: a small native binary, explicit config files, JSON output for
automation, and pluggable producer adapters.
