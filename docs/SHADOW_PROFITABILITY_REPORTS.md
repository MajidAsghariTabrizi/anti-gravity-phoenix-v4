# SHADOW Profitability Reports

## Contract

`shadow-profitability-report.sh` runs a read-only, repeatable-read PostgreSQL transaction against `shadow_profitability_report_rows`, selects the newest bounded rows by `(evaluated_at, candidate_key)`, and passes NDJSON to a strict local analyzer. It never starts, recreates, or modifies a service and never reads from fixtures in production.

The limit is required to remain between 1 and 1,000. Input is additionally bounded to 1 MiB per row and 16 MiB total. The analyzer rejects unknown fields, duplicate candidate keys, floating-point finance, malformed integers, inconsistent arithmetic, non-canonical settlement assets, and any row that is not permanently SHADOW-only.

Run the installed operator command as an account permitted to read the production environment files and access Docker:

```sh
sudo /opt/phoenix/deploy/shadow-profitability-report.sh --format text --limit 100
sudo /opt/phoenix/deploy/shadow-profitability-report.sh --format json --limit 100
```

The command consumes `/etc/phoenix/phoenix.env`, `/opt/phoenix/deploy/current-release.env`, and the canonical production Compose file without sourcing, editing, or printing them.

## Sections

The text and JSON formats contain the same twelve deterministic sections:

1. Candidate funnel.
2. Counts by route.
3. Primary and secondary rejection reasons.
4. Profitability distribution by settlement asset.
5. Nearest-to-profitable candidates.
6. Cost breakdown by settlement asset.
7. RPC failure evidence contribution without a causality claim.
8. Stale-state evidence contribution without a causality claim.
9. Route-level expected PnL by settlement asset.
10. Model comparison by settlement asset.
11. Conservative and severe sensitivity by settlement asset.
12. Data-completeness status.

All financial sections are labeled `SHADOW expected` and `not realized`. The report does not call expected, conservative, severe, counterfactual, projected, or fork-simulated values revenue.

## Completeness

Only rows with `evidence_completeness_status=complete` participate in financial calculations. Historical decisions and candidate classifications with missing canonical evidence appear in counts as `incomplete`; their missing values remain null. This preserves the candidate funnel without manufacturing historical economics.

The report is evidence for SHADOW analysis only. It does not prove a route is executable, authorize LIVE mode, create an execution request, or replace real relay, RPC, fork, contract, or settlement verification.
