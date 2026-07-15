use crate::api::Api;
use crate::auth::context::current_api_key;
use crate::auth::scope::ApiKeyScopeExt;
use crate::pagination::Cursor;
use async_trait::async_trait;
use aws_sdk_s3::primitives::ByteStream;
use axum::extract::Host;
use axum_extra::extract::multipart::Field;
use axum_extra::extract::{CookieJar, Multipart};
use chrono::{DateTime, Utc};
use graph_api::apis::patch_notes::{
    CreatePatchNoteResponse, DeletePatchNoteByIdResponse, GetPatchNoteByIdResponse,
    ListPatchNotesResponse, PatchNotes,
};
use graph_api::models::{
    ApiKeyScope, CreatePatchNoteRequest, DeletePatchNoteByIdPathParams, GetPatchNoteByIdPathParams,
    ListPatchNotes200Response, ListPatchNotes200ResponseItemsInner, ListPatchNotesQueryParams,
    PatchNoteCategory, PatchNoteTarget,
};
use graph_api::types::{ByteArray, Nullable};
use http::Method;
use sqlx::{FromRow, MySql, QueryBuilder};
use std::{collections::HashMap, str::FromStr};
use uuid::Uuid;

const DEFAULT_PATCH_NOTES_LIMIT: i32 = 20;
const MAX_PATCH_NOTES_LIMIT: i32 = 100;

type PatchNoteCursor = Cursor<DateTime<Utc>, Uuid>;

#[derive(Debug, FromRow)]
struct PatchNoteRecord {
    id: Uuid,
    target: String,
    category: String,
    title: String,
    body: String,
    created_at: DateTime<Utc>,
}

