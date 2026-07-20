ALTER TABLE patch_notes
    DROP CONSTRAINT patch_notes_target_check,
    ADD CONSTRAINT patch_notes_target_check CHECK (
        target IN (
            'general',
            'creativePro',
            'frontier',
            'life',
            'leonGunWar2',
            'sclat'
        )
    );
