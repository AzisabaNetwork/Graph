use crate::auth::ApiKeyScopeChecker;
use crate::mcp::Mcp;
use crate::records::{PunishmentProofRecord, PunishmentRecord};
use graph_api::models::{ApiKey, ApiKeyScope};
use rmcp::ErrorData;
use rmcp::model::{ReadResourceResponse, ReadResourceResult, ResourceContents, ResourceTemplate};

const URI_PREFIX: &str = "graph://punishments/";

pub(super) fn template() -> ResourceTemplate {
    ResourceTemplate::new(format!("{URI_PREFIX}{{id}}"), "punishment")
}

pub(super) fn parse_uri(uri: &str) -> Option<u64> {
    uri.strip_prefix(URI_PREFIX)?.parse().ok()
}

impl Mcp {
    pub(super) async fn read_punishment_resource(
        &self,
        api_key: &ApiKey,
        punishment_id: u64,
        uri: &str,
    ) -> Result<ReadResourceResponse, ErrorData> {
        if !api_key.has_scope(&ApiKeyScope::PunishmentsColonRead) {
            return Err(ErrorData::invalid_request(
                "missing required scope: punishments:read",
                None,
            ));
        }

        let record =
            sqlx::query_as::<_, PunishmentRecord>("SELECT * FROM punishmentHistory WHERE id = ?")
                .bind(punishment_id as i64)
                .fetch_optional(&self.punishments_pool)
                .await
                .map_err(|error| {
                    tracing::error!(%error, %punishment_id, "failed to fetch punishment record");
                    ErrorData::internal_error("failed to fetch punishment", None)
                })?
                .ok_or_else(|| ErrorData::resource_not_found("punishment not found", None))?;

        let proof_records = sqlx::query_as::<_, PunishmentProofRecord>(
            r#"
            SELECT punish_id, id, text, public
            FROM proofs
            WHERE punish_id = ?
            "#,
        )
        .bind(punishment_id as i64)
        .fetch_all(&self.punishments_pool)
        .await
        .map_err(|error| {
            tracing::error!(%error, %punishment_id, "failed to fetch punishment proofs");
            ErrorData::internal_error("failed to fetch punishment proofs", None)
        })?;

        let mut proofs = Vec::with_capacity(proof_records.len());
        for pr in proof_records {
            proofs.push(pr.into_proof().map_err(|e| {
                ErrorData::internal_error(format!("failed to convert proof: {}", e), None)
            })?);
        }

        let punishment = record.into_punishment(proofs).map_err(|e| {
            ErrorData::internal_error(format!("failed to convert punishment: {}", e), None)
        })?;

        let content = serde_json::to_string(&punishment).map_err(|error| {
            tracing::error!(%error, %punishment_id, "failed to serialize punishment resource");
            ErrorData::internal_error("failed to serialize punishment", None)
        })?;

        Ok(ReadResourceResponse::Complete(ReadResourceResult::new(
            vec![ResourceContents::text(content, uri).with_mime_type("application/json")],
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_punishment_uri() {
        assert_eq!(parse_uri("graph://punishments/123"), Some(123));
    }

    #[test]
    fn rejects_invalid_punishment_uri() {
        assert_eq!(parse_uri("graph://punishments/abc"), None);
        assert_eq!(parse_uri("graph://other/123"), None);
    }
}
