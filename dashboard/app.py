from __future__ import annotations

import os
from datetime import timedelta
from decimal import Decimal
from pathlib import Path
from typing import Any, Iterable

import pandas as pd
import streamlit as st

try:
    from dashboard.snapshot_model import (
        DEFAULT_SNAPSHOT_PATH,
        MAX_LOG_ROWS,
        DashboardSnapshot,
        SnapshotError,
        load_snapshot,
        read_artifact,
    )
except ModuleNotFoundError:  # pragma: no cover - Streamlit script-path execution
    from snapshot_model import (  # type: ignore[no-redef]
        DEFAULT_SNAPSHOT_PATH,
        MAX_LOG_ROWS,
        DashboardSnapshot,
        SnapshotError,
        load_snapshot,
        read_artifact,
    )


SHADOW_FINANCIAL_LABEL = "SHADOW / SIMULATED - NOT REALIZED CAPITAL PNL"
SNAPSHOT_PATH_VARIABLE = "PHOENIX_DASHBOARD_SNAPSHOT_PATH"


def kpi(label: str, value: Any) -> None:
    st.metric(label, value)


def _rows(value: Iterable[dict[str, Any]]) -> pd.DataFrame:
    return pd.DataFrame(list(value))


def _chart_rows(rows: Iterable[dict[str, Any]], numeric: Iterable[str]) -> pd.DataFrame:
    frame = _rows(rows)
    for column in numeric:
        if column in frame:
            frame[column] = pd.to_numeric(frame[column], errors="coerce")
    return frame


def _metric_grid(items: list[tuple[str, Any]], width: int = 4) -> None:
    for offset in range(0, len(items), width):
        columns = st.columns(width)
        for column, item in zip(columns, items[offset : offset + width]):
            with column:
                kpi(item[0], item[1] if item[1] is not None else "Unavailable")


def _projection(value: str, sample_count: Decimal) -> str:
    return value if sample_count > 0 else "Unavailable"


def _percent_from_bps(value: str | None) -> str:
    if value is None:
        return "Unavailable"
    return f"{Decimal(value) / Decimal(100):f}%"


def _load_current_snapshot() -> DashboardSnapshot:
    configured = os.getenv(SNAPSHOT_PATH_VARIABLE)
    path = Path(configured) if configured else DEFAULT_SNAPSHOT_PATH
    return load_snapshot(path)


def _render_alerts(snapshot: DashboardSnapshot) -> None:
    if not snapshot.alerts:
        st.success("No active alert conditions in the current bounded snapshot")
        return
    critical = sum(item["severity"] in {"critical", "high"} for item in snapshot.alerts)
    if critical:
        st.error(f"{critical} blocking alert condition(s)")
    else:
        st.warning(f"{len(snapshot.alerts)} review condition(s)")
    st.dataframe(_rows(snapshot.alerts), width="stretch", hide_index=True)


def _render_overview(snapshot: DashboardSnapshot) -> None:
    data = snapshot.data
    safety = data["safety"]
    governance = data["governance"]
    business = data["business"]
    summary = data["profitability"]["summary"]
    funnel = {row["stage"]: row["count"] for row in data["funnel"]}
    sample_count = Decimal(business["sample_count"])
    fork_sample_count = Decimal(data["fork"]["simulations"])

    _metric_grid(
        [
            ("Operating mode", safety["mode"]),
            ("LIVE flag", str(safety["live_execution"]).lower()),
            ("Pre-LIVE lock", "locked" if safety["prelive_lock"] else "open"),
            ("Gate summary", snapshot.gate_status),
            ("Realized PnL", "0"),
            (
                "Expected PnL (counterfactual)",
                _projection(summary["expected_net_pnl"], sample_count),
            ),
            (
                "Conservative PnL (counterfactual)",
                _projection(summary["conservative_net_pnl"], sample_count),
            ),
            (
                "Severe PnL (counterfactual)",
                _projection(summary["severe_net_pnl"], sample_count),
            ),
            (
                "Fork-simulated PnL",
                _projection(summary["fork_simulated_net_pnl"], fork_sample_count),
            ),
            ("Candidates", funnel["candidates"]),
            ("Primary profitable", funnel["primary_profitable"]),
            ("Independently verified", business["independently_verified_count"]),
            ("Fork successful", business["fork_successful_count"]),
            ("Nearest to profitable", business["nearest_to_profitable_count"]),
            ("Active SHADOW routes", business["active_shadow_routes"]),
            ("Sample size", f"{business['sample_count']} / {data['window_hours']}h"),
        ]
    )
    st.caption(
        "All projections are counterfactual, not realized, and based on the displayed sample count and period. "
        "A projection is unavailable when its required evidence sample is zero. "
        "Realized PnL is 0 / not applicable in SHADOW."
    )
    metadata = [
        {"field": "model_version", "value": governance["model_version"]},
        {"field": "policy_version", "value": governance["policy_version"]},
        {
            "field": "route_configuration_hash",
            "value": governance["route_configuration_hash"],
        },
        {"field": "report_generated_at", "value": data["generated_at"]},
        {"field": "report_age_seconds", "value": str(snapshot.age_seconds)},
        {"field": "data_completeness", "value": data["feed"]["completeness_status"]},
    ]
    st.dataframe(_rows(metadata), width="stretch", hide_index=True)
    _render_alerts(snapshot)


