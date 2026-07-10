use crate::{api::Api, pagination::Cursor};
use async_trait::async_trait;
use axum::extract::Host;
use axum_extra::extract::CookieJar;
use chrono::{DateTime, Utc};
use graph_api::apis::patch_notes::{
    CreatePatchNoteResponse, DeletePatchNoteByIdResponse, GetPatchNoteByIdResponse,
    ListPatchNotesResponse, PatchNotes,
};
use graph_api::models::{
    CreatePatchNoteRequest, DeletePatchNoteByIdPathParams, GetPatchNoteByIdPathParams,
    ListPatchNotes200Response, ListPatchNotes200ResponseItemsInner, ListPatchNotesQueryParams,
    PatchNote, PatchNoteCategory, PatchNoteTarget,
};
use graph_api::types::Nullable;
use http::Method;
use sqlx::{MySql, QueryBuilder};
use std::str::FromStr;
use uuid::Uuid;

type PatchNotesCursor = Cursor<DateTime<Utc>, Uuid>;

#[derive(Debug, sqlx::FromRow)]
struct PatchNoteRow {
    id: Uuid,
    target: String,
    category: String,
    title: String,
    body: String,
    created_at: DateTime<Utc>,
}

impl TryFrom<PatchNoteRow> for PatchNote {
    type Error = String;

    fn try_from(value: PatchNoteRow) -> Result<Self, Self::Error> {
        PatchNoteTarget::from_str(&value.target)
            .map_err(|_| format!("invalid patch note target: {}", value.target))?;
        PatchNoteCategory::from_str(&value.category)
            .map_err(|_| format!("invalid patch note category: {}", value.category))?;

        Ok(Self {
            id: value.id,
            target: value.target,
            category: value.category,
            title: value.title,
            body: value.body,
            created_at: value.created_at,
        })
    }
}

