use crate::api::Api;
use crate::api::filters::is_valid_half_open_range;
use crate::api::pagination::Cursor;
use crate::api::stream::{
    punishment_created_event, punishment_proof_created_event, punishment_proof_deleted_event,
    punishment_proof_updated_event, punishment_revoked_event, punishment_updated_event,
};
use crate::auth::ApiKeyScopeChecker;
use crate::records::{ProofRecord, PunishmentProofRecord, PunishmentRecord};
use async_trait::async_trait;
use axum_extra::extract::CookieJar;
use chrono::{DateTime, Utc};
use graph_api::apis::punishments::*;
use graph_api::models::*;
use graph_api::types::Nullable;
use headers::Host;
use http::Method;
use sqlx::{MySql, QueryBuilder};
use std::{collections::BTreeMap, net::IpAddr};
use uuid::Uuid;

const DEFAULT_LIMIT: u8 = 20;
const MAX_LIMIT: u8 = 100;
type PunishmentCursor = Cursor<i64, u64>;

fn can_read(key: &ApiKey) -> bool {
    key.has_scope(&ApiKeyScope::PunishmentsColonRead)
}

fn write_actor(key: &ApiKey) -> Option<Uuid> {
    if !key.has_scope(&ApiKeyScope::PunishmentsColonWrite) {
        return None;
    }
    match key.player_id {
        Nullable::Present(actor_id) => Some(actor_id),
        Nullable::Null => None,
    }
}

fn non_empty(value: &str) -> bool {
    !value.trim().is_empty()
}

fn punishable_ip(value: &str) -> bool {
    let Ok(ip) = value.parse::<IpAddr>() else {
        return false;
    };
    let IpAddr::V4(ip) = ip else {
        return true;
    };
    let [a, b, c, _] = ip.octets();
    !(a == 0
        || a == 10
        || (a == 100 && (64..=127).contains(&b))
        || a == 127
        || (a == 169 && b == 254)
        || (a == 192 && b == 0 && (c == 0 || c == 2))
        || (a == 192 && b == 88 && c == 99)
        || (a == 192 && b == 168)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 203 && b == 0 && c == 133)
        || (224..=239).contains(&a)
        || a >= 240)
}

fn normalize_target(kind: PunishmentType, target: &str) -> Option<String> {
    if is_ip_based(kind) {
        punishable_ip(target).then(|| {
            target
                .parse::<IpAddr>()
                .expect("validated IP address")
                .to_string()
        })
    } else {
        Uuid::parse_str(target)
            .ok()
            .map(|target| target.to_string())
    }
}

fn end_millis(
    kind: PunishmentType,
    expires_at: &Nullable<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<i64> {
    match (is_temporary(kind), expires_at) {
        (true, Nullable::Present(expires_at)) if *expires_at > now => {
            Some(expires_at.timestamp_millis())
        }
        (false, Nullable::Null) => Some(-1),
        _ => None,
    }
}

fn with_seen(extra: &str, value: bool) -> String {
    let mut flags = extra
        .split(',')
        .filter(|flag| !flag.is_empty() && *flag != "SEEN")
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if value {
        flags.push("SEEN".to_string());
    }
    flags.join(",")
}

fn is_temporary(kind: PunishmentType) -> bool {
    matches!(
        kind,
        PunishmentType::TempBan
            | PunishmentType::TempIpBan
            | PunishmentType::TempMute
            | PunishmentType::TempIpMute
    )
}

fn is_ip_based(kind: PunishmentType) -> bool {
    matches!(
        kind,
        PunishmentType::IpBan
            | PunishmentType::TempIpBan
            | PunishmentType::IpMute
            | PunishmentType::TempIpMute
    )
}

fn punishment_type_database_value(kind: PunishmentType) -> &'static str {
    match kind {
        PunishmentType::Ban => "BAN",
        PunishmentType::TempBan => "TEMP_BAN",
        PunishmentType::IpBan => "IP_BAN",
        PunishmentType::TempIpBan => "TEMP_IP_BAN",
        PunishmentType::Mute => "MUTE",
        PunishmentType::TempMute => "TEMP_MUTE",
        PunishmentType::IpMute => "IP_MUTE",
        PunishmentType::TempIpMute => "TEMP_IP_MUTE",
        PunishmentType::Warning => "WARNING",
        PunishmentType::Caution => "CAUTION",
        PunishmentType::Kick => "KICK",
        PunishmentType::Note => "NOTE",
    }
}

impl Api {
    async fn load_proofs(&self, punishment_id: i64) -> Result<Vec<Proof>, String> {
        sqlx::query_as::<_, ProofRecord>(
            "SELECT id, text, public FROM proofs WHERE punish_id = ? ORDER BY id",
        )
        .bind(punishment_id)
        .fetch_all(self.punishments_pool())
        .await
        .map_err(db_error)?
        .into_iter()
        .map(Proof::try_from)
        .collect()
    }

    async fn load_proofs_for_punishments(
        &self,
        punishment_ids: &[i64],
    ) -> Result<BTreeMap<i64, Vec<Proof>>, String> {
        if punishment_ids.is_empty() {
            return Ok(BTreeMap::new());
        }

        let mut query = QueryBuilder::<MySql>::new(
            "SELECT punish_id, id, text, public FROM proofs WHERE punish_id IN (",
        );
        let mut separated = query.separated(", ");
        for punishment_id in punishment_ids {
            separated.push_bind(punishment_id);
        }
        separated.push_unseparated(") ORDER BY punish_id, id");

        let rows = query
            .build_query_as::<PunishmentProofRecord>()
            .fetch_all(self.punishments_pool())
            .await
            .map_err(db_error)?;
        let mut proofs = BTreeMap::<i64, Vec<Proof>>::new();
        for row in rows {
            let punishment_id = row.punish_id;
            proofs
                .entry(punishment_id)
                .or_default()
                .push(row.into_proof()?);
        }
        Ok(proofs)
    }

