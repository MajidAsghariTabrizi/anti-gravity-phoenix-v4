import os
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


def query(sql: str, params=None) -> pd.DataFrame:
    if psycopg is None:
        return pd.DataFrame()
    try:
        with psycopg.connect(POSTGRES_DSN, connect_timeout=2) as conn:
            return pd.read_sql(sql, conn, params=params)
    except Exception:
        return pd.DataFrame()


def metric_value(name: str) -> Decimal:
    try:
        with urllib.request.urlopen(f"{METRICS_URL}/api/v1/query?query={name}", timeout=2) as response:
            payload = response.read().decode("utf-8")
        marker = '"value":['
        if marker not in payload:
            return Decimal("0")
        value_part = payload.split(marker, 1)[1].split("]", 1)[0].split(",", 1)[1]
        return Decimal(value_part.strip().strip('"'))
    except Exception:
        return Decimal("0")


def kpi(label: str, value):
    st.metric(label, value)


st.title("Phoenix Command Center")

tabs = st.tabs(
    [
        "Command Center",
        "Opportunity Funnel",
        "Live Origins",
        "Arbitrage Opportunities",
        "Executions",
        "Realized PnL",
        "Miss Analysis",
        "Pool State Health",
        "RPC Budget",
        "System Health",
    ]
)

with tabs[0]:
    pnl = query("select coalesce(sum(realized_profit_asset_units), 0) as realized_pnl from realized_pnl")
    attempts = query("select status, count(*) as count from execution_attempts group by status order by status")
    c1, c2, c3, c4 = st.columns(4)
    with c1:
        kpi("Realized PnL", pnl.iloc[0]["realized_pnl"] if not pnl.empty else "0")
    with c2:
        kpi("Capture Rate", metric_value("opportunities_settled_total"))
    with c3:
        kpi("Median Decision Latency", metric_value("decision_latency_seconds"))
    with c4:
        kpi("P95 Origin To Submission", metric_value("origin_to_submission_latency_seconds"))
    st.dataframe(attempts, use_container_width=True)

with tabs[1]:
    funnel = {
        "Origin Seen": metric_value("feed_transactions_total"),
        "Supported": metric_value("supported_origins_total"),
        "Affected Route": metric_value("affected_routes_total"),
        "Simulated": metric_value("route_simulations_total"),
        "Profitable": metric_value("profitable_opportunities_total"),
        "Submitted": metric_value("opportunities_submitted_total"),
        "Receipt Success": metric_value("execution_receipt_success_total"),
        "Settled": metric_value("opportunities_settled_total"),
        "Realized Profit": metric_value("realized_profit_total"),
    }
    st.bar_chart(pd.DataFrame({"stage": list(funnel.keys()), "count": list(funnel.values())}), x="stage", y="count")

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
    opportunities = query(
        """
        select id, route_id, lifecycle_state, flash_asset, optimized_amount,
               expected_gross_profit as theoretical_profit,
               expected_net_profit,
               created_at
        from opportunities
        order by created_at desc
        limit 200
        """
    )
    st.dataframe(opportunities, use_container_width=True)

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
        "Requests": metric_value("rpc_requests_total"),
        "Provider Requests": metric_value("rpc_provider_requests_total"),
        "Cache Hits": metric_value("rpc_cache_hits_total"),
        "Coalesced": metric_value("rpc_coalesced_requests_total"),
        "Rate Limited": metric_value("rpc_rate_limit_total"),
        "Circuit Open": metric_value("rpc_circuit_open_total"),
        "Budget Rejected": metric_value("rpc_budget_rejected_total"),
    }
    st.dataframe(pd.DataFrame({"metric": list(rpc.keys()), "value": list(rpc.values())}), use_container_width=True)

with tabs[9]:
    system = {
        "Pools Tracked": metric_value("pools_tracked"),
        "Pools Complete": metric_value("pools_complete"),
        "Pools Incomplete": metric_value("pools_incomplete"),
        "State Reconciliation Age": metric_value("state_reconciliation_age_seconds"),
        "Hot Path RPC Calls": metric_value("hot_path_external_rpc_calls_total"),
    }
    st.dataframe(pd.DataFrame({"metric": list(system.keys()), "value": list(system.values())}), use_container_width=True)

