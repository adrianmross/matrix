# Matrix GraphQL Reference

Matrix GraphQL is a deterministic query surface for agents and scripts. It is
not a natural-language parser: translate user intent into explicit fields,
variables, and selections before calling `matrix graphql`.

Print the embedded schema from any install:

```bash
matrix graphql --schema
```

Run a document from a file:

```bash
matrix graphql -f examples/queries/version-for.graphql \
  --var component=eunomia \
  --var for=aphrodite \
  -o json
```

## Root Fields

### `path`

Find an inferred compatibility path between two components.

```graphql
query Path($from: String!, $to: String!) {
  path(from: $from, to: $to, limit: 3) {
    status
    found
    confidence
    paths {
      confidence
      nodes { component version repo }
      edges { relationship capability status sourceFactId }
    }
  }
}
```

### `worksWith`

Check whether two components have a known compatible connection.

```graphql
query WorksWith($left: String!, $right: String!) {
  worksWith(left: $left, right: $right, limit: 3) {
    compatible
    direction
    confidence
    reasons
    paths { nodes { component version } }
  }
}
```

### `status`

Show incoming and outgoing graph edges for a component.

```graphql
query Status($component: String!) {
  status(component: $component, limit: 10) {
    component { component version repo }
    outgoingCount
    incomingCount
    outgoing { relationship to { component version } }
    incoming { relationship from { component version } }
  }
}
```

### `versions`

Find versions of one component used by or connected to another component.

```graphql
query VersionFor($component: String!, $for: String!) {
  versions(component: $component, for: $for, limit: 5) {
    versions
    versionCandidates { version confidence score pathCount }
  }
}
```

### `resolve`

Explain how Matrix resolves a name, repo, package, or component alias.

```graphql
query Resolve($name: String!) {
  resolve(name: $name) {
    requested
    name
    ambiguous
    matchCount
    resolved { component version repo }
    matches { aliasKinds node { component version repo } }
    warnings
  }
}
```

```bash
matrix graphql '{ resolve(name:"red-wiz/eunomia") { name resolved { component version repo } } }' -o json
```

### `producers`

Summarize fact-side producer presence, freshness, validity, and metadata.

```graphql
query Producers($limit: Int!, $staleDays: Int!) {
  producers(limit: $limit, staleDays: $staleDays) {
    summary {
      producers
      facts
      staleProducers
      invalidFacts
      missingProducerMetadataFacts
    }
    rows {
      producer
      facts
      components
      freshness
      producer_metadata
      last_observed_at
    }
  }
}
```

For a single producer readback after `wiz repo health`, use the CLI command:

```bash
wiz repo health --repo red-wiz/aphrodite -v
matrix producers --readback --repo red-wiz/aphrodite --audit -o json
```

## Variables

Pass variables as repeated `--var name=value` flags. Matrix parses integer and
boolean variable values when the GraphQL argument expects `Int` or `Boolean`;
otherwise values are treated as strings.

```bash
matrix graphql \
  'query Producers($limit:Int!,$staleDays:Int!){ producers(limit:$limit, staleDays:$staleDays){ summary { producers facts } } }' \
  --var limit=10 \
  --var staleDays=14 \
  -o json
```

Missing variables fail before execution with a deterministic error such as
`missing GraphQL variable $component`.

## Aliases And Projection

Aliases are preserved under `data`, and selections project the response shape.
Fields omitted from the selection are omitted from the output.

```graphql
{
  aphroditeToEunomia: path(from: "aphrodite", to: "eunomia", limit: 1) {
    status
    paths { nodes { component version } }
  }
}
```

Structured output always returns:

```json
{
  "kind": "graphql-result",
  "data": {}
}
```

When a cache-backed command runs, JSON/YAML output also includes a `cache`
object with the source, age, freshness, and digest metadata.

## Error Handling

Matrix rejects unsupported root fields and unsupported selections explicitly.
Examples:

- `unsupported GraphQL root field`
- `missing GraphQL variable $component`
- `unsupported GraphQL field`

Agents should surface those errors directly and retry with a corrected explicit
query rather than asking Matrix to infer intent.
