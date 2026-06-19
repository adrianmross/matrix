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
- `matrix-enter`: the interactive SQL shell used by `matrix enter`.
- `matrix-construct`: a generic HTTP service that stores facts and serves the
  Matrix API for teams that want to run their own construct.

The CLI and construct are versioned from the same repository, but they should be
packaged and released separately. Producer/CI environments can install only the
core CLI, end users can add `matrix-enter`, and operators can deploy the
construct service independently.

`matrix enter` is a dispatcher. It starts the `matrix-enter` binary from `PATH`,
or from `MATRIX_ENTER_BIN` when you want to point at a custom location. This
keeps the core CLI small while still giving users the familiar `matrix enter`
command when the shell package is installed.

For automation-only environments, build the core CLI without interactive
dependencies:

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
matrix update --check
matrix config set construct https://matrix.example.dev
matrix list
matrix view sdk-runtime
matrix current --zone sdk-runtime --level preview
matrix gate --zone sdk-runtime --level stage
matrix trace --zone sdk-runtime --subject my-package
matrix upload facts.json
matrix query 'select id, zone, status, subject_name from facts limit 20'
matrix query 'select id, zone, status from facts limit 20' -o json
matrix query --zone sdk-runtime 'select * from zone where type==chaincode'
matrix members smart-contract-tuple.vdr.0.1.0
matrix deref smart-contract-tuple.vdr.0.1.0
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

The plugin skill prefers `-o json` for automation, uses `matrix query` for
read-only SQL over compatibility facts, and uses `matrix upload` or
`matrix ingest --upload` for producer evidence.

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
For tabular query results, `json` and `yaml` emit row arrays. `table`, `human`,
and `csv` use the result columns for rendering, with nested arrays converted to
readable cell text. `MATRIX_OUTPUT` can set the default output format for a shell
or CI step. `-o` / `--out` is a global option, so it works before or after
commands; command-local placement is recommended for readability.

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
- `current`: alias of `active` for context-aware SQL.
- `upstream`: capabilities required by the current context, joined to providers.
- `downstream`: facts that require capabilities provided by the current context.
- `compatible_with_current`: valid downstream facts compatible with the current
  context.
- `components`: flattened component facts.
- `valid_facts`: raw facts whose status is compatible/passed/observed/candidate.
- `invalid_facts`: raw facts whose status is incompatible/failed/invalid/blocked.
- `requirements`: expanded `requires` capability edges.
- `capabilities`: expanded `provides` capability edges.
- `members`: expanded tuple/member entries from facts with `members[]`.
- `deref`: a rolled-up dereference view combining `members`, `requires`, and
  `provides` edges.
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

select edge, target, target_version, physical_chaincode, channel
from deref
where fact_id==smart-contract-tuple.vdr.0.1.0;

select component, version, physical_chaincode, services
from members
where fact_id==smart-contract-tuple.vdr.0.1.0;

select *
from deref
where fact_id==smart-contract-tuple.vdr.0.1.0
  and edge==member;

select * from current;

select current_version, capability, component, version, status
from upstream;

select current_version, capability, component, version, status
from downstream;

select component, version, status
from compatible_with_current;

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

For the common case of dereferencing a single fact, use commands instead of
writing SQL:

```bash
matrix members smart-contract-tuple.vdr.0.1.0
matrix deref smart-contract-tuple.vdr.0.1.0
```

Custom shortcuts can be loaded as SQLite view packs:

```bash
matrix config set sql-init ~/.config/matrix/init.sql
matrix config set sql-pack ~/.config/matrix/packs/odin.sql
matrix config set sql-packs ~/.config/matrix/packs/base.sql,~/.config/matrix/packs/odin.sql
```

```sql
create view vdr_tuple as
select component, version, physical_chaincode, channel
from members
where fact_id = 'smart-contract-tuple.vdr.0.1.0';
```

