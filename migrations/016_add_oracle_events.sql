-- Migration: 016_add_oracle_events.sql
-- Adds table for event-driven oracle event tracking
-- Run: sea-db migration run 016_add_oracle_events

CREATE TABLE IF NOT EXISTS live_canary.oracle_events (
    event_id UUID PRIMARY KEY,
    auction_id VARCHAR(255) NOT NULL,
    signal_id VARCHAR(255),
    event_type VARCHAR(50) NOT NULL,
    block_number BIGINT NOT NULL,
    transaction_hash VARCHAR(66),
    observed_at TIMESTAMPTZ NOT NULL,
    processed BOOLEAN DEFAULT FALSE,
    processing_attempts INTEGER DEFAULT 0,
    created_at TIMESTAMPTZ DEFAULT now(),
    completed_at TIMESTAMPTZ
);

-- Indexes for fast lookup by auction and type
CREATE INDEX IF NOT EXISTS idx_oracle_events_auction_id ON live_canary.oracle_events(auction_id);
CREATE INDEX IF NOT EXISTS idx_oracle_events_processed ON live_canary.oracle_events(processed);
CREATE INDEX IF NOT EXISTS idx_oracle_events_type ON live_canary.oracle_events(event_type);
CREATE INDEX IF NOT EXISTS idx_oracle_events_observed_at ON live_canary.oracle_events(observed_at DESC);

-- Comment: Oracle events table stores detected liquidation/oracle events
-- from the feed ingestor. These events feed the EventDrivenTrigger which
-- bypasses the ~31h polling cycle for faster liquidation recall.
COMMENT ON TABLE live_canary.oracle_events IS 'Event-driven oracle events from feed processing;
used by EventDrivenTrigger to bypass periodic polling cycle';