def _render_funnel(snapshot: DashboardSnapshot) -> None:
    funnel = snapshot.data["funnel"]
    chart = _chart_rows(
        ({"stage": row["stage"], "count": row["count"]} for row in funnel),
        ("count",),
    )
    st.bar_chart(chart, x="stage", y="count", width="stretch")
    dropoffs = []
    for row in funnel:
        for reason in row["dropoff_reasons"]:
            dropoffs.append({"stage": row["stage"], **reason})
    st.dataframe(_rows(dropoffs), width="stretch", hide_index=True)


def _render_profitability(snapshot: DashboardSnapshot) -> None:
    profitability = snapshot.data["profitability"]
    summary = profitability["summary"]
    sample_count = Decimal(snapshot.data["business"]["sample_count"])
    _metric_grid(
        [
            (
                "Gross (counterfactual)",
                _projection(summary["gross_profit"], sample_count),
            ),
            (
                "Total cost (counterfactual)",
                _projection(summary["total_cost"], sample_count),
            ),
            ("Net (counterfactual)", _projection(summary["net_pnl"], sample_count)),
            (
                "Fork-simulated net",
                _projection(
                    summary["fork_simulated_net_pnl"],
                    Decimal(snapshot.data["fork"]["simulations"]),
                ),
            ),
        ]
    )
    st.caption(SHADOW_FINANCIAL_LABEL)
    left, right = st.columns(2)
    with left:
        st.subheader("Cost breakdown")
        st.dataframe(
            _rows(profitability["cost_breakdown"]), width="stretch", hide_index=True
        )
    with right:
        st.subheader("Nearest opportunities")
        st.dataframe(
            _rows(profitability["nearest_opportunities"]),
            width="stretch",
            hide_index=True,
        )
    st.subheader("Route-level PnL")
    st.dataframe(_rows(profitability["route_pnl"]), width="stretch", hide_index=True)
    left, right = st.columns(2)
    with left:
        st.subheader("Profitability distribution")
        distribution = _chart_rows(profitability["distribution"], ("count",))
        st.bar_chart(
            distribution, x="bucket", y="count", color="scenario", width="stretch"
        )
    with right:
        st.subheader("Prediction vs fork error")
        error = _chart_rows(profitability["prediction_error"], ("count",))
        st.bar_chart(error, x="bucket", y="count", width="stretch")
    st.subheader("Daily trend")
    daily = _chart_rows(
        profitability["daily_trend"],
        ("expected", "conservative", "severe", "fork_simulated", "sample_count"),
    )
    if not daily.empty:
        st.line_chart(
            daily,
            x="period",
            y=["expected", "conservative", "severe", "fork_simulated"],
            width="stretch",
        )
    st.subheader("Weekly trend")
    weekly = _chart_rows(
        profitability["weekly_trend"],
        ("expected", "conservative", "severe", "fork_simulated", "sample_count"),
    )
    if not weekly.empty:
        st.line_chart(
            weekly,
            x="period",
            y=["expected", "conservative", "severe", "fork_simulated"],
            width="stretch",
        )
    st.subheader("Model comparison")
    st.dataframe(
        _rows(profitability["model_comparison"]), width="stretch", hide_index=True
    )


