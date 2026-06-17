# matrix

`matrix` is a compatibility matrix CLI for software zones, levels, facts,
gates, and traces.

It is designed for complex release trains where many producers can emit
evidence: test runners, package managers, SBOM tools, supply-chain graph tools,
CI workflows, and custom validators.

## Concepts

- **oracle**: the configured Matrix API/server.
- **zone**: a compatibility domain or train.
- **level**: a promotion lane such as dev, preview, stage, or production.
- **fact**: a normalized compatibility assertion.
- **evidence**: raw proof, output, or links behind a fact.
- **gate**: a policy decision for a zone at a level.
- **trace**: an explanation path through the facts.

## Quickstart

```bash
matrix config set oracle https://matrix.example.dev
matrix list
matrix view sdk-runtime
matrix current --zone sdk-runtime --level preview
matrix gate --zone sdk-runtime --level stage
matrix trace --zone sdk-runtime --subject my-package
matrix upload facts.json
matrix query 'select id, zone, status, subject_name from facts limit 20'
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
oracle.

## REPL

```bash
matrix enter
```

Inside the shell:

```text
matrix> select id, zone, status from facts limit 10;
matrix> blue
matrix> red
```

`red` exits. `blue` clears the local session context.

## Configuration

```bash
matrix config list
matrix config set oracle https://matrix.example.dev
matrix config set api-prefix /v1/compatibility
```

Environment overrides:

- `MATRIX_ORACLE_URL`
- `MATRIX_API_PREFIX`
- `MATRIX_TOKEN`

## Design Notes

`matrix` follows a terminal-first Rust CLI shape similar to modern local
developer tools: a small native binary, explicit config files, JSON output for
automation, and pluggable producer adapters.