#[async_trait]
impl PatchNotes for Api {
    async fn create_patch_note(
        &self,
        _method: Method,
        _host: Host,
        _cookies: CookieJar,
        body: Multipart,
    ) -> Result<CreatePatchNoteResponse, String> {
        let api_key = current_api_key()?;
        if !api_key.has_scope(&ApiKeyScope::PatchNotesColonWrite) {
            return Ok(CreatePatchNoteResponse::Status403_TheAuthenticatedAPIKeyDoesNotHaveTheRequiredScope);
        }

        let (request, image_content_types) = match parse_create_patch_note_multipart(body).await {
            Ok(request) => request,
            Err(error) => {
                tracing::error!(%error, "invalid create patch note multipart body");
                return Ok(CreatePatchNoteResponse::Status400_InvalidRequestBody);
            }
        };

        let id = Uuid::new_v4();
        let created_at = Utc::now();
        let CreatePatchNoteRequest {
            target,
            category,
            title,
            body,
            images,
        } = request;
        let images = images.unwrap_or_default();

        let mut stored_images = Vec::with_capacity(images.len());
        if !images.is_empty() {
            let Some(storage) = &self.image_storage else {
                tracing::error!("R2 image storage is not configured");
                return Err("R2 image storage is not configured".to_string());
            };

            for (position, (ByteArray(bytes), content_type)) in
                images.into_iter().zip(image_content_types).enumerate()
            {
                let image_id = Uuid::new_v4();
                let object_key = format!(
                    "patch-notes/{id}/{position:04}-{image_id}{}",
                    image_extension(&content_type)
                );
                let url = storage.public_url(&object_key);

                storage
                    .client
                    .put_object()
                    .bucket(&storage.bucket)
                    .key(&object_key)
                    .content_type(&content_type)
                    .body(ByteStream::from(bytes))
                    .send()
                    .await
                    .map_err(|error| {
                        tracing::error!(?error, %object_key, "failed to upload patch note image");
                        error.to_string()
                    })?;

                stored_images.push((url, object_key, content_type));
            }
        }

        let image_urls = stored_images
            .iter()
            .map(|(url, _, _)| url.clone())
            .collect::<Vec<_>>();

        let mut transaction = self.pool.begin().await.map_err(|error| {
            tracing::error!(?error, "failed to begin transaction");
            error.to_string()
        })?;

        sqlx::query(
            r#"
            INSERT INTO patch_notes (id, target, category, title, body, created_at)
            VALUES (?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(id)
        .bind(&target)
        .bind(&category)
        .bind(&title)
        .bind(&body)
        .bind(created_at)
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            tracing::error!(?error, "failed to insert patch note");
            error.to_string()
        })?;

        for (position, image) in stored_images.iter().enumerate() {
            let (url, object_key, content_type) = image;
            sqlx::query(
                r#"
                INSERT INTO patch_note_images
                    (patch_note_id, position, object_key, url, content_type, created_at)
                VALUES (?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(id)
            .bind(position as u32)
            .bind(object_key)
            .bind(url)
            .bind(content_type)
            .bind(created_at)
            .execute(&mut *transaction)
            .await
            .map_err(|error| {
                tracing::error!(?error, "failed to insert patch note image");
                error.to_string()
            })?;
        }

        transaction.commit().await.map_err(|error| {
            tracing::error!(?error, "failed to commit transaction");
            error.to_string()
        })?;

        Ok(
            CreatePatchNoteResponse::Status201_PatchNoteCreatedSuccessfully(
                ListPatchNotes200ResponseItemsInner::new(
                    id, target, category, title, body, image_urls, created_at,
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
        let api_key = current_api_key()?;
        if !api_key.has_scope(&ApiKeyScope::PatchNotesColonWrite) {
            return Ok(DeletePatchNoteByIdResponse::Status403_TheAuthenticatedAPIKeyDoesNotHaveTheRequiredScope);
        }

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
        let api_key = current_api_key()?;
        if !api_key.has_scope(&ApiKeyScope::PatchNotesColonRead) {
            return Ok(GetPatchNoteByIdResponse::Status403_TheAuthenticatedAPIKeyDoesNotHaveTheRequiredScope);
        }

        let patch_note = sqlx::query_as::<_, PatchNoteRecord>(
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

        let Some(record) = patch_note else {
            return Ok(GetPatchNoteByIdResponse::Status404_PatchNoteNotFound);
        };
        let image_urls = self.load_patch_note_image_urls(&[record.id]).await?;
        let image_urls = image_urls.get(&record.id).cloned().unwrap_or_default();

        Ok(
            GetPatchNoteByIdResponse::Status200_PatchNoteRetrievedSuccessfully(
                patch_note_from_record(record, image_urls),
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
        let api_key = current_api_key()?;
        if !api_key.has_scope(&ApiKeyScope::PatchNotesColonRead) {
            return Ok(
                ListPatchNotesResponse::Status403_TheAuthenticatedAPIKeyDoesNotHaveTheRequiredScope,
            );
        }

        let limit = query_params
            .limit
            .unwrap_or(DEFAULT_PATCH_NOTES_LIMIT)
            .clamp(1, MAX_PATCH_NOTES_LIMIT) as usize;

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
            Some(cursor) => match PatchNoteCursor::decode(cursor) {
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
            .build_query_as::<PatchNoteRecord>()
            .fetch_all(&self.pool)
            .await
            .map_err(|error| {
                tracing::error!(?error, "failed to list patch notes");
                error.to_string()
            })?;

        let next_cursor = if rows.len() > limit {
            rows.pop();
            let last = rows.last().expect("page must include at least one row");
            let cursor = PatchNoteCursor {
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

        let image_urls = self
            .load_patch_note_image_urls(&rows.iter().map(|record| record.id).collect::<Vec<_>>())
            .await?;

        let items = rows
            .into_iter()
            .map(|record| {
                let record_image_urls = image_urls.get(&record.id).cloned().unwrap_or_default();
                patch_note_from_record(record, record_image_urls)
            })
            .collect::<Vec<_>>();

        let next_cursor = next_cursor.map_or(Nullable::Null, Nullable::Present);
        let response = ListPatchNotes200Response::new(items, next_cursor);

        Ok(ListPatchNotesResponse::Status200_PatchNotesRetrievedSuccessfully(response))
    }
}

impl Api {
    async fn load_patch_note_image_urls(
        &self,
        patch_note_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, Vec<String>>, String> {
        if patch_note_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let mut query = QueryBuilder::<MySql>::new(
            r#"
            SELECT patch_note_id, url
            FROM patch_note_images
            WHERE patch_note_id IN (
            "#,
        );

        let mut separated = query.separated(", ");
        for id in patch_note_ids {
            separated.push_bind(*id);
        }
        separated.push_unseparated(") ORDER BY patch_note_id, position");

        let rows = query
            .build_query_as::<(Uuid, String)>()
            .fetch_all(&self.pool)
            .await
            .map_err(|error| {
                tracing::error!(?error, "failed to load patch note image URLs");
                error.to_string()
            })?;

        let mut image_urls = HashMap::<Uuid, Vec<String>>::new();
        for (patch_note_id, url) in rows {
            image_urls.entry(patch_note_id).or_default().push(url);
        }

        Ok(image_urls)
    }
}

async fn parse_create_patch_note_multipart(
    mut multipart: Multipart,
) -> Result<(CreatePatchNoteRequest, Vec<String>), String> {
    let mut target = None;
    let mut category = None;
    let mut title = None;
    let mut body = None;
    let mut images = Vec::new();
    let mut image_content_types = Vec::new();

    let read_text = async |field: Field, name: &str| -> Result<String, String> {
        field.text().await.map_err(|error| {
            tracing::error!(?error, field = name, "failed to read multipart text field");
            error.to_string()
        })
    };

    while let Some(field) = multipart.next_field().await.map_err(|error| {
        tracing::error!(?error, "failed to read multipart field");
        error.to_string()
    })? {
        let Some(name) = field.name().map(str::to_string) else {
            continue;
        };

        match name.as_str() {
            "target" => target = Some(read_text(field, "target").await?),
            "category" => category = Some(read_text(field, "category").await?),
            "title" => title = Some(read_text(field, "title").await?),
            "body" => body = Some(read_text(field, "body").await?),
            "images" => {
                let content_type = field
                    .content_type()
                    .map(str::to_string)
                    .ok_or_else(|| "field `images` must include a content type".to_string())?;
                if !is_supported_image_content_type(&content_type) {
                    return Err(format!(
                        "field `images` has unsupported content type: {content_type}"
                    ));
                }

                let bytes = field.bytes().await.map_err(|error| {
                    tracing::error!(%error, field = "images", "failed to read multipart image field");
                    error.to_string()
                })?;

                if bytes.is_empty() {
                    return Err("field `images` must not be empty".to_string());
                }

                let bytes = bytes.to_vec();
                images.push(ByteArray(bytes));
                image_content_types.push(content_type);
            }
            _ => {}
        }
    }

    let target = target.ok_or_else(|| "missing field: target".to_string())?;
    let category = category.ok_or_else(|| "missing field: category".to_string())?;
    let title = title.ok_or_else(|| "missing field: title".to_string())?;
    let body = body.ok_or_else(|| "missing field: body".to_string())?;

    PatchNoteTarget::from_str(&target)
        .map_err(|error| format!("invalid field `target`: {error}"))?;

    PatchNoteCategory::from_str(&category)
        .map_err(|error| format!("invalid field `category`: {error}"))?;

    if title.trim().is_empty() {
        return Err("field `title` must not be empty".to_string());
    }
    if body.trim().is_empty() {
        return Err("field `body` must not be empty".to_string());
    }

    Ok((
        CreatePatchNoteRequest {
            target,
            category,
            title,
            body,
            images: (!images.is_empty()).then_some(images),
        },
        image_content_types,
    ))
}

fn is_supported_image_content_type(content_type: &str) -> bool {
    matches!(
        content_type,
        "image/jpeg" | "image/png" | "image/gif" | "image/webp"
    )
}

fn image_extension(content_type: &str) -> &'static str {
    match content_type {
        "image/jpeg" => ".jpg",
        "image/png" => ".png",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        _ => "",
    }
}

fn patch_note_from_record(
    record: PatchNoteRecord,
    image_urls: Vec<String>,
) -> ListPatchNotes200ResponseItemsInner {
    let PatchNoteRecord {
        id,
        target,
        category,
        title,
        body,
        created_at,
    } = record;

    ListPatchNotes200ResponseItemsInner::new(
        id, target, category, title, body, image_urls, created_at,
    )
}
