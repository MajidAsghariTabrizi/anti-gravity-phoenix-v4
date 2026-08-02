CREATE OR REPLACE FUNCTION pg_temp.phoenix_mark_ctes_not_materialized(
    target_view regclass,
    cte_names text[]
) RETURNS void
LANGUAGE plpgsql
AS $$
DECLARE
    view_definition text;
    rewritten_definition text;
    cte_name text;
BEGIN
    view_definition := pg_get_viewdef(target_view, true);
    IF view_definition IS NULL THEN
        RAISE EXCEPTION 'view definition is missing for %', target_view;
    END IF;

    FOREACH cte_name IN ARRAY cte_names LOOP
        IF view_definition ~* format(
            '\m%s\M[[:space:]]+AS[[:space:]]+NOT[[:space:]]+MATERIALIZED',
            cte_name
        ) THEN
            CONTINUE;
        END IF;

        rewritten_definition := regexp_replace(
            view_definition,
            format('(\m%s\M[[:space:]]+AS)[[:space:]]*\(', cte_name),
            E'\\1 NOT MATERIALIZED (',
            'i'
        );
        IF rewritten_definition = view_definition THEN
            RAISE EXCEPTION 'expected CTE % is missing from %', cte_name, target_view;
        END IF;
        view_definition := rewritten_definition;
    END LOOP;

    EXECUTE format(
        'CREATE OR REPLACE VIEW %s AS %s',
        target_view,
        view_definition
    );
END;
$$;

SELECT pg_temp.phoenix_mark_ctes_not_materialized(
    'public.phoenix_live_economic_truth'::regclass,
    ARRAY['size_points', 'facts']
);

SELECT pg_temp.phoenix_mark_ctes_not_materialized(
    'public.phoenix_live_economic_loss_ledger'::regclass,
    ARRAY['numeric_truth', 'contextual', 'caused']
);

SELECT pg_temp.phoenix_mark_ctes_not_materialized(
    'public.phoenix_daily_economic_attack_surface'::regclass,
    ARRAY['ranked']
);
