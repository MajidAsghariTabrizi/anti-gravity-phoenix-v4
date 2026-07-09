# Opportunity Funnel

Primary funnel:

`Origin Seen -> Supported -> Affected Route -> Simulated -> Profitable -> Submitted -> Receipt Success -> Settled -> Realized Profit`

Required metrics:

- `feed_normalized_transactions_total`
- `supported_origins_total`
- `affected_routes_total`
- `route_simulations_total`
- `profitable_opportunities_total`
- `opportunities_submitted_total`
- `execution_receipt_success_total`
- `opportunities_settled_total`
- `realized_profit_total`

Latency histograms:

- `feed_ingest_latency_seconds`
- `origin_decode_latency_seconds`
- `route_lookup_latency_seconds`
- `simulation_latency_seconds`
- `optimizer_latency_seconds`
- `decision_latency_seconds`
- `sign_latency_seconds`
- `submission_latency_seconds`
- `origin_to_submission_latency_seconds`

Profit labels remain separate:

- theoretical profit
- expected net profit
- realized profit
