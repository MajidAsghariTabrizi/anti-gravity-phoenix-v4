# PRE-LIVE Money-Path Observability

This contract observes the bounded SHADOW path from Nitro feed receipt through route discovery,
profitability evaluation, durable persistence, independent RPC verification, and isolated fork
evidence. It does not create execution eligibility or a transaction request.

Safety is invariant in every generated report:

- `PHOENIX_MODE=SHADOW`
- `LIVE_EXECUTION=false`
- `execution_eligible=false`
- `execution_request_created=false`

## Runtime Metrics

Feed metrics distinguish complete transport messages, normalized transactions, official protocol
kind handling, ignored and unsupported bounded numeric kinds, sequence gaps, missing sequence count,
reconnects, recent gap time, and data completeness. Numeric Nitro kinds are constrained to the fixed
L1/L2 range `0..255`; payload text is never a label.

Engine metrics cover configured route matches, candidate count, no-route outcomes, discovery
eligibility, fixed ranking exclusions, primary profitability status, near-profitable observations,
fixed PnL buckets, safe gas totals, retry recovery, quarantine progress, processing and persistence
latency, pending durable work, and fixed runtime exit classes.

RPC Gateway metrics cover request and upstream budgets, fixed method/outcome/provider-slot calls,
primary success, provider availability and rate limiting, independent verification requested,
agreed, disagreed, and unavailable outcomes, state freshness, and fixed latency buckets.

Recorder and Shadow Dispatcher metrics cover database retries and actual recovery, persistence
latency, JetStream pending and ACK-pending work, redelivery, fetch and acknowledgement failures,
outbox backlog age, publish retry recovery, and terminal integrity failures.

Fork simulation counts, pass/revert outcomes, simulated profitability, prediction error, and gas
utilization come from `fork_simulation_results`. The fork sandbox is intentionally isolated and
one-shot, so these are read-only database aggregates rather than a false production scrape target.

Prometheus enforces `sample_limit: 2048`, at most eight labels, and bounded label lengths for every
money-path service. No metric label may contain a transaction hash, arbitrary address, source event
identity, provider URL, or unbounded route identifier.

## Bounded Report

Run the installed report from the production deploy directory:

```sh
/opt/phoenix/deploy/prelive-money-path-report.sh --format text --window-hours 24
/opt/phoenix/deploy/prelive-money-path-report.sh --format json --window-hours 24
```

The wrapper first validates the production SHADOW environment and digest-pinned Compose context.
It then executes `prelive-money-path-report.sql` in a repeatable-read, read-only transaction and
queries only loopback Prometheus. Windows are bounded to `1..168` hours, rejection reasons to the
reviewed enum, Prometheus input to 2,048 series, input/output to 2 MiB, and database output to
aggregates over indexed time ranges.

The machine report uses `phoenix.prelive.money-path-summary.v1`, explicitly labels runtime counters
as `process_lifetime`, and validates against
`schemas/prelive-money-path-summary.schema.json`. It has separate technical and business sections,
uses canonical integer or decimal strings, and contains no hashes, addresses, routes, source
identities, provider identities, instances, or URLs. Missing scrape evidence, an unsafe safety
field, an unknown label, a schema mismatch, or unavailable PostgreSQL/Prometheus evidence fails the
report without claiming readiness.

## Interpretation

Prometheus counters are process-lifetime observations and may reset after a restart. The SQL window
is the durable aggregate source for profitability, RPC quality, outbox, and fork facts. Compare both
surfaces and the release SHA before drawing an operational conclusion.

Expected, conservative, severe, and fork-simulated PnL remain counterfactual SHADOW evidence.
Positive counts do not prove realized profit, production execution safety, or PRE-LIVE readiness.
