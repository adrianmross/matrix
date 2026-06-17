use std::{
    env,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Utc;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracing_subscriber::{EnvFilter, fmt};

type SharedDb = Arc<Mutex<Connection>>;

#[derive(Clone)]
struct AppState {
    db: SharedDb,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

#[derive(Debug, Deserialize, Default)]
struct FactQuery {
    zone: Option<String>,
    track: Option<String>,
    kind: Option<String>,
    id: Option<String>,
    status: Option<String>,
    #[serde(rename = "sourceRepository")]
    source_repository: Option<String>,
    #[serde(rename = "subjectType")]
    subject_type: Option<String>,
    #[serde(rename = "subjectName")]
    subject_name: Option<String>,
    channel: Option<String>,
    limit: Option<usize>,
    cursor: Option<String>,
}

#[derive(Debug, Serialize)]
struct FactPage {
    facts: Vec<Value>,
    page: PageInfo,
}

#[derive(Debug, Serialize)]
struct PageInfo {
    limit: usize,
    total: usize,
    #[serde(rename = "nextCursor")]
    next_cursor: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    if env::args()
        .nth(1)
        .is_some_and(|arg| matches!(arg.as_str(), "--version" | "-V"))
    {
        println!("matrix-construct {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .json()
        .init();

    let db_path = env::var("MATRIX_CONSTRUCT_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(":memory:"));
    let conn = Connection::open(&db_path)
        .with_context(|| format!("failed to open database {}", db_path.display()))?;
    init_db(&conn)?;

    let state = AppState {
        db: Arc::new(Mutex::new(conn)),
    };
    let app = router(state);
    let addr: SocketAddr = env::var("MATRIX_CONSTRUCT_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8080".to_string())
        .parse()
        .context("MATRIX_CONSTRUCT_ADDR must be a socket address")?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "matrix construct listening");
    axum::serve(listener, app).await?;
    Ok(())
}

fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/matrix", get(overview))
        .route("/v1/matrix/zones/{zone}", get(zone))
        .route("/v1/matrix/zones/{zone}/gates/{level}", get(gate))
        .route("/v1/matrix/zones/{zone}/candidates/{level}", get(candidate))
        .route("/v1/matrix/facts", get(facts).post(upload_facts))
        .route("/v1/matrix/facts/latest", get(latest_fact))
        // Compatibility aliases for adapters migrating from track-based APIs.
        .route("/v1/compatibility", get(overview))
        .route("/v1/compatibility/tracks/{zone}", get(zone))
        .route(
            "/v1/compatibility/tracks/{zone}/promotion-gates/{level}",
            get(gate),
        )
        .route(
            "/v1/compatibility/tracks/{zone}/promotion-candidates/{level}",
            get(candidate),
        )
        .route("/v1/compatibility/facts", get(facts).post(upload_facts))
        .route("/v1/compatibility/facts/latest", get(latest_fact))
        .with_state(state)
}

async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

async fn overview(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let db = state.db.lock().map_err(internal)?;
    let mut stmt = db
        .prepare("select distinct zone from facts order by zone")
        .map_err(internal)?;
    let zones = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(internal)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(internal)?;
    Ok(Json(json!({
        "generatedAt": Utc::now(),
        "zones": zones,
    })))
}

async fn zone(
    State(state): State<AppState>,
    Path(zone): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let db = state.db.lock().map_err(internal)?;
    let facts = query_facts(
        &db,
        &FactQuery {
            zone: Some(zone.clone()),
            limit: Some(100),
            ..FactQuery::default()
        },
    )?;
    if facts.facts.is_empty() {
        return Err(not_found(format!("zone {zone:?} was not found")));
    }
    Ok(Json(json!({
        "zone": zone,
        "facts": facts.facts,
        "page": facts.page,
    })))
}

async fn gate(
    State(state): State<AppState>,
    Path((zone, level)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let db = state.db.lock().map_err(internal)?;
    let failed = count_by_zone_status(&db, &zone, "failed")?;
    let total = count_by_zone(&db, &zone)?;
    if total == 0 {
        return Err(not_found(format!("zone {zone:?} was not found")));
    }
    Ok(Json(json!({
        "zone": zone,
        "level": level,
        "gate": {
            "eligible": failed == 0,
            "status": if failed == 0 { "passed" } else { "failed" },
            "failedFacts": failed,
            "totalFacts": total
        }
    })))
}

async fn candidate(
    State(state): State<AppState>,
    Path((zone, level)): Path<(String, String)>,
    Query(query): Query<FactQuery>,
) -> Result<Json<Value>, ApiError> {
    let db = state.db.lock().map_err(internal)?;
    let mut normalized = query;
    normalized.zone = Some(zone.clone());
    normalized.limit = Some(100);
    let facts = query_facts(&db, &normalized)?;
    Ok(Json(json!({
        "zone": zone,
        "level": level,
        "candidate": {
            "eligible": facts.facts.iter().all(|fact| fact.get("status").and_then(Value::as_str) != Some("failed")),
            "facts": facts.facts
        }
    })))
}

async fn facts(
    State(state): State<AppState>,
    Query(query): Query<FactQuery>,
) -> Result<Json<FactPage>, ApiError> {
    let db = state.db.lock().map_err(internal)?;
    Ok(Json(query_facts(&db, &query)?))
}

async fn latest_fact(
    State(state): State<AppState>,
    Query(mut query): Query<FactQuery>,
) -> Result<Json<Value>, ApiError> {
    query.limit = Some(1);
    let db = state.db.lock().map_err(internal)?;
    let page = query_facts(&db, &query)?;
    Ok(Json(json!({ "fact": page.facts.into_iter().next() })))
}

async fn upload_facts(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let facts = extract_facts(body)?;
    let mut db = state.db.lock().map_err(internal)?;
    let tx = db.transaction().map_err(internal)?;
    let mut accepted = 0usize;
    for fact in facts {
        upsert_fact(&tx, fact)?;
        accepted += 1;
    }
    tx.commit().map_err(internal)?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "accepted": accepted }))))
}

