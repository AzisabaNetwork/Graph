ALTER TABLE api_key_scopes
    DROP CONSTRAINT api_key_scopes_scope_check;

ALTER TABLE api_key_scopes
    ADD CONSTRAINT api_key_scopes_scope_check CHECK (
        scope IN (
            '*',
            'api-keys:read',
            'api-keys:write',
            'crawls:read',
            'crawls:write',
            'emojis:read',
            'emojis:write',
            'patch-notes:read',
            'patch-notes:write',
            'players:read',
            'players:read-details',
            'players:write',
            'punishments:read',
            'punishments:write',
            'stream:read'
        )
    );