def _render_routes(snapshot: DashboardSnapshot) -> None:
    governance = snapshot.data["governance"]
    kpi("Route configuration hash", governance["route_configuration_hash"])
    rows = []
    for route in snapshot.data["routes"]:
        rows.append(
            {
                "rank": route["rank"],
                "route_id": route["route_id"],
                "active_shadow": route["active_shadow"],
                "candidate_count": route["candidate_count"],
                "ranking_score": _percent_from_bps(route["ranking_score_bps"]),
                "liquidity_score": _percent_from_bps(
                    route["score_components"]["liquidity"]
                ),
                "freshness_score": _percent_from_bps(
                    route["score_components"]["freshness"]
                ),
                "confidence_score": _percent_from_bps(
                    route["score_components"]["confidence"]
                ),
                "reliability_score": _percent_from_bps(
                    route["score_components"]["reliability"]
                ),
                "data_quality_warnings": ", ".join(route["data_quality_warnings"])
                or "none",
                "expected_net_pnl": route["expected_net_pnl"],
                "conservative_net_pnl": route["conservative_net_pnl"],
                "provider_failure_contribution": _percent_from_bps(
                    route["provider_failure_contribution_bps"]
                ),
                "fork_success_rate": _percent_from_bps(route["fork_success_rate_bps"]),
            }
        )
    st.dataframe(_rows(rows), width="stretch", hide_index=True)


def _render_health(snapshot: DashboardSnapshot) -> None:
    rows = []
    for service in snapshot.data["services"]:
        rows.append(
            {
                "service": service["service"],
                "state": service["state"],
                "expected": service["expected_state"],
                "image_digest": service["image_digest"],
                "git_sha": service["git_sha"],
                "started_at": service["started_at"] or "not_running",
                "exit_code": str(service["exit_code"])
                if service["exit_code"] is not None
                else "not_applicable",
                "oom": service["oom"],
                "restart_count": service["restart_count"],
                "readiness_freshness_seconds": service["readiness_freshness_seconds"]
                or "not_available",
            }
        )
    st.dataframe(_rows(rows), width="stretch", hide_index=True)


def _render_feed(snapshot: DashboardSnapshot) -> None:
    feed = snapshot.data["feed"]
    _metric_grid(
        [
            ("Completeness", feed["completeness_status"]),
            ("Feed gaps", feed["gap_count"]),
            ("Missing sequences", feed["missing_sequences"]),
            ("Reconnects", feed["reconnects"]),
            ("Most recent gap", feed["most_recent_gap_at"] or "none_observed"),
            ("Affected windows", len(feed["affected_windows"])),
        ],
        width=3,
    )
    left, right = st.columns(2)
    with left:
        st.subheader("Unsupported kinds")
        st.dataframe(_rows(feed["unsupported_kinds"]), width="stretch", hide_index=True)
    with right:
        st.subheader("Ignored kinds")
        st.dataframe(_rows(feed["ignored_kinds"]), width="stretch", hide_index=True)
    st.dataframe(
        _rows({"affected_window": value} for value in feed["affected_windows"]),
        width="stretch",
        hide_index=True,
    )


def _render_rpc(snapshot: DashboardSnapshot) -> None:
    rpc = snapshot.data["rpc"]
    _metric_grid(
        [
            ("Secondary requested", rpc["secondary_requested"]),
            ("Agreed", rpc["agreed"]),
            ("Disagreed", rpc["disagreed"]),
            ("State freshness (s)", rpc["state_freshness_seconds"]),
            ("Pinned block", rpc["pinned_block_status"]),
        ],
        width=3,
    )
    rows = []
    for provider in rpc["providers"]:
        rows.append(
            {
                "provider_id": provider["provider_id"],
                "role": provider["role"],
                "success_rate": _percent_from_bps(provider["success_rate_bps"]),
                "timeouts": provider["timeouts"],
                "rate_limits": provider["rate_limits"],
                "unavailable": provider["unavailable"],
                "p50_latency_ms": provider["p50_latency_ms"] or "not_supported",
                "p95_latency_ms": provider["p95_latency_ms"] or "not_supported",
                "p99_latency_ms": provider["p99_latency_ms"] or "not_supported",
                "budget_utilization": _percent_from_bps(
                    provider["budget_utilization_bps"]
                ),
                "self_verification_prevented": provider["self_verification_prevented"],
            }
        )
    st.dataframe(_rows(rows), width="stretch", hide_index=True)


