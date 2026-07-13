import json
import os
import urllib.parse
import urllib.request
from decimal import Decimal

import pandas as pd
import streamlit as st

try:
    import psycopg
except Exception:  # pragma: no cover - dependency is installed in the dashboard image
    psycopg = None


POSTGRES_DSN = os.getenv("POSTGRES_DSN", "postgres://phoenix:phoenix@localhost:5432/phoenix")
METRICS_URL = os.getenv("PROMETHEUS_METRICS_URL", "http://prometheus:9090")


st.set_page_config(page_title="Phoenix Command Center", layout="wide")

SHADOW_FINANCIAL_LABEL = "SHADOW / SIMULATED — NOT REALIZED CAPITAL PNL"


def query(sql: str, params=None) -> pd.DataFrame:
    if psycopg is None:
        return pd.DataFrame()
    try:
        with psycopg.connect(POSTGRES_DSN, connect_timeout=2) as conn:
            return pd.read_sql(sql, conn, params=params)
    except Exception:
        return pd.DataFrame()


def metric_value(expression: str) -> Decimal:
    try:
        encoded = urllib.parse.urlencode({"query": expression})
        with urllib.request.urlopen(f"{METRICS_URL}/api/v1/query?{encoded}", timeout=2) as response:
            payload = json.load(response)
        if payload.get("status") != "success":
            return Decimal("0")
        values = []
        for result in payload.get("data", {}).get("result", []):
            value = Decimal(str(result.get("value", [0, "0"])[1]))
            if value.is_finite():
                values.append(value)
        return sum(values, Decimal("0"))
    except Exception:
        return Decimal("0")


def kpi(label: str, value):
    st.metric(label, value)


st.title("Phoenix Command Center")
st.warning(SHADOW_FINANCIAL_LABEL)

tabs = st.tabs(
    [
        "Command Center",
        "Opportunity Funnel",
        "Live Origins",
        "Shadow Decisions",
        "Executions",
        "Realized PnL",
        "Miss Analysis",
        "Pool State Health",
        "RPC Budget",
        "System Health",
        "Shadow Economics",
        "Shadow Risk",
    ]
)

with tabs[0]:
    classifications = query(
        """
        select classification, count(*) as count
        from shadow_engine_classifications
        group by classification
        order by classification
        """
    )
    c1, c2, c3, c4 = st.columns(4)
    with c1:
        kpi("Engine Inputs", metric_value("phoenix_engine_inputs_received_total"))
    with c2:
        kpi("Candidates", metric_value("phoenix_engine_candidates_total"))
    with c3:
        kpi("Shadow Rejected", metric_value("phoenix_engine_shadow_rejected_total"))
    with c4:
        kpi(
            "Input Throughput (5m)",
            metric_value("sum(rate(phoenix_engine_inputs_processed_total[5m]))"),
        )
    st.dataframe(classifications, use_container_width=True)

with tabs[1]:
    funnel = {
        "Origin Seen": metric_value("feed_normalized_transactions_total"),
        "Engine Input": metric_value("phoenix_engine_inputs_received_total"),
        "Processed": metric_value("phoenix_engine_inputs_processed_total"),
        "Candidate Route": metric_value("phoenix_engine_candidates_total"),
        "Shadow Rejected": metric_value("phoenix_engine_shadow_rejected_total"),
        "Shadow Accepted": metric_value("phoenix_engine_shadow_accepted_total"),
    }
    st.bar_chart(
        pd.DataFrame(
            {
                "stage": list(funnel.keys()),
                "count": [float(value) for value in funnel.values()],
            }
        ),
        x="stage",
        y="count",
    )

with tabs[2]:
    st.dataframe(
        query(
            """
            select tx_hash, sequence_number, classification, router, seen_at
            from origin_transactions
            order by seen_at desc
            limit 200
            """
        ),
        use_container_width=True,
    )

with tabs[3]:
    decisions = query(
        """
        select id, source_event_identity, route_fingerprint, disposition,
               primary_rejection_reason, confidence_bps, execution_eligible,
               base_net_pnl as base_simulated_net_pnl,
               conservative_net_pnl as conservative_simulated_net_pnl,
               severe_net_pnl as severe_simulated_net_pnl,
               decided_at
        from shadow_decisions
        order by decided_at desc
        limit 200
        """
    )
    st.dataframe(decisions, use_container_width=True)

with tabs[4]:
    st.dataframe(
        query(
            """
            select e.tx_hash, e.receipt_status, e.block_number, e.actual_tx_fee_wei,
                   e.settled_event_found, e.reconciled_at
            from executions e
            order by e.reconciled_at desc
            limit 200
            """
        ),
        use_container_width=True,
    )

with tabs[5]:
    st.warning(SHADOW_FINANCIAL_LABEL)
    st.caption("Execution reconciliation is inactive unless a separately reviewed LIVE release exists.")
    realized = query(
        """
        select asset, flash_amount, premium, realized_profit_asset_units as realized_profit,
               actual_tx_fee_wei, actual_ordering_cost_wei, created_at
        from realized_pnl
        order by created_at desc
        limit 200
        """
    )
    st.dataframe(realized, use_container_width=True)

