# Matrix REPL

`matrix enter` starts the interactive SQL shell. The shell lives in the
separate `matrix-enter` binary so automation-only installs can keep the core CLI
small.

```bash
matrix enter
```

Install `matrix-enter` next to `matrix`, or point the dispatcher at it
explicitly:

```bash
MATRIX_ENTER_BIN=/path/to/matrix-enter matrix enter
```

Use `matrix enter --version <version>` when you want to focus a component
version through the dispatcher. If you invoke `matrix-enter` directly, use
`--target-version <version>` so `matrix-enter --version` remains the binary
version check.

## Session Basics

SQL statements can span multiple lines and execute when they end with `;`. The
REPL follows the configured cache policy on startup, opens the SQLite fact cache
for the session, persists command history, offers tab completion, and uses light
SQL highlighting when the terminal supports it.

```text
matrix> select id, zone, status from facts limit 10;
matrix> .context
matrix> .context set repo example/payments-api
matrix> .component ledger-service
matrix> .versions
matrix> .use 1
matrix> .zones
matrix> .describe facts
matrix> .views
matrix> .examples
matrix> .members release-bundle.api.1.0.0
matrix> .deref release-bundle.api.1.0.0
matrix> .compare ledger-service
matrix> .path aphrodite eunomia
matrix> .works-with putto aphrodite
matrix> .why aphrodite eunomia
matrix> .resolve aphrodite
matrix> .producers
matrix> .read examples/queries/current-runtime.sql
matrix> .graphql --schema
matrix> .graphql --var component=eunomia --var for=aphrodite query Matrix($component:String!,$for:String!) { versions(component:$component, for:$for) { versions } }
matrix> .graphql -f examples/queries/aphrodite-eunomia-path.graphql --var from=aphrodite --var to=eunomia
matrix> .explain graph aphrodite -> eunomia
matrix> .save aphrodite-path { path(from:"aphrodite", to:"eunomia") { status paths { confidence nodes { component version } } } }
matrix> .open aphrodite-path
matrix> .snippets
matrix> .mode json
matrix> .mode yaml
matrix> .refresh
matrix> .offline
matrix> blue
matrix> red
```

SQL errors print in the shell without exiting the session. `red` exits. `blue`
clears the current local session context.

## Commands

- `.help` or `/help`: show REPL commands.
- `.status` or `/status`: show construct, cache, output mode, and timing state.
- `.tables`, `.views`, `.schema [table]`, `.describe [table]`: inspect the
  local query model.
- `.examples`: print copyable SQL and helper-command examples.
- `.get <fact-id>`: show the current or selected revision of a fact.
- `.members <fact-id>`: show tuple members for a fact.
- `.deref <fact-id>`: show member, requirement, and provide edges for a fact.
- `.history <fact-id>`: show accepted revisions and supersession metadata for a
  fact.
- `.compare <target>`: compare the active context to a target component,
  subject, or repo.
- `.path <from> <to>`, `.works-with <a> <b>`, `.why <a> <b>`: answer graph
  compatibility questions from the session fact cache.
- `.resolve <name>`: show how a repo, package, identity, or short component name
  resolves into graph nodes.
- `.graph <query>` or `.graphql <query>`: run native GraphQL documents or legacy
  graph shorthand from the session fact cache.
- `.graph -f <file>` or `.graphql -f <file>`: run a graph query file.
- `.graphql --schema`: print the native Matrix GraphQL schema.
- `.graphql --var <name=value> <query>`: bind a native GraphQL variable. Values
  parse as JSON when possible and fall back to strings.
- `.explain graph <query>`: show how Matrix parsed a native GraphQL document or
  legacy graph shorthand, plus the result keys or recommendation summary.
- `.save <name> <query>`: save a SQL or graph query snippet under the Matrix
  config directory. Names without an extension get `.graphql` for native
  GraphQL documents or graph shorthand and `.sql` otherwise.
- `.open <name>` or `.run <name>`: run a saved snippet. GraphQL snippets accept
  `--var <name=value>` and `--limit <n>`.
- `.snippets` or `.queries`: list saved snippets and their on-disk path.
- `.producers` or `.coverage`: summarize fact producers, coverage, invalid
  facts, and freshness.
- `.read <file>`, `.load <file>`, or `.source <file>`: run a saved SQL query
  file against the session cache.
- `.mode human|table|json|yaml|csv`: change result rendering.
- `.x`: toggle expanded records.
- `.timing`: toggle query timings.
- `.limit <n>`: change the fact fetch limit and refresh the cache.
- `.refresh`: reload facts from the construct.
- `.offline`: reopen the local persistent SQLite cache without contacting the
  construct.
- `.context`: show the active zone, repo, component, version, tag, ref, and SHA.
- `.context <field> <value>`: set `zone`, `repo`, `component`, `version`,
  `tag`, `sha`, or `ref` without leaving the REPL.
- `.context set <field> <value>`: alias for `.context <field> <value>`.
- `.context auto`: reset to the current git repo/tag/ref/SHA.
- `.context clear [field]`: clear one context field, or all fields.
- `.zone`, `.repo`, `.component`, `.version`, `.tag`, `.sha`, `.ref`: shortcut
  setters for context fields.
- `.components`, `.versions [component]`, `.tags`: list selectable context
  values. `.components` and `.versions` accept `--all`, `--type <type>`,
  `--include-applications`, and `--include-dependencies`.
- `.use <pick>`: focus the numbered value from the last `.components`,
  `.versions`, or `.tags` output.
- `.zones`, `.subjects`, `.trace <subject>`: Matrix-native inspection helpers.
- `.gate <zone> [level]`: fetch a gate decision from the construct.
- `.explain <sql>`: run `EXPLAIN QUERY PLAN`.

For copyable GraphQL and SQL files, see [Query examples](query-examples.md).

## Useful GraphQL

Native GraphQL is useful for agents and scripts because it is structured,
projectable, and accepts variables:

```text
.graphql --schema

.graphql --var component=eunomia --var for=aphrodite query Matrix($component:String!,$for:String!) {
  versions(component:$component, for:$for) {
    versions
    versionCandidates { version confidence score }
  }
}

.graphql {
  aphroditeToEunomia: path(from:"aphrodite", to:"eunomia", limit:1) {
    status
    paths { confidence nodes { component version } }
  }
}

.explain graph aphrodite -> eunomia
```

Save repeated queries as snippets:

```text
.save aphrodite-path { path(from:"aphrodite", to:"eunomia") { status paths { confidence nodes { component version } } } }
.open aphrodite-path
.snippets
```

## Useful SQL

```sql
select * from current;

select current_version, capability, component, version, status
from upstream;

select current_version, capability, component, version, status
from downstream;

select component, version, runtime, platform
from members
where fact_id==release-bundle.api.1.0.0;

select edge, target, target_version, runtime, platform
from deref
where fact_id==release-bundle.api.1.0.0;
```

History comes from the construct audit endpoint rather than the local session
cache:

```text
.get release-bundle.api.1.0.0
.get release-bundle.api.1.0.0 --relative -1
.history release-bundle.api.1.0.0
.history release-bundle.api.1.0.0 --relative -1
.history release-bundle.api.1.0.0 --as-of 2026-06-19
```
