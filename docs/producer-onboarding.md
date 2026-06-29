# Matrix Producer Onboarding

Use this guide when a repository needs to publish compatibility evidence to a
Matrix construct. A producer should emit stable, repeatable facts from CI rather
than one-off human notes.

## Producer Checklist

1. Pick the producer owner.
   - Usually the repository that built, tested, released, or scanned the
     artifact should publish the fact.
   - Do not make a downstream consumer publish facts for an upstream artifact it
     did not observe.

2. Pick stable fact IDs.
   - Reuse the same `id` for the same logical assertion across reruns.
   - Include the component/artifact identity and assertion type in the ID.
   - Do not include construct `eventId` values in producer IDs; the server
     assigns those for audit history.

3. Pick the fact shape.
   - Use `matrix ingest` for common producer outputs such as JUnit, SBOM, tox,
     nox, k6, or Microcks.
   - Use a custom fact batch for capability/provider, requirement/consumer, and
     release tuple assertions.

4. Attach evidence.
   - Keep large logs, SBOMs, provenance, and reports in artifact storage.
   - Put URLs, digests, workflow run IDs, and artifact names in `evidence`.
   - Do not inline large reports in the fact body.

5. Publish from CI.
   - For Red Wiz CI, use `MATRIX_CONSTRUCT_URL`,
     `MATRIX_API_PREFIX=/v1/compatibility`, and a secret-backed `MATRIX_TOKEN`
     or runner-local `MATRIX_TOKEN_COMMAND`.
   - For local Red Wiz use, run `wiz auth login` and `matrix config use red-wiz`.
   - Always include explicit producer metadata through `source.repo` or
     `sourceRepository`. Matrix can fall back to the subject repo for older
     facts, but `matrix producers -o json` reports those as
     `missingProducerMetadataFacts`.

## Minimum Fact Fields

Custom facts should include these fields:

```json
{
  "id": "validation.example-api.ci",
  "zone": "test",
  "kind": "validation",
  "status": "passed",
  "subjectType": "service",
  "subjectName": "example-api",
  "subjectVersion": "1.2.3",
  "subjectRepo": "example/example-api",
  "source": {
    "repo": "example/example-api",
    "ref": "refs/heads/main",
    "sha": "0123456789abcdef"
  },
  "observedAt": "2026-06-26T19:00:00Z",
  "evidence": [
    {
      "type": "github-actions",
      "url": "https://github.com/example/example-api/actions/runs/123"
    }
  ]
}
```

Use these optional arrays when the fact participates in compatibility joins:

- `provides`: capabilities this artifact/component provides.
- `requires`: capabilities this artifact/component needs.
- `members`: artifacts/components included in a tuple, release bundle, or SBOM.
- `aliases`: alternate names that should resolve to the same subject identity.

See [examples/producers/fact-batch.json](../examples/producers/fact-batch.json)
for a complete custom batch with validation, capability provider,
requirement/consumer, and SBOM/provenance-style facts.

## Common Producer Commands

Normalize and inspect facts without publishing:

```bash
matrix ingest junit --file reports/junit.xml \
  --repo example/example-api \
  --component example-api \
  --version "$VERSION" \
  --sha "$GITHUB_SHA" \
  --ref "$GITHUB_REF"
```

Publish normalized producer output:

```bash
matrix ingest junit --file reports/junit.xml \
  --repo example/example-api \
  --component example-api \
  --version "$VERSION" \
  --sha "$GITHUB_SHA" \
  --ref "$GITHUB_REF" \
  --upload
```

Publish a custom fact batch:

```bash
matrix upload matrix-facts.json
```

Query what landed:

```bash
matrix producers --repo example/example-api -o json

matrix query --repo example/example-api \
  'select id, zone, type, status from active order by observed_at desc limit 20'

matrix query --repo example/example-api \
  'select capability, capability_version, status from capabilities order by capability'

matrix query --repo example/example-api \
  'select capability, capability_version, status from requirements order by capability'
```

## GitHub Actions Pattern

Start from
[examples/producers/github-actions-matrix-facts.yml](../examples/producers/github-actions-matrix-facts.yml).

The example uses:

- `MATRIX_CONSTRUCT_URL=https://platform-api.red-wiz.stream`
- `MATRIX_API_PREFIX=/v1/compatibility`
- `MATRIX_TOKEN` from a GitHub secret
- `matrix ingest junit ... --upload`
- `matrix upload matrix-facts.json`

Keep the token secret scoped to publishing compatibility facts. Do not print the
token, commit it into the repository, or embed it in generated fact files.
