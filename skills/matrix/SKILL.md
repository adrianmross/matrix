---
name: matrix
description: Use the Matrix CLI to inspect compatibility zones, query facts, evaluate gates, trace evidence, publish producer results, or work with a configured Matrix construct.
---

# Matrix

Use this skill when a user asks about compatibility matrices, release trains,
zones, promotion levels, gates, facts, traces, producer evidence, or the
`matrix` CLI.

## Tooling

- Prefer the local `matrix` binary.
- Use `matrix --help` and command-specific `--help` output to confirm current
  flags before building unfamiliar commands.
- Use `-o json` when you need to parse command output or summarize exact
  fields. `-o` / `--out` is global, so prefer command-local placement such as
  `matrix doctor -o json`.
- Do not invent a construct URL. Use `MATRIX_CONSTRUCT_URL`, `matrix config get
  construct`, a built-in profile such as `matrix config use red-wiz`, or a
  user-provided `--construct` value.
- Do not print, persist, or echo tokens. Matrix uses `MATRIX_TOKEN`,
  `MATRIX_TOKEN_FILE`, `MATRIX_TOKEN_COMMAND`, or saved config token sources for
  bearer authentication.

## Common Workflows

- Show configured state: `matrix config list`
- Use the hosted Red Wiz compatibility construct: `matrix config use red-wiz`
- Install shell completions: `matrix completion <shell>`
- Check construct health: `matrix doctor`
- List graph capabilities: `matrix capabilities`
- List providers for a capability: `matrix providers <capability>`
- List artifacts or validations: `matrix artifacts --track <track>` or
  `matrix validations --track <track>`
- Inspect an artifact: `matrix requirements <artifact-id>` or
  `matrix consumers <artifact-id>`
- Inspect promotion blockers: `matrix blockers <track> --environment <env>` or
  `matrix eligibility <track> <env>`
- List zones: `matrix list`
- View a zone: `matrix view <zone>`
- Check current repo compatibility:
  `matrix current --zone <zone> --level <level>`
- Check a specific tag or SHA:
  `matrix current --zone <zone> --level <level> --tag <tag>` or
  `matrix current --zone <zone> --level <level> --sha <sha>`
- Evaluate a promotion gate:
  `matrix gate --zone <zone> --level <level>`
- Trace evidence:
  `matrix trace --zone <zone> --subject <subject>`
- Query facts:
  `matrix query 'select id, zone, status from facts limit 20'`
- Publish facts:
  `matrix upload facts.json`
- Normalize producer evidence:
  `matrix ingest <adapter> --file <path>`
- Normalize and publish producer evidence:
  `matrix ingest <adapter> --file <path> --upload`

## SQL Guidance

`matrix query` builds a local read-only SQLite view over fetched construct facts.
Only `SELECT` and `WITH` statements are allowed. The `facts` table includes:

- `id`
- `zone`
- `kind`
- `status`
- `source_repository`
- `source_sha`
- `subject_type`
- `subject_name`
- `channel`
- `observed_at`
- `accepted_at`
- `json`

Use the `json` column for fields that have not been promoted into first-class
columns yet.

The local engine also exposes `active`, `zone`, `zones`, `subjects`,
`components`, `identities`, `identity_aliases`, `valid_facts`,
`invalid_facts`, `requirements`, `capabilities`, and one SQL-safe view per
zone such as `runtime`. In status filters,
`status==valid` expands to the compatible status set:
`compatible`, `passed`, `observed`, `candidate`, `valid`, and `ready`.

## Producer Adapters

Use `matrix ingest` for producer tools such as test runners, build matrices,
SBOM generators, package managers, supply-chain graph tools, or custom CI
validators. Adapter names are intentionally plain strings so new producers can
start emitting facts before a specialized adapter exists. Prefer supported
adapters when available: `junit`, `sbom`, `tox`, `nox`, `k6`, and `microcks`.
For tox/nox, prefer attached JUnit reports with `--junit-file` or
`--junit-glob`; tox/nox facts should describe runner/session orchestration, not
duplicate the JUnit test-case model. Use `--upload` only when the configured
construct and credentials are known.

## Interactive Mode

Use `matrix enter` when the user wants an interactive SQL session. In the REPL,
`red` exits and `blue` clears local session context. Use `.context repo
example/payments-api` or `.context set repo example/payments-api` to change
context without leaving the REPL.
