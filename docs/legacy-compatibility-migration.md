# Legacy Compatibility-Matrix Migration

Use this guide when a repository still dispatches or checks out
`red-wiz/compatibility-matrix` to publish or inspect compatibility facts.

The target shape is:

- **Matrix CLI** for fact upload, ingestion, local SQLite cache, SQL, GraphQL,
  producer coverage, and deterministic troubleshooting.
- **Shared producer action** for Red Wiz CI repositories that should publish
  compatibility facts from GitHub Actions.
- **Wiz repo health** for repo-side governance: whether the producer workflow is
  expected, present, pinned, and using OIDC.

Do not keep direct workflow dispatches to `red-wiz/compatibility-matrix` as the
fact publishing interface.

## Replace Legacy Dispatch

Legacy shape:

```yaml
- name: Submit compatibility facts
  run: |
    gh workflow run emit.yml \
      --repo red-wiz/compatibility-matrix \
      --field platform_compatibility_fact_token="$PLATFORM_COMPATIBILITY_FACT_TOKEN"
```

Preferred Red Wiz CI shape:

```yaml
- uses: red-wiz/submit-compatibility-facts-action/emit@v1.2.0
  with:
    file: matrix-facts.json
```

Generic Matrix CLI shape:

```yaml
- name: Submit compatibility facts
  env:
    MATRIX_CONSTRUCT_URL: https://platform-api.red-wiz.stream
    MATRIX_API_PREFIX: /v1/compatibility
    MATRIX_TOKEN: ${{ secrets.MATRIX_TOKEN }}
  run: matrix upload matrix-facts.json
```

For adapter-backed evidence, publish normalized facts directly:

```bash
matrix ingest junit --file reports/junit.xml \
  --repo "$GITHUB_REPOSITORY" \
  --sha "$GITHUB_SHA" \
  --ref "$GITHUB_REF" \
  --upload
```

## Replace Direct Checkout

Legacy shape:

```yaml
- uses: actions/checkout@v4
  with:
    repository: red-wiz/compatibility-matrix
```

Replacement options:

- Install the released Matrix CLI through Homebrew:

  ```bash
  brew install adrianmross/tap/matrix
  ```

- Download the GitHub Release archive for Linux CI.
- Use the shared producer action when the workflow only needs to publish facts.

Do not vendor Matrix scripts from the old repository. Keep repository-specific
fact generation local, then submit through Matrix or the shared action.

## Replace Lookup Scripts

Legacy lookup scripts usually pulled generated compatibility-matrix views or
queried ad hoc artifacts. Replace them with Matrix commands:

```bash
matrix config use red-wiz
matrix sync --max-facts 10000
matrix producers --repo red-wiz/eos -o json
matrix producers --zone odin --stale-days 7 -o json
matrix query -f examples/queries/current-runtime.sql --repo red-wiz/aphrodite -o json
matrix graphql -f examples/queries/version-for.graphql \
  --var component=eunomia \
  --var for=aphrodite \
  -o json
```

Use [Query examples](query-examples.md) for copyable GraphQL and SQL files.

## Migration Checklist

1. Remove direct `gh workflow run ... --repo red-wiz/compatibility-matrix`
   dispatches.
2. Remove workflow checkouts of `red-wiz/compatibility-matrix`.
3. Pick the publishing path:
   - Red Wiz CI: shared producer action;
   - generic CI: `matrix upload` or `matrix ingest ... --upload`.
4. Ensure facts include explicit producer metadata:
   - `source.repo`;
   - `source.sha`;
   - `source.ref`.
5. Verify the repo-side posture:

   ```bash
   wiz repo health --repo <owner/repo> -v
   ```

6. Verify fact-side readback:

   ```bash
   matrix producers --readback --repo <owner/repo> --audit -o json
   matrix graphql -f examples/queries/producer-coverage.graphql \
     --var limit=25 \
     --var staleDays=7 \
     -o json
   ```

## Representative Migration

Use this shape for a Red Wiz repo that still dispatches the legacy matrix
workflow:

1. Remove the legacy dispatch:

   ```bash
   rg 'compatibility-matrix|workflow run' .github/workflows
   ```

2. Add or keep the shared producer action in the repo workflow:

   ```yaml
   - uses: red-wiz/submit-compatibility-facts-action@<immutable-version>
     with:
       facts: path/to/facts.json
   ```

3. Confirm repo-side governance:

   ```bash
   wiz repo health --repo red-wiz/eos -v
   ```

4. Rerun the producing workflow, then confirm Matrix readback:

   ```bash
   matrix producers --readback --repo red-wiz/eos --audit -o json
   ```

5. Use broad inventory only after per-repo readback passes:

   ```bash
   matrix producers --zone odin --stale-days 7 --audit -o json
   ```

Expected result: Wiz owns the answer to "is this repo wired and governed
correctly?" Matrix owns the answer to "did fresh, valid facts with explicit
producer metadata arrive?"

## Ownership Boundary

Matrix answers whether facts exist, what they say, when they were observed, and
which producer metadata is present. Wiz repo health answers whether a repository
should publish compatibility facts and whether its workflow is governed
correctly.

See [Producer coverage boundary](producer-coverage.md) for the detailed split.