`sql-init` is the legacy single-file hook. `sql-pack` sets one reusable pack,
and `sql-packs` sets an ordered comma-separated list. Pack files are applied to
the local in-memory query database and may only create views. Use them for
project, team, or org-specific rollups while keeping normal Matrix queries plain
SQL. See `examples/sql-packs/vdr.sql` for a small pack.

## REPL

```bash
matrix enter
```

The REPL lives in the separate `matrix-enter` binary. Install it next to
`matrix`, or point the dispatcher at it explicitly:

```bash
MATRIX_ENTER_BIN=/path/to/matrix-enter matrix enter
```

Use `matrix enter --version <version>` when you want to focus a component
version through the dispatcher. If you invoke `matrix-enter` directly, use
`--target-version <version>` so `matrix-enter --version` remains the binary
version check.

Core automation builds keep commands such as `query`, `upload`, `publish`,
`gate`, `trace`, and `doctor` without linking the interactive shell libraries.

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
matrix> .views
matrix> .examples
matrix> .members smart-contract-tuple.vdr.0.1.0
matrix> .deref smart-contract-tuple.vdr.0.1.0
matrix> .mode json
matrix> .mode yaml
matrix> .refresh
matrix> blue
matrix> red
```

Useful commands:

- `.help` or `/help`: show REPL commands.
- `.status` or `/status`: show construct, cache, output mode, and timing state.
- `.tables`, `.views`, `.schema [table]`, `.describe [table]`: inspect the
  local query model.
- `.examples`: print copyable SQL and helper-command examples.
- `.members <fact-id>`: show tuple members for a fact.
- `.deref <fact-id>`: show member, requirement, and provide edges for a fact.
- `.mode table|json|yaml|csv`: change result rendering.
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

SQL errors print in the shell without exiting the session. `red` exits. `blue`
clears the current local session context.

## Configuration

```bash
matrix config list
matrix config set construct https://matrix.example.dev
matrix config set api-prefix /v1/matrix
matrix config set sql-init ~/.config/matrix/init.sql
matrix config set sql-pack ~/.config/matrix/packs/odin.sql
```

Environment overrides:

- `MATRIX_CONSTRUCT_URL`
- `MATRIX_API_PREFIX`
- `MATRIX_TOKEN`
- `MATRIX_OUTPUT`
- `MATRIX_SQL_INIT`
- `MATRIX_SQL_PACKS`, comma-separated SQL pack paths

## Releases

`matrix --version` prints the installed CLI version. `matrix-enter --version`
prints the interactive shell version. `matrix-construct --version` prints the
service binary version.

`matrix` checks GitHub Releases for a newer version once per day on interactive
startup and prints `matrix update` when one is available. Set
`MATRIX_NO_UPDATE_CHECK=1` to disable the notice. For the GitHub release API, it
uses `MATRIX_GITHUB_TOKEN`, `GITHUB_TOKEN`, or `GH_TOKEN` when one is present.
Homebrew-managed installs update through the Adrian Ross tap:

```bash
brew update
brew upgrade adrianmross/tap/matrix
```

Use `matrix update --check` for a machine-readable release check.

Tags drive releases:

```bash
git tag -a v0.3.7 -m "Release v0.3.7"
git push origin v0.3.7
```

The `Release` workflow builds `matrix`, `matrix-enter`, and `matrix-construct`
for Linux x64, macOS Intel, and macOS Apple Silicon, publishes tarballs, and
uploads SHA-256 checksums to the GitHub Release.

The `Tag Release` workflow is the preferred path for normal releases. Run it
with `version=0.3.7` after bumping the Cargo package versions. It validates
formatting, tests, clippy, version alignment, and tag uniqueness before pushing
the annotated tag.

## Design Notes

`matrix` follows a terminal-first Rust CLI shape similar to modern local
developer tools: a small native binary, explicit config files, JSON output for
automation, and pluggable producer adapters.