#[async_trait]
impl PatchNotes for Api {
    async fn create_patch_note(
        &self,
        _method: Method,
        _host: Host,
        _cookies: CookieJar,
        body: CreatePatchNoteRequest,
    ) -> Result<CreatePatchNoteResponse, String> {
        if body.title.trim().is_empty() || body.body.trim().is_empty() {
            return Ok(CreatePatchNoteResponse::Status400_InvalidRequestBody);
        }

        let target = match PatchNoteTarget::from_str(&body.target) {
            Ok(target) => target.to_string(),
            Err(_) => return Ok(CreatePatchNoteResponse::Status400_InvalidRequestBody),
        };

        let category = match PatchNoteCategory::from_str(&body.category) {
            Ok(category) => category.to_string(),
            Err(_) => return Ok(CreatePatchNoteResponse::Status400_InvalidRequestBody),
        };

        let id = Uuid::new_v4();
        let created_at = Utc::now();

        sqlx::query(
            r#"
            INSERT INTO patch_notes (id, target, category, title, body, created_at)
            VALUES (?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(id)
        .bind(&target)
        .bind(&category)
        .bind(&body.title)
        .bind(&body.body)
        .bind(created_at)
        .execute(&self.pool)
        .await
        .map_err(|error| {
            tracing::error!(?error, "failed to insert patch note");
            error.to_string()
        })?;

        Ok(
            CreatePatchNoteResponse::Status201_PatchNoteCreatedSuccessfully(
                ListPatchNotes200ResponseItemsInner::new(
                    id, target, category, body.title, body.body, created_at,
                ),
            ),
        )
    }

    async fn delete_patch_note_by_id(
        &self,
        _method: Method,
        _host: Host,
        _cookies: CookieJar,
        path_params: DeletePatchNoteByIdPathParams,
    ) -> Result<DeletePatchNoteByIdResponse, String> {
        let result = sqlx::query("DELETE FROM patch_notes WHERE id = ?")
            .bind(path_params.patch_note_id)
            .execute(&self.pool)
            .await
            .map_err(|error| {
                tracing::error!(?error, "failed to delete patch note");
                error.to_string()
            })?;

        if result.rows_affected() == 0 {
            return Ok(DeletePatchNoteByIdResponse::Status404_PatchNoteNotFound);
        }

        Ok(DeletePatchNoteByIdResponse::Status204_PatchNoteDeletedSuccessfully)
    }

    async fn get_patch_note_by_id(
        &self,
        _method: Method,
        _host: Host,
        _cookies: CookieJar,
        path_params: GetPatchNoteByIdPathParams,
    ) -> Result<GetPatchNoteByIdResponse, String> {
        let patch_note =
            sqlx::query_as::<_, (Uuid, String, String, String, String, DateTime<Utc>)>(
                r#"
            SELECT id, target, category, title, body, created_at
            FROM patch_notes
            WHERE id = ?
            "#,
            )
            .bind(path_params.patch_note_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| {
                tracing::error!(?error, "failed to get patch note");
                error.to_string()
            })?;

        let Some((id, target, category, title, body, created_at)) = patch_note else {
            return Ok(GetPatchNoteByIdResponse::Status404_PatchNoteNotFound);
        };

        Ok(
            GetPatchNoteByIdResponse::Status200_PatchNoteRetrievedSuccessfully(
                ListPatchNotes200ResponseItemsInner::new(
                    id, target, category, title, body, created_at,
                ),
            ),
        )
    }

    async fn list_patch_notes(
        &self,
        _method: Method,
        _host: Host,
        _cookies: CookieJar,
        query_params: ListPatchNotesQueryParams,
    ) -> Result<ListPatchNotesResponse, String> {
        let limit = query_params.limit.unwrap_or(20).clamp(1, 100) as usize;

        let target = match query_params.target.as_deref() {
            Some(target) => match PatchNoteTarget::from_str(target) {
                Ok(target) => Some(target.to_string()),
                Err(_) => return Ok(ListPatchNotesResponse::Status400_InvalidQueryParameters),
            },
            None => None,
        };

        let category = match query_params.category.as_deref() {
            Some(category) => match PatchNoteCategory::from_str(category) {
                Ok(category) => Some(category.to_string()),
                Err(_) => return Ok(ListPatchNotesResponse::Status400_InvalidQueryParameters),
            },
            None => None,
        };

        let cursor = match query_params.cursor.as_deref() {
            Some(cursor) => match PatchNotesCursor::decode(cursor) {
                Ok(cursor) => Some(cursor),
                Err(_) => return Ok(ListPatchNotesResponse::Status400_InvalidQueryParameters),
            },
            None => None,
        };

        let mut query = QueryBuilder::<MySql>::new(
            r#"
            SELECT id, target, category, title, body, created_at
            FROM patch_notes
            WHERE 1 = 1
            "#,
        );

        if let Some(target) = target {
            query.push(" AND target = ").push_bind(target);
        }

        if let Some(category) = category {
            query.push(" AND category = ").push_bind(category);
        }

        if let Some(cursor) = cursor {
            query
                .push(" AND (created_at < ")
                .push_bind(cursor.value)
                .push(" OR (created_at = ")
                .push_bind(cursor.value)
                .push(" AND id < ")
                .push_bind(cursor.tie_breaker)
                .push("))");
        }

        query
            .push(" ORDER BY created_at DESC, id DESC LIMIT ")
            .push_bind((limit + 1) as i64);

        let mut rows = query
            .build_query_as::<PatchNoteRow>()
            .fetch_all(&self.pool)
            .await
            .map_err(|error| {
                tracing::error!(?error, "failed to list patch notes");
                error.to_string()
            })?;

        let next_cursor = if rows.len() > limit {
            rows.pop();
            let last = rows.last().expect("page must include at least one row");
            let cursor = PatchNotesCursor {
                value: last.created_at,
                tie_breaker: last.id,
            };

            Some(cursor.encode().map_err(|error| {
                tracing::error!(?error, "failed to encode patch notes cursor");
                "failed to encode patch notes cursor".to_string()
            })?)
        } else {
            None
        };

        let items = rows
            .into_iter()
            .map(|row| {
                PatchNote::try_from(row).map(|patch_note| {
                    ListPatchNotes200ResponseItemsInner::new(
                        patch_note.id,
                        patch_note.target,
                        patch_note.category,
                        patch_note.title,
                        patch_note.body,
                        patch_note.created_at,
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| {
                tracing::error!(error, "failed to convert patch note rows");
                error
            })?;

        let mut response = ListPatchNotes200Response::new(items);
        response.next_cursor = next_cursor.map(Nullable::Present);

        Ok(ListPatchNotesResponse::Status200_PatchNotesRetrievedSuccessfully(response))
    }
}
