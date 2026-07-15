# SHADOW Secondary Verification

Phoenix requests independent RPC evidence only after the primary block-pinned
profitability screen meets its configured minimum. A primary result below that
minimum stops before a second provider call and records:

```text
verification_status=primary_only
independent_verification_status=not_requested
verification_skip_reason=primary_screen_no_profitable_candidate
independent_verification_lifecycle=[not_requested]
```

An economically promising primary result records a lifecycle beginning with
`requested`. The terminal independent status is exactly one of:

- `agreed`
- `disagreed`
- `provider_unavailable`
- `integrity_failure`

`requested` is a valid transient status, but it is never accepted as complete
decision evidence. Complete records use `[requested, terminal_status]`.

## Identity Contract

Agreement requires all of the following:

1. The secondary logical provider identifier differs from the primary one.
2. The secondary block number equals the pinned primary block number.
3. The secondary block hash equals the pinned primary block hash.
4. The secondary route configuration hash equals the primary route configuration hash.
5. The secondary state hash equals the primary state hash.

The RPC Gateway excludes the primary provider before selecting a secondary.
The Engine independently validates the response contract. PostgreSQL migration
`009_profit_triggered_secondary_verification.sql` enforces the same provider,
block, route, state, lifecycle, and safety relationships for new records.
Provider identifiers are bounded logical labels; URLs are rejected from
evidence and metrics.

`disagreed` carries the same provider, block, and route identity proof but a
different state hash. `provider_unavailable` carries no secondary identity.
`integrity_failure` is separate from availability and also carries no untrusted
secondary identity.

## Fail-Closed Behavior

Only explicit `agreed` evidence satisfies RPC agreement. Disagreement,
availability failure, integrity failure, stale state, malformed evidence, a
self-verification attempt, or any block/route mismatch rejects the SHADOW
candidate. None of these paths can create an execution request.

The Engine may synthesize a terminal failure record from an already validated
primary response when the verification request itself fails. It copies no
malformed secondary material and labels transport failure as
`provider_unavailable` or response-contract failure as `integrity_failure`.

## Persistence And Reports

New complete profitability facts persist the route hash, secondary block and
route proof when available, terminal status, and full lifecycle. Rows created
before migration 009 retain nulls for these new columns; reports label that
condition as historical evidence instead of inventing a result.

The bounded profitability and positive-route reports expose the explicit
status. They validate same-provider exclusion and block/route equality before
rendering agreement. Reporting remains read-only and uses integer arithmetic.

## Scope

This is SHADOW evidence only. It does not sign, submit, broadcast, simulate an
executor transaction, authorize LIVE mode, or prove provider diversity at the
network/operator level. Logical provider independence and real Arbitrum
behavior still require controlled runtime observation.
