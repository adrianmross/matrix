# Producer Coverage Boundary

Matrix producer coverage answers fact-side questions: which producers emitted
compatibility facts, how fresh those facts are, which zones and components they
cover, and whether any facts are invalid. It does not inspect repository
workflow adoption or decide whether a repository should have installed a
producer workflow.

Use this split:

| Question | Owner | Command |
| --- | --- | --- |
| Which repositories have emitted compatibility facts? | Matrix | `matrix producers -o json` |
| Are emitted facts fresh or stale? | Matrix | `matrix producers --stale-days 7` |
| Which zones/components are represented by facts? | Matrix | `matrix producers --zone odin` |
| Are facts missing explicit producer metadata? | Matrix | `matrix producers -o json` |
| Should this repo install the shared compatibility producer workflow? | Wiz repo health | `wiz repo health --repo <owner/repo>` |
| Is the shared action present and pinned correctly? | Wiz repo health | `wiz repo health -v` |
| Is publish auth using OIDC instead of a static token? | Wiz repo health | `wiz repo health -o json` |

Agents can combine both structured outputs when they need an org-level answer:

```bash
matrix producers --zone odin --stale-days 7 -o json
wiz repo health --repo red-wiz/eos -o json
```

## Matrix Output Contract

`matrix producers -o json` returns a stable `producer-inventory` object:

```json
{
  "kind": "producer-inventory",
  "summary": {
    "producers": 2,
    "staleProducers": 0,
    "facts": 42,
    "invalidFacts": 1,
    "sourceRepoFacts": 40,
    "inferredSubjectRepoFacts": 2,
    "unknownProducerFacts": 0,
    "missingProducerMetadataFacts": 2,
    "staleAfterDays": 7
  },
  "rows": [
    {
      "producer": "red-wiz/eos",
      "facts": 24,
      "components": 3,
      "zones": 2,
      "invalid_facts": 0,
      "source_repo_facts": 24,
      "inferred_subject_repo_facts": 0,
      "unknown_producer_facts": 0,
      "producer_metadata": "explicit-source",
      "last_observed_at": "2026-06-28T00:00:00Z",
      "freshness": "fresh"
    }
  ]
}
```

`producer_metadata` is one of:

- `explicit-source`: every grouped fact had explicit source repository metadata.
- `inferred-subject-repo`: Matrix inferred the producer from the subject repo.
- `mixed`: the group includes both explicit and inferred producer facts.
- `unknown`: at least one grouped fact had neither explicit producer metadata
  nor subject repository metadata.

`missingProducerMetadataFacts` is the count repo-health and agents should use
when recommending producer cleanup. It means Matrix could ingest the fact, but
the producer should start sending explicit `source.repo` or
`sourceRepository`.

## Wiz Handoff

Use `wiz repo health` for repo-side adoption and governance. That surface owns
checks such as:

- expected compatibility producer workflow presence;
- `red-wiz/submit-compatibility-facts-action` adoption;
- immutable action pinning;
- GitHub OIDC publish auth;
- legacy direct `red-wiz/compatibility-matrix` workflow dispatches.

Matrix should not duplicate those checks. If producer facts are absent or stale
for a repo, inspect the repo with:

```bash
wiz repo health --repo <owner/repo> -v
```

Then use the Matrix producer output to confirm facts landed after the repo-side
fix is merged and run.