def _render_jetstream(snapshot: DashboardSnapshot) -> None:
    jetstream = snapshot.data["jetstream"]
    persistence = jetstream["persistence"]
    st.subheader("Streams")
    st.dataframe(_rows(jetstream["streams"]), width="stretch", hide_index=True)
    st.subheader("Consumers")
    st.dataframe(_rows(jetstream["consumers"]), width="stretch", hide_index=True)
    latency = persistence["database_write_latency_ms"]
    _metric_grid(
        [
            ("Persistence throughput/s", persistence["throughput_per_second"]),
            ("Batch size", persistence["batch_size"]),
            ("Backlog growth", persistence["backlog_growth"]),
            ("DB write p50 (ms)", latency["p50"]),
            ("DB write p95 (ms)", latency["p95"]),
            ("DB write p99 (ms)", latency["p99"]),
        ],
        width=3,
    )


def _render_postgres(snapshot: DashboardSnapshot) -> None:
    postgres = snapshot.data["postgres"]
    _metric_grid(
        [
            ("Readiness", str(postgres["readiness"]).lower()),
            ("Database size (bytes)", postgres["database_size_bytes"]),
            ("Growth 1h (bytes)", postgres["growth_bytes_1h"]),
            ("Growth 6h (bytes)", postgres["growth_bytes_6h"]),
            ("Growth 24h (bytes)", postgres["growth_bytes_24h"]),
            ("Projected headroom (bytes)", postgres["projected_disk_headroom_bytes"]),
            ("Active connections", postgres["active_connections"]),
            ("WAL bytes", postgres["wal_bytes"]),
            ("Timed checkpoints", postgres["checkpoints_timed"]),
            ("Requested checkpoints", postgres["checkpoints_requested"]),
            ("Retention", postgres["retention_status"]),
        ],
        width=3,
    )
    st.dataframe(
        _rows(
            [
                {
                    "field": "oldest_relevant_event",
                    "value": postgres["oldest_relevant_event"] or "unavailable",
                },
                {
                    "field": "newest_relevant_event",
                    "value": postgres["newest_relevant_event"] or "unavailable",
                },
                {"field": "migration_version", "value": postgres["migration_version"]},
                {
                    "field": "migration_checksum",
                    "value": postgres["migration_checksum"],
                },
            ]
        ),
        width="stretch",
        hide_index=True,
    )


def _render_reliability(snapshot: DashboardSnapshot) -> None:
    reliability = snapshot.data["reliability"]
    _metric_grid(
        [
            ("Retry attempts", reliability["retry_attempts"]),
            ("Recovered retries", reliability["recovered_retries"]),
            ("Exhausted / quarantined", reliability["exhausted_or_quarantined"]),
            ("Terminal integrity failures", reliability["terminal_integrity_failures"]),
            ("Restart loops", reliability["restart_loops"]),
            (
                "Later progress after quarantine",
                reliability["later_message_progress_after_quarantine"],
            ),
            (
                "Protected service identity",
                reliability["protected_service_identity_status"],
            ),
        ],
        width=3,
    )
    st.dataframe(_rows(reliability["runtime_exits"]), width="stretch", hide_index=True)


def _render_fork(snapshot: DashboardSnapshot) -> None:
    fork = snapshot.data["fork"]
    _metric_grid(
        [
            ("Unsigned plans", fork["unsigned_plan_count"]),
            ("Simulations", fork["simulations"]),
            ("Success", fork["success"]),
            ("Reverted", fork["reverted"]),
            ("Gas used", fork["gas_used"]),
            ("Balance delta", fork["balance_delta"]),
            ("Simulated net PnL", fork["simulated_net_pnl"]),
            ("Absolute prediction error", fork["absolute_prediction_error"]),
            ("Fork block", fork["fork_block"]),
            ("Contract guard failures", fork["contract_guard_failures"]),
        ],
        width=3,
    )
    st.caption(
        "Fork values are counterfactual simulation evidence and are not realized revenue."
    )