with tabs[6]:
    misses = query(
        """
        select reason, count(*) as count
        from miss_reasons
        group by reason
        order by count desc
        """
    )
    st.dataframe(misses, use_container_width=True)

with tabs[7]:
    health = query(
        """
        select pool, max(block_number) as last_block,
               max(completeness_min_tick) as min_tick,
               max(completeness_max_tick) as max_tick,
               max(created_at) as updated_at
        from pool_state_checkpoints
        group by pool
        order by updated_at desc
        """
    )
    st.dataframe(health, use_container_width=True)

with tabs[8]:
    rpc = {
        "State Requests": metric_value("rpc_state_requests_total"),
        "Upstream Calls": metric_value("rpc_upstream_calls_total"),
        "Route/Block Cache Hits": metric_value("rpc_route_block_cache_hits_total"),
        "Coalesced": metric_value("rpc_coalesced_requests_total"),
        "Provider Rate Limited": metric_value("rpc_provider_rate_limited_total"),
        "State Budget Rejected": metric_value("rpc_state_request_budget_rejected_total"),
        "Upstream Budget Rejected": metric_value("rpc_upstream_call_budget_rejected_total"),
        "Primary Screen Rejected": metric_value("rpc_primary_screen_rejected_total"),
        "Secondary Skipped": metric_value("rpc_secondary_skipped_total"),
    }
    st.dataframe(pd.DataFrame({"metric": list(rpc.keys()), "value": list(rpc.values())}), use_container_width=True)

with tabs[9]:
    system = {
        "Feed Readiness": metric_value("feed_readiness"),
        "JetStream Publish Failures": metric_value("feed_jetstream_publish_failures_total"),
        "Recorder Readiness": metric_value("recorder_readiness"),
        "Dispatcher Readiness": metric_value("shadow_dispatcher_readiness"),
        "Outbox Pending": metric_value("shadow_dispatcher_pending_rows"),
        "Oldest Outbox Age (s)": metric_value(
            "shadow_dispatcher_oldest_pending_age_seconds"
        ),
        "RPC Gateway Readiness": metric_value("rpc_gateway_readiness"),
        "Engine Readiness": metric_value("phoenix_engine_readiness"),
        "Engine Consumer Pending": metric_value("phoenix_engine_consumer_pending"),
        "Engine ACK Pending": metric_value("phoenix_engine_consumer_ack_pending"),
        "Engine Processing Failures": metric_value(
            "phoenix_engine_processing_failures_total"
        ),
        "Hot Path RPC Calls": metric_value("hot_path_external_rpc_calls_total"),
    }
    st.dataframe(pd.DataFrame({"metric": list(system.keys()), "value": list(system.values())}), use_container_width=True)

with tabs[10]:
    st.warning(SHADOW_FINANCIAL_LABEL)
    economics = query(
        """
        select decided_at, route_fingerprint, disposition,
               base_net_pnl as base_simulated_net_pnl,
               conservative_net_pnl as conservative_simulated_net_pnl,
               severe_net_pnl as severe_simulated_net_pnl,
               primary_rejection_reason
        from shadow_decisions
        order by decided_at desc
        limit 200
        """
    )
    st.dataframe(economics, use_container_width=True)

with tabs[11]:
    quality = {
        "Sequence Gaps": metric_value("feed_sequence_gaps_total"),
        "Missing Feed Messages": metric_value("feed_sequence_gap_messages_total"),
        "Decoder Failures": metric_value("feed_decode_failures_total"),
        "Unsupported Messages": metric_value("feed_unsupported_messages_total"),
        "Replay Lag": metric_value("phoenix_replay_lag_seconds"),
        "Engine Candidates": metric_value("phoenix_engine_candidates_total"),
        "No Route": metric_value("phoenix_engine_no_route_total"),
        "Shadow Accepted": metric_value("phoenix_engine_shadow_accepted_total"),
        "Shadow Rejected": metric_value("phoenix_engine_shadow_rejected_total"),
        "Redeliveries": metric_value("phoenix_engine_redeliveries_total"),
        "Duplicate Skips": metric_value("phoenix_engine_duplicate_skips_total"),
        "RPC Disagreements": metric_value("rpc_provider_disagreement_total"),
    }
    st.dataframe(
        pd.DataFrame({"risk_or_quality_metric": list(quality.keys()), "value": list(quality.values())}),
        use_container_width=True,
    )
    rejection_reasons = query(
        """
        select coalesce(primary_rejection_reason, 'unspecified') as rejection_reason,
               count(*) as count
        from shadow_decisions
        where disposition = 'rejected'
        group by coalesce(primary_rejection_reason, 'unspecified')
        order by count desc, rejection_reason
        """
    )
    st.dataframe(rejection_reasons, use_container_width=True)
