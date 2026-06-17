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
matrix config set construct https://matrix.example.dev
matrix list
matrix view sdk-runtime
matrix current --zone sdk-runtime --level preview
matrix gate --zone sdk-runtime --level stage
matrix trace --zone sdk-runtime --subject my-package
matrix upload facts.json
matrix query 'select id, zone, status, subject_name from facts limit 20'
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

## REPL

```bash
matrix enter
```

Inside the shell, SQL statements can span multiple lines and execute when they
end with `;`. The REPL keeps a local fact cache for the session, persists
command history, offers tab completion, and uses light SQL highlighting when the
terminal supports it.

```text
matrix> select id, zone, status from facts limit 10;
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

## Design Notes

`matrix` follows a terminal-first Rust CLI shape similar to modern local
developer tools: a small native binary, explicit config files, JSON output for
automation, and pluggable producer adapters.
