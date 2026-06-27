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

## Installation

On macOS or Linux with Homebrew:

```bash
brew tap adrianmross/tap
brew install adrianmross/tap/matrix
```

From source:

```bash
cargo install --locked --git https://github.com/adrianmross/matrix matrix
```

Linux x86_64 direct installs can use the GitHub Release archive. The Linux
archive is statically linked so it does not require the release runner's glibc
version on the install host:

```bash
MATRIX_VERSION="$(gh release view --repo adrianmross/matrix --json tagName -q .tagName)"
gh release download "$MATRIX_VERSION" --repo adrianmross/matrix --pattern "matrix-${MATRIX_VERSION#v}-x86_64-unknown-linux-gnu.tar.gz" --dir /tmp/matrix-install
tar -xzf "/tmp/matrix-install/matrix-${MATRIX_VERSION#v}-x86_64-unknown-linux-gnu.tar.gz" -C /tmp/matrix-install
install "/tmp/matrix-install/matrix-${MATRIX_VERSION#v}-x86_64-unknown-linux-gnu/matrix" ~/.cargo/bin/matrix
install "/tmp/matrix-install/matrix-${MATRIX_VERSION#v}-x86_64-unknown-linux-gnu/matrix-enter" ~/.cargo/bin/matrix-enter
install "/tmp/matrix-install/matrix-${MATRIX_VERSION#v}-x86_64-unknown-linux-gnu/matrix-construct" ~/.cargo/bin/matrix-construct
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

Use the Red Wiz hosted compatibility construct through platform-api:

```bash
wiz auth login
matrix config use red-wiz
matrix doctor
matrix capabilities
matrix scopes
matrix scope odin/native-askar
matrix providers smart-contract-tuple:vdr
matrix artifacts --track odin --subject-type smart-contract-tuple
matrix requirements smart-contract-tuple.vdr.0.1.1
matrix consumers smart-contract-tuple.vdr.0.1.1
matrix blockers odin --environment stage
matrix eligibility odin stage
```

The `red-wiz` profile stores the hosted construct URL, the `/v1/compatibility`
API prefix, and a Wiz token command. Matrix asks `wiz` for the current token at
request time instead of saving a bearer token in the Matrix config.

Run a local construct:

```bash
cargo run -p matrix-construct
matrix --construct http://127.0.0.1:8080 list
```

## Documentation

- [CLI guide](docs/cli.md)
- [Popular commands](docs/cli-popular-cmds.md)
- [Producer onboarding](docs/producer-onboarding.md)
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

For custom producer facts, start with the
[producer onboarding guide](docs/producer-onboarding.md) and the copyable
[fact batch example](examples/producers/fact-batch.json).

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
matrix config use red-wiz
matrix config set token-file ~/.config/matrix/red-wiz.token
matrix config set token-command 'op read op://platform/matrix/token'
matrix config set sql-pack ~/.config/matrix/packs/release.sql
```

Environment overrides:

- `MATRIX_CONSTRUCT_URL`
- `MATRIX_API_PREFIX`
- `MATRIX_PROFILE`, for example `red-wiz`
- `MATRIX_TOKEN`
- `MATRIX_TOKEN_FILE`
- `MATRIX_TOKEN_COMMAND`
- `MATRIX_OUTPUT`
- `MATRIX_SQL_INIT`
- `MATRIX_SQL_PACKS`, comma-separated SQL pack paths

`matrix config use red-wiz` configures Matrix to call
`wiz auth token --audience platform-api --format json` when it needs a bearer
token.
It also clears any saved Matrix `token` or `token-file` value so stale bearer
tokens do not override the profile handoff.
For other platforms, use `MATRIX_TOKEN`, `MATRIX_TOKEN_FILE`, or
`MATRIX_TOKEN_COMMAND`. Token commands can print a raw token, JSON with an
`access_token` field, or JSON with a `token`, `bearerToken`, or `bearer_token`
field.

Use `matrix doctor -o json` to inspect the active profile, construct URL, API
prefix, redacted auth source, token-command health, and construct reachability.

## Releases

`matrix --version` prints the installed CLI version. `matrix-enter --version`
prints the interactive shell version. `matrix-construct --version` prints the
service binary version.

`matrix` checks GitHub Releases for a newer version once per day on interactive
startup and prints `matrix update` when one is available. Set
`MATRIX_NO_UPDATE_CHECK=1` to disable the notice. For the GitHub release API, it
does not need a token for public releases. Set `MATRIX_GITHUB_TOKEN` for a
Matrix-specific install token, or `GITHUB_TOKEN` for generic GitHub automation
and higher API rate limits.

`matrix update` delegates to Homebrew for Homebrew-managed installs and updates
Linux x86_64 direct installs from the release archive when possible:

```bash
brew update
brew upgrade adrianmross/tap/matrix
```

For Linux direct installs, override the target binary path when the running
binary is not the path you want to replace:

```bash
matrix update --install-path /home/me/bin/matrix
```

For source installs, update with:

```bash
cargo install --locked --git https://github.com/adrianmross/matrix matrix --force
```

Use `matrix update --check` for a machine-readable release check.

Tags drive releases:

```bash
git tag -a v0.3.17 -m "Release v0.3.17"
git push origin v0.3.17
```

The `Release` workflow builds `matrix`, `matrix-enter`, and `matrix-construct`
for Linux x64, macOS Intel, and macOS Apple Silicon, publishes tarballs, and
uploads SHA-256 checksums to the GitHub Release. Linux x64 binaries are
statically linked with musl so direct installs are not tied to the release
runner's glibc version. Each target archive is extracted on its build runner
before upload, and the packaged `matrix`,
`matrix-enter`, and `matrix-construct` binaries must print the release version.
The publish job verifies all checksums again and includes the checksum summary
in the release notes. The workflow also builds and smokes the
`matrix-construct` container image. Publishing releases push
`ghcr.io/adrianmross/matrix-construct:<version>`.

The `Tag Release` workflow is the preferred path for normal releases. Run it
with `version=0.3.17` after bumping the Cargo package versions. It validates
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