    async fn fetch_punishment(&self, id: i64) -> Result<Option<Punishment>, String> {
        let record = sqlx::query_as::<_, PunishmentRecord>(
            "SELECT h.id, h.name, h.target, h.reason, h.operator, h.type, h.start, h.end, h.server, h.extra, (p.id IS NOT NULL) AS active, u.id AS revocation_id, u.reason AS revocation_reason, u.timestamp AS revocation_timestamp, u.operator AS revocation_operator FROM punishmentHistory h LEFT JOIN punishments p ON p.id = h.id LEFT JOIN unpunish u ON u.punish_id = h.id WHERE h.id = ? ORDER BY u.id DESC LIMIT 1"
        ).bind(id).fetch_optional(self.punishments_pool()).await.map_err(db_error)?;
        match record {
            Some(record) => {
                let proofs = self.load_proofs(record.id).await?;
                record.into_punishment(proofs).map(Some)
            }
            None => Ok(None),
        }
    }

    async fn punishment_exists(&self, id: i64, active: bool) -> Result<bool, String> {
        let row = if active {
            sqlx::query_scalar::<_, i64>("SELECT id FROM punishments WHERE id = ?")
                .bind(id)
                .fetch_optional(self.punishments_pool())
                .await
        } else {
            sqlx::query_scalar::<_, i64>("SELECT id FROM punishmentHistory WHERE id = ?")
                .bind(id)
                .fetch_optional(self.punishments_pool())
                .await
        };
        row.map(|row| row.is_some()).map_err(db_error)
    }

    async fn fetch_proof(
        &self,
        punishment_id: i64,
        proof_id: i64,
    ) -> Result<Option<Proof>, String> {
        sqlx::query_as::<_, ProofRecord>(
            "SELECT id, text, public FROM proofs WHERE id = ? AND punish_id = ?",
        )
        .bind(proof_id)
        .bind(punishment_id)
        .fetch_optional(self.punishments_pool())
        .await
        .map_err(db_error)?
        .map(Proof::try_from)
        .transpose()
    }
}

#[async_trait]
impl Punishments<String> for Api {
    type Claims = ApiKey;

