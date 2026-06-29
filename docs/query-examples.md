# Query Examples

These examples are deterministic starting points for humans, agents, and
scripts. They avoid natural-language handling inside Matrix: translate the
question into one of these explicit commands, GraphQL documents, or SQL files.

Run GraphQL examples with `matrix graphql -f`:

```bash
matrix graphql -f examples/queries/aphrodite-eunomia-path.graphql \
  --var from=aphrodite \
  --var to=eunomia \
  -o json

matrix graphql -f examples/queries/putto-aphrodite-works-with.graphql \
  --var left=putto \
  --var right=aphrodite \
  -o json

matrix graphql -f examples/queries/version-for.graphql \
  --var component=eunomia \
  --var for=aphrodite \
  -o json

matrix graphql -f examples/queries/component-status.graphql \
  --var component=aphrodite \
  -o json

matrix graphql -f examples/queries/producer-coverage.graphql \
  --var limit=25 \
  --var staleDays=7 \
  -o json
```

Run SQL examples against the local SQLite fact cache with `matrix query -f`:

```bash
matrix query -f examples/queries/eos-chaincode-members.sql --offline -o table
matrix query -f examples/queries/chaincode-athena-compatibility.sql --offline -o json
matrix query -f examples/queries/current-runtime.sql --repo red-wiz/aphrodite -o json
```

Use `matrix sync` first when you want fully offline iteration:

```bash
matrix sync --max-facts 10000
matrix graphql -f examples/queries/version-for.graphql \
  --var component=eunomia \
  --var for=aphrodite \
  --offline \
  -o json
```

## Common Questions

| Question | Command |
| --- | --- |
| Which Putto can work with an Aphrodite version? | `matrix works-with putto aphrodite -o json` |
| Why are Aphrodite and Eunomia connected? | `matrix graphql -f examples/queries/aphrodite-eunomia-path.graphql --var from=aphrodite --var to=eunomia -o json` |
| What version of Eunomia is Aphrodite using? | `matrix graphql -f examples/queries/version-for.graphql --var component=eunomia --var for=aphrodite -o json` |
| What is connected to Aphrodite right now? | `matrix graphql -f examples/queries/component-status.graphql --var component=aphrodite -o json` |
| Which producers are stale or missing metadata? | `matrix graphql -f examples/queries/producer-coverage.graphql --var limit=25 --var staleDays=7 -o json` |
| Which chaincode members are in an EOS bundle? | `matrix query -f examples/queries/eos-chaincode-members.sql --offline -o table` |
| Which chaincode facts connect to Athena? | `matrix query -f examples/queries/chaincode-athena-compatibility.sql --offline -o json` |
| What does my current repo context see? | `matrix query -f examples/queries/current-runtime.sql --repo red-wiz/aphrodite -o json` |

For REPL workflows, save any of these into the Matrix snippet directory with
`.save`, or open the repository copy directly:

```text
matrix> .graphql -f examples/queries/version-for.graphql --var component=eunomia --var for=aphrodite
matrix> .read examples/queries/eos-chaincode-members.sql
```
