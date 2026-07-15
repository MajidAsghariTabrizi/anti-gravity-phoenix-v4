ALTER TABLE shadow_engine_classifications
    DROP CONSTRAINT IF EXISTS shadow_engine_classification_value_check;

ALTER TABLE shadow_engine_classifications
    ADD CONSTRAINT shadow_engine_classification_value_check
        CHECK (classification IN (
            'no_relevant_route',
            'candidate_generated',
            'candidate_rejected',
            'shadow_accepted',
            'malformed_internal_event',
            'unsupported_schema',
            'transient_dependency_failure',
            'dependency_exhausted',
            'terminal_integrity_failure'
        ));

ALTER TABLE shadow_engine_processing_attempts
    DROP CONSTRAINT IF EXISTS shadow_engine_attempt_classification_check;

ALTER TABLE shadow_engine_processing_attempts
    ADD CONSTRAINT shadow_engine_attempt_classification_check
        CHECK (classification IN (
            'no_relevant_route',
            'candidate_generated',
            'candidate_rejected',
            'shadow_accepted',
            'malformed_internal_event',
            'unsupported_schema',
            'transient_dependency_failure',
            'dependency_exhausted',
            'terminal_integrity_failure'
        ));
