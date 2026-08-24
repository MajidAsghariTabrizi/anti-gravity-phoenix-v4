-- Economic dashboard refresh budget insurance.
--
-- The window_funnel CTE of the economic-dashboard snapshot counts
-- feed_events and rpc_quality_records per evidence window (1h/24h/7d)
-- on every refresh. Without timestamp indexes those counts are full
-- sequential scans whose cost grows linearly with ingest history.
CREATE INDEX IF NOT EXISTS feed_events_recorded_at_idx
    ON public.feed_events(recorded_at);
CREATE INDEX IF NOT EXISTS rpc_quality_records_recorded_at_idx
    ON public.rpc_quality_records(recorded_at);
