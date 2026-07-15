use graph_api::models::{ApiKey, ApiKeyScope};

pub(crate) trait ApiKeyScopeExt {
    fn has_scope(&self, scope: &ApiKeyScope) -> bool;

    fn has_all_scopes(&self, scopes: &[ApiKeyScope]) -> bool;
}

impl ApiKeyScopeExt for ApiKey {
    fn has_scope(&self, scope: &ApiKeyScope) -> bool {
        let star = ApiKeyScope::Star.to_string();
        let scope = scope.to_string();

        self.scopes
            .iter()
            .any(|granted| granted == &star || granted == &scope)
    }

    fn has_all_scopes(&self, scopes: &[ApiKeyScope]) -> bool {
        scopes.iter().all(|scope| self.has_scope(scope))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use graph_api::types::Nullable;

    #[test]
    fn star_grants_every_scope() {
        let api_key = api_key_with_scopes(&[ApiKeyScope::Star]);

        assert!(api_key.has_scope(&ApiKeyScope::ApiKeysColonWrite));
        assert!(api_key.has_scope(&ApiKeyScope::PatchNotesColonRead));
    }

    #[test]
    fn only_star_can_delegate_star() {
        let star = api_key_with_scopes(&[ApiKeyScope::Star]);
        let writer = api_key_with_scopes(&[ApiKeyScope::ApiKeysColonWrite]);

        assert!(star.has_all_scopes(&[ApiKeyScope::Star]));
        assert!(!writer.has_all_scopes(&[ApiKeyScope::Star]));
    }

    fn api_key_with_scopes(scopes: &[ApiKeyScope]) -> ApiKey {
        ApiKey::new(
            "Test API key".to_string(),
            "test-public-id".to_string(),
            scopes.iter().map(ToString::to_string).collect(),
            Utc::now(),
            Nullable::Null,
        )
    }
}