fn init_db(db: &Connection) -> Result<()> {
    db.execute_batch(
        "create table if not exists facts (
            id text primary key,
            zone text not null,
            kind text,
            status text,
            source_repository text,
            source_sha text,
            subject_type text,
            subject_name text,
            channel text,
            observed_at text,
            json text not null
        );
        create index if not exists idx_facts_zone on facts(zone, observed_at desc);
        create index if not exists idx_facts_subject on facts(subject_type, subject_name);
        create index if not exists idx_facts_source on facts(source_repository, source_sha);",
    )?;
    Ok(())
}

fn extract_facts(body: Value) -> Result<Vec<Value>, ApiError> {
    if let Some(facts) = body.get("facts").and_then(Value::as_array) {
        return Ok(facts.clone());
    }
    if let Some(fact) = body.get("fact") {
        return Ok(vec![fact.clone()]);
    }
    if body.is_object() {
        return Ok(vec![body]);
    }
    Err(bad_request("expected an object, fact, or facts array"))
}

fn upsert_fact(db: &Connection, fact: Value) -> Result<(), ApiError> {
    let id = fact
        .get("id")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .unwrap_or_else(|| generated_id(&fact));
    let zone = text_at(&fact, &["zone"])
        .or_else(|| text_at(&fact, &["track"]))
        .ok_or_else(|| bad_request(format!("fact {id:?} is missing zone")))?;
    db.execute(
        "insert into facts (
            id, zone, kind, status, source_repository, source_sha,
            subject_type, subject_name, channel, observed_at, json
        ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
        on conflict(id) do update set
            zone = excluded.zone,
            kind = excluded.kind,
            status = excluded.status,
            source_repository = excluded.source_repository,
            source_sha = excluded.source_sha,
            subject_type = excluded.subject_type,
            subject_name = excluded.subject_name,
            channel = excluded.channel,
            observed_at = excluded.observed_at,
            json = excluded.json",
        params![
            id,
            zone,
            text_at(&fact, &["kind"]),
            text_at(&fact, &["status"]),
            text_at(&fact, &["sourceRepository"]).or_else(|| text_at(&fact, &["source", "repo"])),
            text_at(&fact, &["sourceSha"]).or_else(|| text_at(&fact, &["source", "sha"])),
            text_at(&fact, &["subjectType"]).or_else(|| text_at(&fact, &["subject", "type"])),
            text_at(&fact, &["subjectName"]).or_else(|| text_at(&fact, &["subject", "name"])),
            text_at(&fact, &["channel"]),
            text_at(&fact, &["observedAt"]).unwrap_or_else(|| Utc::now().to_rfc3339()),
            serde_json::to_string(&fact).map_err(internal)?,
        ],
    )
    .map_err(internal)?;
    Ok(())
}

