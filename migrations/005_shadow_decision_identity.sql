DO $$
DECLARE
    legacy_constraint TEXT;
BEGIN
    SELECT constraint_row.conname
      INTO legacy_constraint
      FROM pg_constraint constraint_row
     WHERE constraint_row.conrelid = 'public.shadow_decisions'::regclass
       AND constraint_row.contype = 'u'
       AND pg_get_constraintdef(constraint_row.oid) =
           'UNIQUE (strategy_version, route_fingerprint, source_sequence, observed_block)'
     LIMIT 1;

    IF legacy_constraint IS NOT NULL THEN
        EXECUTE format(
            'ALTER TABLE public.shadow_decisions DROP CONSTRAINT %I',
            legacy_constraint
        );
    END IF;
END
$$;

CREATE UNIQUE INDEX IF NOT EXISTS shadow_decisions_source_event_route_idx
    ON shadow_decisions(source_event_identity, strategy_version, route_fingerprint)
    WHERE source_event_identity IS NOT NULL;
