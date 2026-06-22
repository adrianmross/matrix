# matrix

`matrix` is a generic compatibility matrix toolkit for software zones, release
levels, facts, gates, traces, and dependency-aware queries.

It is designed for complex release trains where many producers emit evidence:
test runners, package managers, SBOM tools, supply-chain graph tools, CI
workflows, and custom validators.

## Packages

This repository is a small monorepo with separate distributions:

- `matrix`: the terminal CLI for publishing, querying, gating, and inspecting
  compatibility facts.
- `matrix-enter`: the interactive SQL shell used by `matrix enter`.
- `matrix-construct`: a generic HTTP service that stores facts and serves the
  Matrix API for teams that want to run their own construct.

The CLI and construct are versioned from the same repository, but they are
packaged independently. Producer/CI environments can install only the core CLI,
end users can add `matrix-enter`, and operators can deploy the construct
service separately.

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
matrix view runtime
matrix current --zone runtime --level preview
matrix gate --zone runtime --level stage
matrix trace --zone runtime --subject payments-api
matrix upload facts.json
matrix query 'select id, zone, status, subject_name from facts limit 20'
matrix history release-bundle.api.1.0.0
matrix enter
```

Run a local construct:

```bash
cargo run -p matrix-construct
matrix --construct http://127.0.0.1:8080 list
```

## Documentation

- [CLI guide](docs/cli.md)
- [Popular commands](docs/cli-popular-cmds.md)
- [Interactive REPL](docs/cli-repl.md)

## Producers

Adapters can feed evidence into Matrix:

```bash
matrix ingest tox --file tox-result.json
matrix ingest nox --file nox-result.json
matrix ingest junit --file junit.xml
matrix ingest tox --file tox-result.json --junit-glob '.tox/*/junit.xml'
matrix ingest nox --file nox-result.json --junit-file reports/junit.xml
matrix ingest sbom --file bom.cdx.json
matrix ingest k6 --file summary.json
matrix ingest microcks --file test-result.json
```

Adapters emit normalized Matrix fact batches with stable fields such as
`zone`, `status`, `subject`, `requires`, `provides`, and `members`. Add
`--upload` to submit the normalized facts to the configured construct. Use
`--zone`, `--repo`, `--component`, `--version`, `--sha`, and `--ref` to override
the context detected from git or the input file.

For tox and nox, Matrix treats the runner output as environment/session
orchestration evidence. Attach JUnit reports with `--junit-file` or
`--junit-glob` for the canonical test-case facts.

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

## Configuration

```bash
matrix config list
matrix config set construct https://matrix.example.dev
matrix config set api-prefix /v1/matrix
matrix config set sql-pack ~/.config/matrix/packs/release.sql
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
git tag -a v0.3.11 -m "Release v0.3.11"
git push origin v0.3.11
```

The `Release` workflow builds `matrix`, `matrix-enter`, and `matrix-construct`
for Linux x64, macOS Intel, and macOS Apple Silicon, publishes tarballs, and
uploads SHA-256 checksums to the GitHub Release. Each target archive is
extracted on its build runner before upload, and the packaged `matrix`,
`matrix-enter`, and `matrix-construct` binaries must print the release version.
The publish job verifies all checksums again and includes the checksum summary
in the release notes.

The `Tag Release` workflow is the preferred path for normal releases. Run it
with `version=0.3.11` after bumping the Cargo package versions. It validates
formatting, tests, clippy, version alignment, and tag uniqueness before pushing
the annotated tag.

After updating the Homebrew tap formula, run the `Homebrew Validation` workflow
with the same version. It taps `adrianmross/tap`, audits and installs the
`matrix` formula on macOS, checks all three binary versions, verifies shell
completion generation, and runs a non-network-success `matrix doctor` command.

## Design Notes

`matrix` follows a terminal-first Rust CLI shape similar to modern local
developer tools: a small native binary, explicit config files, JSON output for
automation, and pluggable producer adapters.