    async fn create_punishment(
        &self,
        _: &Method,
        _: &Host,
        _: &CookieJar,
        key: &ApiKey,
        body: &CreatePunishmentRequest,
    ) -> Result<CreatePunishmentResponse, String> {
        let Some(actor_id) = write_actor(key) else {
            return Ok(
                CreatePunishmentResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope,
            );
        };
        let Ok(kind) = body.r_type.parse::<PunishmentType>() else {
            return Ok(CreatePunishmentResponse::Status400_TheRequestIsInvalid);
        };
        let now = Utc::now();
        let Some(end) = end_millis(kind, &body.expires_at, now) else {
            return Ok(CreatePunishmentResponse::Status400_TheRequestIsInvalid);
        };
        let Some(target) = normalize_target(kind, &body.target) else {
            return Ok(CreatePunishmentResponse::Status400_TheRequestIsInvalid);
        };
        if !non_empty(&body.target_name) || !non_empty(&body.reason) || !non_empty(&body.server) {
            return Ok(CreatePunishmentResponse::Status400_TheRequestIsInvalid);
        }
        let server = body.server.to_lowercase();
        let mut tx = self.punishments_pool.begin().await.map_err(db_error)?;
        let conflict = sqlx::query_scalar::<_, i64>("SELECT id FROM punishments WHERE target = ? AND type = ? AND server = ? LIMIT 1 FOR UPDATE")
            .bind(&target).bind(punishment_type_database_value(kind)).bind(&server).fetch_optional(&mut *tx).await.map_err(db_error)?;
        if conflict.is_some() {
            tx.rollback().await.map_err(db_error)?;
            return Ok(
                CreatePunishmentResponse::Status409_AConflictingActivePunishmentAlreadyExists,
            );
        }
        let result = sqlx::query("INSERT INTO punishmentHistory (name, target, reason, operator, type, start, end, server, extra) VALUES (?, ?, ?, ?, ?, ?, ?, ?, '')")
            .bind(&body.target_name).bind(&target).bind(&body.reason).bind(actor_id.to_string()).bind(punishment_type_database_value(kind))
            .bind(now.timestamp_millis()).bind(end).bind(&server).execute(&mut *tx).await.map_err(db_error)?;
        let id = i64::try_from(result.last_insert_id())
            .map_err(|_| "punishment ID exceeds signed 64-bit range".to_string())?;
        sqlx::query("INSERT INTO punishments (id, name, target, reason, operator, type, start, end, server, extra) SELECT id, name, target, reason, operator, type, start, end, server, extra FROM punishmentHistory WHERE id = ?")
            .bind(id).execute(&mut *tx).await.map_err(db_error)?;
        sqlx::query("INSERT INTO events (event_id, data, handled) VALUES ('add_punishment', ?, 1)")
            .bind(serde_json::json!({"id": id}).to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        let punishment = self
            .fetch_punishment(id)
            .await?
            .ok_or_else(|| "created punishment disappeared".to_string())?;
        self.publish_stream_event(punishment_created_event(punishment.clone()))
            .await;
        Ok(CreatePunishmentResponse::Status201_ThePunishmentWasCreatedSuccessfully(punishment))
    }

    async fn create_punishment_proof(
        &self,
        _: &Method,
        _: &Host,
        _: &CookieJar,
        key: &ApiKey,
        path: &CreatePunishmentProofPathParams,
        body: &CreatePunishmentProofRequest,
    ) -> Result<CreatePunishmentProofResponse, String> {
        if write_actor(key).is_none() {
            return Ok(CreatePunishmentProofResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope);
        }
        if !non_empty(&body.text) {
            return Ok(CreatePunishmentProofResponse::Status400_TheRequestIsInvalid);
        }
        let Ok(id) = i64::try_from(path.punishment_id) else {
            return Ok(CreatePunishmentProofResponse::Status404_TheActivePunishmentWasNotFound);
        };
        let mut tx = self.punishments_pool.begin().await.map_err(db_error)?;
        if sqlx::query_scalar::<_, i64>("SELECT id FROM punishments WHERE id = ? FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_error)?
            .is_none()
        {
            tx.rollback().await.map_err(db_error)?;
            return Ok(CreatePunishmentProofResponse::Status404_TheActivePunishmentWasNotFound);
        }
        let result = sqlx::query("INSERT INTO proofs (punish_id, text, public) VALUES (?, ?, ?)")
            .bind(id)
            .bind(&body.text)
            .bind(body.public)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        let proof_id = i64::try_from(result.last_insert_id())
            .map_err(|_| "proof ID exceeds signed 64-bit range".to_string())?;
        tx.commit().await.map_err(db_error)?;
        let proof = self
            .fetch_proof(id, proof_id)
            .await?
            .ok_or_else(|| "created proof disappeared".to_string())?;
        self.publish_stream_event(punishment_proof_created_event(
            path.punishment_id,
            proof.clone(),
        ))
        .await;
        Ok(CreatePunishmentProofResponse::Status201_TheProofWasCreatedSuccessfully(proof))
    }

    async fn delete_punishment_by_id(
        &self,
        _: &Method,
        _: &Host,
        _: &CookieJar,
        key: &ApiKey,
        path: &DeletePunishmentByIdPathParams,
        body: &DeletePunishmentByIdRequest,
    ) -> Result<DeletePunishmentByIdResponse, String> {
        let Some(actor_id) = write_actor(key) else {
            return Ok(
                DeletePunishmentByIdResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope,
            );
        };
        if !non_empty(&body.reason) {
            return Ok(DeletePunishmentByIdResponse::Status400_TheRequestIsInvalid);
        }
        let Ok(id) = i64::try_from(path.punishment_id) else {
            return Ok(DeletePunishmentByIdResponse::Status404_TheActivePunishmentWasNotFound);
        };
        let mut tx = self.punishments_pool.begin().await.map_err(db_error)?;
        let deleted = sqlx::query("DELETE FROM punishments WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?
            .rows_affected();
        if deleted == 0 {
            tx.rollback().await.map_err(db_error)?;
            return Ok(DeletePunishmentByIdResponse::Status404_TheActivePunishmentWasNotFound);
        }
        sqlx::query(
            "INSERT INTO unpunish (punish_id, reason, timestamp, operator) VALUES (?, ?, ?, ?)",
        )
        .bind(id)
        .bind(&body.reason)
        .bind(Utc::now().timestamp_millis())
        .bind(actor_id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        sqlx::query(
            "INSERT INTO events (event_id, data, handled) VALUES ('removed_punishment', ?, 1)",
        )
        .bind(serde_json::json!({"punish_id": id}).to_string())
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        let punishment = self
            .fetch_punishment(id)
            .await?
            .ok_or_else(|| "revoked punishment disappeared".to_string())?;
        self.publish_stream_event(punishment_revoked_event(punishment))
            .await;
        Ok(DeletePunishmentByIdResponse::Status204_ThePunishmentWasRevokedSuccessfully)
    }

    async fn delete_punishment_proof_by_id(
        &self,
        _: &Method,
        _: &Host,
        _: &CookieJar,
        key: &ApiKey,
        path: &DeletePunishmentProofByIdPathParams,
    ) -> Result<DeletePunishmentProofByIdResponse, String> {
        if write_actor(key).is_none() {
            return Ok(DeletePunishmentProofByIdResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope);
        }
        let (Ok(punishment_id), Ok(proof_id)) = (
            i64::try_from(path.punishment_id),
            i64::try_from(path.proof_id),
        ) else {
            return Ok(
                DeletePunishmentProofByIdResponse::Status404_ThePunishmentOrProofWasNotFound,
            );
        };
        let proof = self.fetch_proof(punishment_id, proof_id).await?;
        let Some(proof) = proof else {
            return Ok(
                DeletePunishmentProofByIdResponse::Status404_ThePunishmentOrProofWasNotFound,
            );
        };
        let affected = sqlx::query("DELETE FROM proofs WHERE id = ? AND punish_id = ?")
            .bind(proof_id)
            .bind(punishment_id)
            .execute(self.punishments_pool())
            .await
            .map_err(db_error)?
            .rows_affected();
        if affected == 0 {
            Ok(DeletePunishmentProofByIdResponse::Status404_ThePunishmentOrProofWasNotFound)
        } else {
            self.publish_stream_event(punishment_proof_deleted_event(path.punishment_id, proof))
                .await;
            Ok(DeletePunishmentProofByIdResponse::Status204_TheProofWasDeletedSuccessfully)
        }
    }

    async fn get_punishment_by_id(
        &self,
        _: &Method,
        _: &Host,
        _: &CookieJar,
        key: &ApiKey,
        path: &GetPunishmentByIdPathParams,
    ) -> Result<GetPunishmentByIdResponse, String> {
        if !can_read(key) {
            return Ok(
                GetPunishmentByIdResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope,
            );
        }
        let Ok(id) = i64::try_from(path.punishment_id) else {
            return Ok(GetPunishmentByIdResponse::Status404_ThePunishmentWasNotFound);
        };
        match self.fetch_punishment(id).await? {
            Some(value) => Ok(
                GetPunishmentByIdResponse::Status200_ThePunishmentWasRetrievedSuccessfully(value),
            ),
            None => Ok(GetPunishmentByIdResponse::Status404_ThePunishmentWasNotFound),
        }
    }

    async fn get_punishment_proof_by_id(
        &self,
        _: &Method,
        _: &Host,
        _: &CookieJar,
        key: &ApiKey,
        path: &GetPunishmentProofByIdPathParams,
    ) -> Result<GetPunishmentProofByIdResponse, String> {
        if !can_read(key) {
            return Ok(GetPunishmentProofByIdResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope);
        }
        let (Ok(punishment_id), Ok(proof_id)) = (
            i64::try_from(path.punishment_id),
            i64::try_from(path.proof_id),
        ) else {
            return Ok(GetPunishmentProofByIdResponse::Status404_ThePunishmentOrProofWasNotFound);
        };
        match self.fetch_proof(punishment_id, proof_id).await? {
            Some(proof) => Ok(
                GetPunishmentProofByIdResponse::Status200_TheProofWasRetrievedSuccessfully(proof),
            ),
            None => Ok(GetPunishmentProofByIdResponse::Status404_ThePunishmentOrProofWasNotFound),
        }
    }

    async fn list_punishment_proofs(
        &self,
        _: &Method,
        _: &Host,
        _: &CookieJar,
        key: &ApiKey,
        path: &ListPunishmentProofsPathParams,
    ) -> Result<ListPunishmentProofsResponse, String> {
        if !can_read(key) {
            return Ok(
                ListPunishmentProofsResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope,
            );
        }
        let Ok(id) = i64::try_from(path.punishment_id) else {
            return Ok(ListPunishmentProofsResponse::Status404_ThePunishmentWasNotFound);
        };
        if !self.punishment_exists(id, false).await? {
            return Ok(ListPunishmentProofsResponse::Status404_ThePunishmentWasNotFound);
        }
        Ok(
            ListPunishmentProofsResponse::Status200_TheProofsWereRetrievedSuccessfully(
                self.load_proofs(id).await?,
            ),
        )
    }

    async fn list_punishments(
        &self,
        _: &Method,
        _: &Host,
        _: &CookieJar,
        key: &ApiKey,
        query: &ListPunishmentsQueryParams,
    ) -> Result<graph_api::apis::punishments::ListPunishmentsResponse, String> {
        if !can_read(key) {
            return Ok(
                graph_api::apis::punishments::ListPunishmentsResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope,
            );
        }
        let limit = query.limit.unwrap_or(DEFAULT_LIMIT);
        if !(1..=MAX_LIMIT).contains(&limit)
            || !is_valid_half_open_range(query.created_from.as_ref(), query.created_to.as_ref())
            || !is_valid_half_open_range(query.expires_from.as_ref(), query.expires_to.as_ref())
            || !is_valid_half_open_range(query.revoked_from.as_ref(), query.revoked_to.as_ref())
        {
            return Ok(graph_api::apis::punishments::ListPunishmentsResponse::Status400_TheRequestIsInvalid);
        }
        let kind =
            match query.r#type.as_deref() {
                Some(value) => match value.parse::<PunishmentType>() {
                    Ok(kind) => Some(kind),
                    Err(_) => return Ok(
                        graph_api::apis::punishments::ListPunishmentsResponse::Status400_TheRequestIsInvalid,
                    ),
                },
                None => None,
            };
        let cursor =
            match query.cursor.as_deref() {
                Some(value) => match PunishmentCursor::decode(value) {
                    Ok(cursor) => Some(cursor),
                    Err(_) => return Ok(
                        graph_api::apis::punishments::ListPunishmentsResponse::Status400_TheRequestIsInvalid,
                    ),
                },
                None => None,
            };
        let mut builder = QueryBuilder::<MySql>::new(
            "SELECT h.id, h.name, h.target, h.reason, h.operator, h.type, h.start, h.end, h.server, h.extra, (p.id IS NOT NULL) AS active, u.id AS revocation_id, u.reason AS revocation_reason, u.timestamp AS revocation_timestamp, u.operator AS revocation_operator FROM punishmentHistory h LEFT JOIN punishments p ON p.id = h.id LEFT JOIN unpunish u ON u.punish_id = h.id WHERE 1=1",
        );
        if let Some(target) = &query.target {
            builder.push(" AND h.target = ").push_bind(target);
        }
        if let Some(kind) = kind {
            builder
                .push(" AND h.type = ")
                .push_bind(punishment_type_database_value(kind));
        }
        if let Some(server) = &query.server {
            builder
                .push(" AND h.server = ")
                .push_bind(server.to_lowercase());
        }
        if let Some(active) = query.active {
            if active {
                builder.push(" AND p.id IS NOT NULL");
            } else {
                builder.push(" AND p.id IS NULL");
            }
        }
        if let Some(created_from) = query.created_from {
            builder
                .push(" AND h.start >= ")
                .push_bind(created_from.timestamp_millis());
        }
        if let Some(created_to) = query.created_to {
            builder
                .push(" AND h.start < ")
                .push_bind(created_to.timestamp_millis());
        }
        if let Some(expires_from) = query.expires_from {
            builder
                .push(" AND h.end >= ")
                .push_bind(expires_from.timestamp_millis());
        }
        if let Some(expires_to) = query.expires_to {
            builder
                .push(" AND h.end < ")
                .push_bind(expires_to.timestamp_millis());
        }
        if let Some(revoked_from) = query.revoked_from {
            builder
                .push(" AND u.timestamp >= ")
                .push_bind(revoked_from.timestamp_millis());
        }
        if let Some(revoked_to) = query.revoked_to {
            builder
                .push(" AND u.timestamp < ")
                .push_bind(revoked_to.timestamp_millis());
        }
        if let Some(cursor) = &cursor {
            builder
                .push(" AND (h.start < ")
                .push_bind(cursor.value)
                .push(" OR (h.start = ")
                .push_bind(cursor.value)
                .push(" AND h.id < ")
                .push_bind(cursor.tie_breaker)
                .push("))");
        }
        builder
            .push(" ORDER BY h.start DESC, h.id DESC LIMIT ")
            .push_bind(u16::from(limit) + 1);
        let mut rows = builder
            .build_query_as::<PunishmentRecord>()
            .fetch_all(self.punishments_pool())
            .await
            .map_err(db_error)?;
        let next_cursor = if rows.len() > usize::from(limit) {
            rows.pop();
            rows.last()
                .map(|row| {
                    PunishmentCursor {
                        value: row.start,
                        tie_breaker: row.id as u64,
                    }
                    .encode()
                })
                .transpose()
                .map_err(|_| "failed to encode cursor".to_string())?
        } else {
            None
        };
        let punishment_ids = rows.iter().map(|row| row.id).collect::<Vec<_>>();
        let mut proofs = self.load_proofs_for_punishments(&punishment_ids).await?;
        let mut items = Vec::with_capacity(rows.len());
        for row in rows {
            let row_proofs = proofs.remove(&row.id).unwrap_or_default();
            items.push(row.into_punishment(row_proofs)?);
        }
        Ok(
            graph_api::apis::punishments::ListPunishmentsResponse::Status200_ThePunishmentsWereRetrievedSuccessfully(
                ListPunishments200Response::new(
                    items,
                    next_cursor.map_or(Nullable::Null, Nullable::Present),
                ),
            ),
        )
    }

    async fn update_punishment_by_id(
        &self,
        _: &Method,
        _: &Host,
        _: &CookieJar,
        key: &ApiKey,
        path: &UpdatePunishmentByIdPathParams,
        body: &UpdatePunishmentByIdRequest,
    ) -> Result<UpdatePunishmentByIdResponse, String> {
        if write_actor(key).is_none() {
            return Ok(
                UpdatePunishmentByIdResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope,
            );
        }
        if body.reason.is_none() && body.seen.is_none()
            || body
                .reason
                .as_deref()
                .is_some_and(|reason| !non_empty(reason))
        {
            return Ok(UpdatePunishmentByIdResponse::Status400_TheRequestIsInvalid);
        }
        let Ok(id) = i64::try_from(path.punishment_id) else {
            return Ok(UpdatePunishmentByIdResponse::Status404_TheActivePunishmentWasNotFound);
        };
        let mut tx = self.punishments_pool.begin().await.map_err(db_error)?;
        let current = sqlx::query_as::<_, (String, String)>(
            "SELECT reason, extra FROM punishments WHERE id = ? FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?;
        let Some((old_reason, old_extra)) = current else {
            tx.rollback().await.map_err(db_error)?;
            return Ok(UpdatePunishmentByIdResponse::Status404_TheActivePunishmentWasNotFound);
        };
        let reason = body.reason.as_ref().unwrap_or(&old_reason);
        let extra = body
            .seen
            .map_or(old_extra.clone(), |value| with_seen(&old_extra, value));
        sqlx::query("UPDATE punishmentHistory SET reason = ?, extra = ? WHERE id = ?")
            .bind(reason)
            .bind(&extra)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        sqlx::query("UPDATE punishments SET reason = ?, extra = ? WHERE id = ?")
            .bind(reason)
            .bind(&extra)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        sqlx::query(
            "INSERT INTO events (event_id, data, handled) VALUES ('updated_punishment', ?, 1)",
        )
        .bind(serde_json::json!({"id": id}).to_string())
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        let value = self
            .fetch_punishment(id)
            .await?
            .ok_or_else(|| "updated punishment disappeared".to_string())?;
        self.publish_stream_event(punishment_updated_event(value.clone()))
            .await;
        Ok(UpdatePunishmentByIdResponse::Status200_ThePunishmentWasUpdatedSuccessfully(value))
    }

    async fn update_punishment_proof_by_id(
        &self,
        _: &Method,
        _: &Host,
        _: &CookieJar,
        key: &ApiKey,
        path: &UpdatePunishmentProofByIdPathParams,
        body: &UpdatePunishmentProofByIdRequest,
    ) -> Result<UpdatePunishmentProofByIdResponse, String> {
        if write_actor(key).is_none() {
            return Ok(UpdatePunishmentProofByIdResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope);
        }
        if body.text.is_none() && body.public.is_none()
            || body.text.as_deref().is_some_and(|text| !non_empty(text))
        {
            return Ok(UpdatePunishmentProofByIdResponse::Status400_TheRequestIsInvalid);
        }
        let (Ok(punishment_id), Ok(proof_id)) = (
            i64::try_from(path.punishment_id),
            i64::try_from(path.proof_id),
        ) else {
            return Ok(
                UpdatePunishmentProofByIdResponse::Status404_ThePunishmentOrProofWasNotFound,
            );
        };
        let current = sqlx::query_as::<_, ProofRecord>(
            "SELECT id, text, public FROM proofs WHERE id = ? AND punish_id = ?",
        )
        .bind(proof_id)
        .bind(punishment_id)
        .fetch_optional(self.punishments_pool())
        .await
        .map_err(db_error)?;
        let Some(current) = current else {
            return Ok(
                UpdatePunishmentProofByIdResponse::Status404_ThePunishmentOrProofWasNotFound,
            );
        };
        sqlx::query("UPDATE proofs SET text = ?, public = ? WHERE id = ? AND punish_id = ?")
            .bind(body.text.as_ref().unwrap_or(&current.text))
            .bind(body.public.unwrap_or(current.public))
            .bind(proof_id)
            .bind(punishment_id)
            .execute(self.punishments_pool())
            .await
            .map_err(db_error)?;
        let proof = self
            .fetch_proof(punishment_id, proof_id)
            .await?
            .ok_or_else(|| "updated proof disappeared".to_string())?;
        self.publish_stream_event(punishment_proof_updated_event(
            path.punishment_id,
            proof.clone(),
        ))
        .await;
        Ok(UpdatePunishmentProofByIdResponse::Status200_TheProofWasUpdatedSuccessfully(proof))
    }
}

fn db_error(error: sqlx::Error) -> String {
    tracing::error!(%error, "punishments database operation failed");
    "punishments database operation failed".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mojang::MojangProfileResolver;
    use headers::Header;

    fn test_host() -> Host {
        let value = http::HeaderValue::from_static("localhost");
        Host::decode(&mut std::iter::once(&value)).unwrap()
    }

    #[test]
    fn all_api_types_map_to_database_values() {
        for (api, database) in [
            ("ban", "BAN"),
            ("tempBan", "TEMP_BAN"),
            ("ipBan", "IP_BAN"),
            ("tempIpBan", "TEMP_IP_BAN"),
            ("mute", "MUTE"),
            ("tempMute", "TEMP_MUTE"),
            ("ipMute", "IP_MUTE"),
            ("tempIpMute", "TEMP_IP_MUTE"),
            ("warning", "WARNING"),
            ("caution", "CAUTION"),
            ("kick", "KICK"),
            ("note", "NOTE"),
        ] {
            let kind = api.parse::<PunishmentType>().unwrap();
            assert_eq!(punishment_type_database_value(kind), database);
            assert_eq!(kind.to_string(), api);
        }
    }

    #[test]
    fn target_validation_matches_legacy_rules_and_normalizes_values() {
        assert_eq!(
            normalize_target(PunishmentType::Ban, "550E8400E29B41D4A716446655440000"),
            Some("550e8400-e29b-41d4-a716-446655440000".to_string())
        );
        assert_eq!(normalize_target(PunishmentType::Ban, "1.1.1.1"), None);
        assert_eq!(
            normalize_target(PunishmentType::IpBan, "1.1.1.1"),
            Some("1.1.1.1".to_string())
        );
        assert_eq!(
            normalize_target(PunishmentType::IpBan, "2001:4860:4860:0:0:0:0:8888"),
            Some("2001:4860:4860::8888".to_string())
        );
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "100.64.0.1",
            "169.254.1.1",
            "192.168.1.1",
            "203.0.133.1",
            "255.255.255.255",
        ] {
            assert!(!punishable_ip(ip), "{ip}");
        }
        assert!(punishable_ip("2001:4860:4860::8888"));
        assert!(!punishable_ip("not-an-ip"));
    }

    #[test]
    fn expiry_rules_and_seen_flag_are_strict() {
        let now = Utc::now();
        assert_eq!(
            end_millis(PunishmentType::Ban, &Nullable::Null, now),
            Some(-1)
        );
        assert!(
            end_millis(
                PunishmentType::TempBan,
                &Nullable::Present(now + chrono::Duration::seconds(1)),
                now
            )
            .is_some()
        );
        assert!(end_millis(PunishmentType::TempBan, &Nullable::Null, now).is_none());
        assert!(
            end_millis(
                PunishmentType::Ban,
                &Nullable::Present(now + chrono::Duration::seconds(1)),
                now
            )
            .is_none()
        );
        assert_eq!(with_seen("OTHER", true), "OTHER,SEEN");
        assert_eq!(with_seen("OTHER,SEEN", false), "OTHER");
    }

    async fn cleanup_targets(pool: &sqlx::MySqlPool, targets: &[String]) {
        for target in targets {
            let ids =
                sqlx::query_scalar::<_, i64>("SELECT id FROM punishmentHistory WHERE target = ?")
                    .bind(target)
                    .fetch_all(pool)
                    .await
                    .unwrap();
            for id in ids {
                sqlx::query("DELETE FROM proofs WHERE punish_id = ?")
                    .bind(id)
                    .execute(pool)
                    .await
                    .unwrap();
                sqlx::query("DELETE FROM unpunish WHERE punish_id = ?")
                    .bind(id)
                    .execute(pool)
                    .await
                    .unwrap();
                sqlx::query("DELETE FROM punishments WHERE id = ?")
                    .bind(id)
                    .execute(pool)
                    .await
                    .unwrap();
                sqlx::query("DELETE FROM punishmentHistory WHERE id = ?")
                    .bind(id)
                    .execute(pool)
                    .await
                    .unwrap();
                for event_id in ["add_punishment", "updated_punishment", "removed_punishment"] {
                    sqlx::query("DELETE FROM events WHERE event_id = ? AND (data = ? OR data = ?)")
                        .bind(event_id)
                        .bind(serde_json::json!({"id": id}).to_string())
                        .bind(serde_json::json!({"punish_id": id}).to_string())
                        .execute(pool)
                        .await
                        .unwrap();
                }
            }
        }
    }

    #[tokio::test]
    async fn mariadb_crud_cursor_events_and_rollback() {
        let Ok(database_url) = std::env::var("PUNISHMENTS_TEST_DATABASE_URL") else {
            eprintln!(
                "PUNISHMENTS_TEST_DATABASE_URL is not set; skipping MariaDB integration test"
            );
            return;
        };
        let pool = sqlx::MySqlPool::connect(&database_url).await.unwrap();
        let api = Api::new(
            pool.clone(),
            pool.clone(),
            None,
            MojangProfileResolver::new().unwrap(),
        );
        let actor_id = Uuid::new_v4();
        let key = ApiKey::new(
            "punishment integration key".to_string(),
            "integration".to_string(),
            Nullable::Present(actor_id),
            vec![
                "punishments:read".to_string(),
                "punishments:write".to_string(),
            ],
            Utc::now(),
            Nullable::Null,
        );
        let target_one = Uuid::new_v4().to_string();
        let target_two = Uuid::new_v4().to_string();
        let rollback_target = Uuid::new_v4().to_string();
        let targets = vec![
            target_one.clone(),
            target_two.clone(),
            rollback_target.clone(),
        ];
        cleanup_targets(&pool, &targets).await;
        sqlx::query("DROP TRIGGER IF EXISTS graph_test_fail_events")
            .execute(&pool)
            .await
            .unwrap();

        let host = test_host();
        let cookies = CookieJar::new();
        let create = |target: String| {
            CreatePunishmentRequest::new(
                "IntegrationTarget".to_string(),
                target,
                "ban".to_string(),
                "integration reason".to_string(),
                Nullable::Null,
                "TeStServer".to_string(),
            )
        };

        let first = match api
            .create_punishment(
                &Method::POST,
                &host,
                &cookies,
                &key,
                &create(target_one.clone()),
            )
            .await
            .unwrap()
        {
            CreatePunishmentResponse::Status201_ThePunishmentWasCreatedSuccessfully(value) => value,
            response => panic!("unexpected response: {response:?}"),
        };
        assert!(first.active);
        assert_eq!(first.server, "testserver");
        assert_eq!(first.actor_id, actor_id);
        assert_eq!(first.expires_at, Nullable::Null);

        assert!(matches!(
            api.create_punishment(
                &Method::POST,
                &host,
                &cookies,
                &key,
                &create(target_one.clone())
            )
            .await
            .unwrap(),
            CreatePunishmentResponse::Status409_AConflictingActivePunishmentAlreadyExists
        ));

        std::thread::sleep(std::time::Duration::from_millis(2));
        let second = match api
            .create_punishment(
                &Method::POST,
                &host,
                &cookies,
                &key,
                &create(target_two.clone()),
            )
            .await
            .unwrap()
        {
            CreatePunishmentResponse::Status201_ThePunishmentWasCreatedSuccessfully(value) => value,
            response => panic!("unexpected response: {response:?}"),
        };

        let page_one = match api
            .list_punishments(
                &Method::GET,
                &host,
                &cookies,
                &key,
                &ListPunishmentsQueryParams {
                    target: None,
                    r#type: Some("ban".to_string()),
                    server: Some("TESTSERVER".to_string()),
                    active: Some(true),
                    created_from: None,
                    created_to: None,
                    expires_from: None,
                    expires_to: None,
                    revoked_from: None,
                    revoked_to: None,
                    cursor: None,
                    limit: Some(1),
                },
            )
            .await
            .unwrap()
        {
            graph_api::apis::punishments::ListPunishmentsResponse::Status200_ThePunishmentsWereRetrievedSuccessfully(value) => value,
            response => panic!("unexpected response: {response:?}"),
        };
        assert_eq!(page_one.items.len(), 1);
        let cursor = match page_one.next_cursor {
            Nullable::Present(cursor) => cursor,
            Nullable::Null => panic!("expected a next cursor"),
        };
        let page_two = match api
            .list_punishments(
                &Method::GET,
                &host,
                &cookies,
                &key,
                &ListPunishmentsQueryParams {
                    target: None,
                    r#type: Some("ban".to_string()),
                    server: Some("testserver".to_string()),
                    active: Some(true),
                    created_from: None,
                    created_to: None,
                    expires_from: None,
                    expires_to: None,
                    revoked_from: None,
                    revoked_to: None,
                    cursor: Some(cursor),
                    limit: Some(1),
                },
            )
            .await
            .unwrap()
        {
            graph_api::apis::punishments::ListPunishmentsResponse::Status200_ThePunishmentsWereRetrievedSuccessfully(value) => value,
            response => panic!("unexpected response: {response:?}"),
        };
        assert_eq!(page_two.items.len(), 1);
        assert_ne!(page_one.items[0].id, page_two.items[0].id);

        let mut update = UpdatePunishmentByIdRequest::new();
        update.reason = Some("updated integration reason".to_string());
        update.seen = Some(true);
        let updated = match api
            .update_punishment_by_id(
                &Method::PATCH,
                &host,
                &cookies,
                &key,
                &UpdatePunishmentByIdPathParams {
                    punishment_id: first.id,
                },
                &update,
            )
            .await
            .unwrap()
        {
            UpdatePunishmentByIdResponse::Status200_ThePunishmentWasUpdatedSuccessfully(value) => {
                value
            }
            response => panic!("unexpected response: {response:?}"),
        };
        assert_eq!(updated.reason, "updated integration reason");
        assert!(updated.seen);
        let copies = sqlx::query_as::<_, (String, String)>(
            "SELECT reason, extra FROM punishmentHistory WHERE id = ? UNION ALL SELECT reason, extra FROM punishments WHERE id = ?",
        )
        .bind(first.id as i64)
        .bind(first.id as i64)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            copies,
            vec![("updated integration reason".to_string(), "SEEN".to_string()); 2]
        );

        let proof = match api
            .create_punishment_proof(
                &Method::POST,
                &host,
                &cookies,
                &key,
                &CreatePunishmentProofPathParams {
                    punishment_id: first.id,
                },
                &CreatePunishmentProofRequest::new("integration proof".to_string(), false),
            )
            .await
            .unwrap()
        {
            CreatePunishmentProofResponse::Status201_TheProofWasCreatedSuccessfully(value) => value,
            response => panic!("unexpected response: {response:?}"),
        };

        assert!(matches!(
            api.delete_punishment_by_id(
                &Method::DELETE,
                &host,
                &cookies,
                &key,
                &DeletePunishmentByIdPathParams {
                    punishment_id: first.id
                },
                &DeletePunishmentByIdRequest::new("integration revocation".to_string()),
            )
            .await
            .unwrap(),
            DeletePunishmentByIdResponse::Status204_ThePunishmentWasRevokedSuccessfully
        ));
        let revoked = match api
            .get_punishment_by_id(
                &Method::GET,
                &host,
                &cookies,
                &key,
                &GetPunishmentByIdPathParams {
                    punishment_id: first.id,
                },
            )
            .await
            .unwrap()
        {
            GetPunishmentByIdResponse::Status200_ThePunishmentWasRetrievedSuccessfully(value) => {
                value
            }
            response => panic!("unexpected response: {response:?}"),
        };
        assert!(!revoked.active);
        match revoked.revocation {
            Nullable::Present(revocation) => {
                assert_eq!(revocation.reason, "integration revocation");
                assert_eq!(revocation.actor_id, actor_id);
            }
            Nullable::Null => panic!("expected revocation"),
        }

        let mut proof_update = UpdatePunishmentProofByIdRequest::new();
        proof_update.text = Some("updated integration proof".to_string());
        proof_update.public = Some(true);
        assert!(matches!(
            api.update_punishment_proof_by_id(
                &Method::PATCH,
                &host,
                &cookies,
                &key,
                &UpdatePunishmentProofByIdPathParams {
                    punishment_id: first.id,
                    proof_id: proof.id
                },
                &proof_update,
            )
            .await
            .unwrap(),
            UpdatePunishmentProofByIdResponse::Status200_TheProofWasUpdatedSuccessfully(_)
        ));
        assert!(matches!(
            api.create_punishment_proof(
                &Method::POST,
                &host,
                &cookies,
                &key,
                &CreatePunishmentProofPathParams {
                    punishment_id: first.id
                },
                &CreatePunishmentProofRequest::new("too late".to_string(), false),
            )
            .await
            .unwrap(),
            CreatePunishmentProofResponse::Status404_TheActivePunishmentWasNotFound
        ));
        assert!(matches!(
            api.delete_punishment_proof_by_id(
                &Method::DELETE,
                &host,
                &cookies,
                &key,
                &DeletePunishmentProofByIdPathParams {
                    punishment_id: first.id,
                    proof_id: proof.id
                },
            )
            .await
            .unwrap(),
            DeletePunishmentProofByIdResponse::Status204_TheProofWasDeletedSuccessfully
        ));

        let events = sqlx::query_as::<_, (String, String, bool)>(
            "SELECT event_id, data, handled FROM events WHERE data IN (?, ?, ?) ORDER BY id",
        )
        .bind(serde_json::json!({"id": first.id}).to_string())
        .bind(serde_json::json!({"punish_id": first.id}).to_string())
        .bind(serde_json::json!({"id": second.id}).to_string())
        .fetch_all(&pool)
        .await
        .unwrap();
        let punishment_event_data = serde_json::json!({"id": first.id}).to_string();
        let removal_event_data = serde_json::json!({"punish_id": first.id}).to_string();
        assert!(events.iter().any(|event| event.0 == "add_punishment"
            && event.1 == punishment_event_data
            && event.2));
        assert!(events.iter().any(|event| event.0 == "updated_punishment"
            && event.1 == punishment_event_data
            && event.2));
        assert!(events.iter().any(|event| event.0 == "removed_punishment"
            && event.1 == removal_event_data
            && event.2));

        sqlx::query("CREATE TRIGGER graph_test_fail_events BEFORE INSERT ON events FOR EACH ROW SIGNAL SQLSTATE '45000' SET MESSAGE_TEXT = 'graph rollback test'")
            .execute(&pool).await.unwrap();
        let rollback_result = api
            .create_punishment(
                &Method::POST,
                &host,
                &cookies,
                &key,
                &create(rollback_target.clone()),
            )
            .await;
        sqlx::query("DROP TRIGGER graph_test_fail_events")
            .execute(&pool)
            .await
            .unwrap();
        assert!(rollback_result.is_err());
        let rollback_rows =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM punishmentHistory WHERE target = ?")
                .bind(&rollback_target)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(rollback_rows, 0);

        cleanup_targets(&pool, &targets).await;
    }
}
