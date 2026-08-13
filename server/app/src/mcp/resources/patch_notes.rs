use crate::auth::ApiKeyScopeChecker;
use crate::mcp::Mcp;
use crate::records::PatchNoteRecord;
use graph_api::models::{ApiKey, ApiKeyScope};
use rmcp::ErrorData;
use rmcp::model::{ReadResourceResponse, ReadResourceResult, ResourceContents, ResourceTemplate};
use uuid::Uuid;

const URI_PREFIX: &str = "graph://patch-notes/";

pub(super) fn template() -> ResourceTemplate {
    ResourceTemplate::new(format!("{URI_PREFIX}{{uuid}}"), "patch-note")
}

pub(super) fn parse_uri(uri: &str) -> Option<Uuid> {
    uri.strip_prefix(URI_PREFIX)?.parse().ok()
}

impl Mcp {
    pub(super) async fn read_patch_note_resource(
        &self,
        api_key: &ApiKey,
        patch_note_id: Uuid,
        uri: &str,
    ) -> Result<ReadResourceResponse, ErrorData> {
        if !api_key.has_scope(&ApiKeyScope::PatchNotesColonRead) {
            return Err(ErrorData::invalid_request(
                "missing required scope: patch-notes:read",
                None,
            ));
        }

        let record = sqlx::query_as::<_, PatchNoteRecord>("SELECT * FROM patch_notes WHERE id = ?")
            .bind(patch_note_id)
            .fetch_optional(&self.default_pool)
            .await
            .map_err(|error| {
                tracing::error!(%error, %patch_note_id, "failed to fetch patch note record");
                ErrorData::internal_error("failed to fetch patch note", None)
            })?
            .ok_or_else(|| ErrorData::resource_not_found("patch note not found", None))?;

        let image_urls = sqlx::query_scalar::<_, String>(
            "SELECT url FROM patch_note_images WHERE patch_note_id = ? ORDER BY id ASC",
        )
        .bind(patch_note_id)
        .fetch_all(&self.default_pool)
        .await
        .map_err(|error| {
            tracing::error!(%error, %patch_note_id, "failed to fetch patch note images");
            ErrorData::internal_error("failed to fetch patch note images", None)
        })?;

        let patch_note = record.into_patch_note(image_urls);

        let content = serde_json::to_string(&patch_note).map_err(|error| {
            tracing::error!(%error, %patch_note_id, "failed to serialize patch note resource");
            ErrorData::internal_error("failed to serialize patch note", None)
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
    fn parses_valid_patch_note_uri() {
        let uuid = Uuid::new_v4();
        let uri = format!("graph://patch-notes/{}", uuid);
        assert_eq!(parse_uri(&uri), Some(uuid));
    }

    #[test]
    fn rejects_invalid_patch_note_uri() {
        assert_eq!(parse_uri("graph://patch-notes/invalid"), None);
        assert_eq!(parse_uri("graph://other/uuid"), None);
    }
}