def _render_artifacts(snapshot: DashboardSnapshot) -> None:
    artifacts = snapshot.data["artifacts"]
    st.dataframe(
        _rows(
            {
                "kind": item["kind"],
                "available": item["available"],
                "generated_at": item["generated_at"] or "unavailable",
                "size_bytes": str(item["size_bytes"])
                if item["size_bytes"] is not None
                else "unavailable",
                "sha256": item["sha256"] or "unavailable",
            }
            for item in artifacts
        ),
        width="stretch",
        hide_index=True,
    )
    for item in artifacts:
        if not item["available"]:
            continue
        try:
            payload = read_artifact(snapshot, item)
        except SnapshotError as exc:
            st.error(f"{item['label']}: {exc.code}")
            continue
        st.download_button(
            label=f"Download {item['label']}",
            data=payload,
            file_name=item["path"],
            mime=item["content_type"],
            key=f"artifact-{item['kind']}",
        )


def _render_logs(snapshot: DashboardSnapshot) -> None:
    logs = snapshot.data["logs"]
    services = sorted({row["service"] for row in logs})
    severities = sorted({row["severity"] for row in logs})
    event_classes = sorted({row["event_class"] for row in logs})
    c1, c2, c3, c4, c5 = st.columns(5)
    with c1:
        selected_services = st.multiselect("Service", services, default=services)
    with c2:
        selected_severities = st.multiselect("Severity", severities, default=severities)
    with c3:
        selected_events = st.multiselect(
            "Event class", event_classes, default=event_classes
        )
    with c4:
        window_label = st.selectbox(
            "Time window", ("15 minutes", "1 hour", "6 hours", "24 hours"), index=3
        )
    with c5:
        row_limit = st.number_input(
            "Row limit",
            min_value=1,
            max_value=MAX_LOG_ROWS,
            value=min(100, MAX_LOG_ROWS),
            step=25,
        )
    window_delta = {
        "15 minutes": timedelta(minutes=15),
        "1 hour": timedelta(hours=1),
        "6 hours": timedelta(hours=6),
        "24 hours": timedelta(hours=24),
    }[window_label]
    lower_bound = snapshot.generated_at - window_delta
    filtered = [
        row
        for row in logs
        if row["service"] in selected_services
        and row["severity"] in selected_severities
        and row["event_class"] in selected_events
        and pd.Timestamp(row["timestamp"]).to_pydatetime() >= lower_bound
    ][: int(row_limit)]
    st.dataframe(_rows(filtered), width="stretch", hide_index=True)


def main() -> None:
    st.set_page_config(page_title="Phoenix PRE-LIVE SHADOW", layout="wide")
    st.title("Phoenix PRE-LIVE SHADOW")
    try:
        snapshot = _load_current_snapshot()
    except SnapshotError as exc:
        st.error(f"Dashboard evidence unavailable: {exc.code}")
        st.metric("Operating mode", "UNAVAILABLE")
        st.metric("Pre-LIVE gate", "blocked")
        st.stop()
    except Exception:
        st.error("Dashboard evidence unavailable: dashboard_internal_error")
        st.metric("Operating mode", "UNAVAILABLE")
        st.metric("Pre-LIVE gate", "blocked")
        st.stop()

    safety = snapshot.data["safety"]
    st.warning(SHADOW_FINANCIAL_LABEL)
    _metric_grid(
        [
            ("Mode", safety["mode"]),
            ("LIVE execution", str(safety["live_execution"]).lower()),
            ("Execution eligible", str(safety["execution_eligible"]).lower()),
            (
                "Execution request created",
                str(safety["execution_request_created"]).lower(),
            ),
            ("Evidence age (s)", snapshot.age_seconds),
        ],
        width=5,
    )

    tabs = st.tabs(
        [
            "Executive",
            "Opportunity Funnel",
            "Profitability",
            "Route Intelligence",
            "Technical Health",
            "Feed Quality",
            "RPC Verification",
            "JetStream & Recorder",
            "PostgreSQL",
            "Reliability",
            "Fork Simulation",
            "Reports & Evidence",
            "Redacted Logs",
        ]
    )
    renderers = (
        _render_overview,
        _render_funnel,
        _render_profitability,
        _render_routes,
        _render_health,
        _render_feed,
        _render_rpc,
        _render_jetstream,
        _render_postgres,
        _render_reliability,
        _render_fork,
        _render_artifacts,
        _render_logs,
    )
    for tab, renderer in zip(tabs, renderers):
        with tab:
            renderer(snapshot)


if __name__ == "__main__":
    main()
