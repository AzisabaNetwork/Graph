ALTER TABLE patch_notes
    DROP CONSTRAINT patch_notes_category_check,
    ADD CONSTRAINT patch_notes_category_check CHECK (
        category IN (
            'balance',
            'event',
            'feature',
            'fix',
            'improvement',
            'remove'
        )
    );