fn query_facts(db: &Connection, query: &FactQuery) -> Result<FactPage, ApiError> {
    let limit = query.limit.unwrap_or(100).clamp(1, 200);
    let offset = query
        .cursor
        .as_deref()
        .map(decode_cursor)
        .transpose()?
        .unwrap_or(0);
    let zone = query.zone.clone().or_else(|| query.track.clone());
    let filters = FactFilters {
        zone,
        kind: query.kind.clone(),
        id: query.id.clone(),
        status: query.status.clone(),
        source_repository: query.source_repository.clone(),
        subject_type: query.subject_type.clone(),
        subject_name: query.subject_name.clone(),
        channel: query.channel.clone(),
    };
    let all = select_matching(db, &filters)?;
    let total = all.len();
    let facts = all.into_iter().skip(offset).take(limit).collect::<Vec<_>>();
    let next_offset = offset + facts.len();
    let next_cursor = (next_offset < total).then(|| encode_cursor(next_offset));
    Ok(FactPage {
        facts,
        page: PageInfo {
            limit,
            total,
            next_cursor,
        },
    })
}

#[derive(Default)]
struct FactFilters {
    zone: Option<String>,
    kind: Option<String>,
    id: Option<String>,
    status: Option<String>,
    source_repository: Option<String>,
    subject_type: Option<String>,
    subject_name: Option<String>,
    channel: Option<String>,
}

fn select_matching(db: &Connection, filters: &FactFilters) -> Result<Vec<Value>, ApiError> {
    let mut stmt = db
        .prepare(
            "select json from facts
             where (?1 is null or zone = ?1)
               and (?2 is null or kind = ?2)
               and (?3 is null or id = ?3)
               and (?4 is null or status = ?4)
               and (?5 is null or source_repository = ?5)
               and (?6 is null or subject_type = ?6)
               and (?7 is null or subject_name = ?7)
               and (?8 is null or channel = ?8)
             order by observed_at desc, id asc",
        )
        .map_err(internal)?;
    stmt.query_map(
        params![
            filters.zone,
            filters.kind,
            filters.id,
            filters.status,
            filters.source_repository,
            filters.subject_type,
            filters.subject_name,
            filters.channel,
        ],
        |row| row.get::<_, String>(0),
    )
    .map_err(internal)?
    .map(|row| {
        let text = row.map_err(internal)?;
        serde_json::from_str(&text).map_err(internal)
    })
    .collect()
}

fn count_by_zone(db: &Connection, zone: &str) -> Result<usize, ApiError> {
    db.query_row(
        "select count(*) from facts where zone = ?1",
        [zone],
        |row| row.get(0),
    )
    .map_err(internal)
}

fn count_by_zone_status(db: &Connection, zone: &str, status: &str) -> Result<usize, ApiError> {
    db.query_row(
        "select count(*) from facts where zone = ?1 and status = ?2",
        params![zone, status],
        |row| row.get(0),
    )
    .map_err(internal)
}

fn text_at(value: &Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_str().map(ToString::to_string)
}

fn generated_id(value: &Value) -> String {
    let mut hash = Sha256::new();
    hash.update(serde_json::to_vec(value).unwrap_or_default());
    format!("fact.{}", URL_SAFE_NO_PAD.encode(hash.finalize()))
}

fn encode_cursor(offset: usize) -> String {
    URL_SAFE_NO_PAD.encode(offset.to_string())
}

fn decode_cursor(cursor: &str) -> Result<usize, ApiError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| bad_request("cursor is invalid"))?;
    let text = String::from_utf8(bytes).map_err(|_| bad_request("cursor is invalid"))?;
    text.parse::<usize>()
        .map_err(|_| bad_request("cursor is invalid"))
}

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError {
        status: StatusCode::BAD_REQUEST,
        message: message.into(),
    }
}

fn not_found(message: impl Into<String>) -> ApiError {
    ApiError {
        status: StatusCode::NOT_FOUND,
        message: message.into(),
    }
}

fn internal(error: impl std::fmt::Display) -> ApiError {
    ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: error.to_string(),
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_are_stable() {
        let value = json!({"zone":"sdk","subject":{"name":"pkg"}});
        assert_eq!(generated_id(&value), generated_id(&value));
    }

    #[test]
    fn cursor_roundtrips() {
        let cursor = encode_cursor(42);
        assert_eq!(decode_cursor(&cursor).unwrap(), 42);
    }

    #[test]
    fn stores_and_queries_facts() {
        let db = Connection::open_in_memory().unwrap();
        init_db(&db).unwrap();
        upsert_fact(&db, json!({"id":"a","zone":"sdk","status":"passed"})).unwrap();
        let page = query_facts(
            &db,
            &FactQuery {
                zone: Some("sdk".to_string()),
                ..FactQuery::default()
            },
        )
        .unwrap();
        assert_eq!(page.facts.len(), 1);
    }
}
