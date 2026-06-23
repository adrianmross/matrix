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
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracing_subscriber::{EnvFilter, fmt};

type SharedDb = Arc<Mutex<Connection>>;
const DEFAULT_FACT_LIMIT: usize = 100;
const DEFAULT_HISTORY_LIMIT: usize = 25;
const MAX_PAGE_LIMIT: usize = 200;

#[derive(Clone)]
struct AppState {
    db: SharedDb,
    auth_token: Option<String>,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
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

#[derive(Debug, Deserialize, Default, Clone)]
struct FactHistoryQuery {
    history: Option<bool>,
    limit: Option<usize>,
    cursor: Option<String>,
    revision: Option<i64>,
    #[serde(rename = "eventId")]
    event_id: Option<String>,
    relative: Option<i64>,
    // Deprecated API aliases. Prefer revision/eventId with relative.
    #[serde(rename = "fromRevision")]
    from_revision: Option<i64>,
    #[serde(rename = "fromEvent")]
    from_event: Option<String>,
    #[serde(rename = "asOf")]
    as_of: Option<String>,
}

#[derive(Debug, Serialize)]
struct FactPage {
    facts: Vec<Value>,
    page: PageInfo,
}

#[derive(Debug, Serialize)]
struct FactHistoryPage {
    #[serde(rename = "factId")]
    fact_id: String,
    events: Vec<Value>,
    page: PageInfo,
}

#[derive(Debug, Serialize)]
struct PageInfo {
    limit: usize,
    #[serde(rename = "maxLimit")]
    max_limit: usize,
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
        auth_token: env::var("MATRIX_CONSTRUCT_TOKEN")
            .ok()
            .filter(|token| !token.trim().is_empty()),
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
        .route("/readyz", get(readyz))
        .route("/openapi.json", get(openapi))
        .route("/v1/matrix", get(overview))
        .route("/v1/matrix/openapi.json", get(openapi))
        .route("/v1/matrix/zones/{zone}", get(zone))
        .route("/v1/matrix/zones/{zone}/gates/{level}", get(gate))
        .route("/v1/matrix/zones/{zone}/candidates/{level}", get(candidate))
        .route("/v1/matrix/facts", get(facts).post(upload_facts))
        .route("/v1/matrix/facts/latest", get(latest_fact))
        .route("/v1/matrix/facts/{id}", get(fact_get))
        .route("/v1/matrix/facts/{id}/history", get(fact_history))
        // Compatibility aliases for adapters migrating from track-based APIs.
        .route("/v1/compatibility", get(overview))
        .route("/v1/compatibility/openapi.json", get(openapi))
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
        .route("/v1/compatibility/facts/{id}", get(fact_get))
        .route("/v1/compatibility/facts/{id}/history", get(fact_history))
        .with_state(state)
}

async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok", "service": "matrix-construct" }))
}

async fn readyz(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let db = state.db.lock().map_err(internal)?;
    db.query_row("select 1", [], |_row| Ok(()))
        .map_err(internal)?;
    Ok(Json(json!({
        "status": "ready",
        "service": "matrix-construct",
        "storage": "sqlite"
    })))
}

async fn openapi() -> Result<Json<Value>, ApiError> {
    serde_json::from_str(include_str!("../openapi.json"))
        .map(Json)
        .map_err(internal)
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
            limit: Some(DEFAULT_FACT_LIMIT),
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
    normalized.limit = Some(DEFAULT_FACT_LIMIT);
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

async fn fact_get(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<FactHistoryQuery>,
) -> Result<Json<Value>, ApiError> {
    let db = state.db.lock().map_err(internal)?;
    if query.history.unwrap_or(false) {
        return Ok(Json(
            serde_json::to_value(query_fact_history(&db, &id, &query)?).map_err(internal)?,
        ));
    }

    let mut selected = query.clone();
    if !selected.has_selector() {
        selected.relative = Some(0);
    }
    let page = query_fact_history(&db, &id, &selected)?;
    let event = page
        .events
        .into_iter()
        .next()
        .ok_or_else(|| not_found(format!("fact {id:?} was not found")))?;
    Ok(Json(fact_get_response(&id, event)))
}

async fn fact_history(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<FactHistoryQuery>,
) -> Result<Json<FactHistoryPage>, ApiError> {
    let db = state.db.lock().map_err(internal)?;
    Ok(Json(query_fact_history(&db, &id, &query)?))
}

async fn upload_facts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    authorize_write(&state, &headers)?;
    let facts = extract_facts(body)?;
    let mut db = state.db.lock().map_err(internal)?;
    let tx = db.transaction().map_err(internal)?;
    let mut accepted = 0usize;
    let mut duplicates = 0usize;
    for fact in facts {
        match upsert_fact(&tx, fact)? {
            UpsertOutcome::Accepted => accepted += 1,
            UpsertOutcome::Duplicate => duplicates += 1,
        }
    }
    tx.commit().map_err(internal)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "accepted": accepted, "duplicates": duplicates })),
    ))
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
            accepted_at text,
            content_hash text,
            json text not null
        );
        create table if not exists fact_events (
            event_id text primary key,
            fact_id text not null,
            revision integer not null,
            accepted_at text not null,
            content_hash text not null,
            source_repository text,
            source_sha text,
            source_ref text,
            json text not null,
            unique(fact_id, revision)
        );
        create index if not exists idx_facts_zone on facts(zone, observed_at desc);
        create index if not exists idx_facts_subject on facts(subject_type, subject_name);
        create index if not exists idx_facts_source on facts(source_repository, source_sha);
        create index if not exists idx_fact_events_fact_revision on fact_events(fact_id, revision desc);
        create index if not exists idx_fact_events_accepted on fact_events(accepted_at desc);",
    )?;
    add_column_if_missing(db, "facts", "accepted_at", "text")?;
    add_column_if_missing(db, "facts", "content_hash", "text")?;
    backfill_fact_events(db)?;
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

enum UpsertOutcome {
    Accepted,
    Duplicate,
}

fn upsert_fact(db: &Connection, fact: Value) -> Result<UpsertOutcome, ApiError> {
    let id = fact
        .get("id")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .unwrap_or_else(|| generated_id(&fact));
    let zone = text_at(&fact, &["zone"])
        .or_else(|| text_at(&fact, &["track"]))
        .ok_or_else(|| bad_request(format!("fact {id:?} is missing zone")))?;
    let accepted_at = Utc::now().to_rfc3339();
    let observed_at = text_at(&fact, &["observedAt"]).unwrap_or_else(|| accepted_at.clone());
    let serialized = serde_json::to_string(&fact).map_err(internal)?;
    let content_hash = content_hash(&serialized);
    let existing_content_hash = db
        .query_row(
            "select content_hash from facts where id = ?1",
            [&id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(internal)?
        .flatten();
    if existing_content_hash.as_deref() == Some(content_hash.as_str()) {
        return Ok(UpsertOutcome::Duplicate);
    }
    insert_fact_event(db, &id, &accepted_at, &content_hash, &fact, &serialized)?;
    db.execute(
        "insert into facts (
            id, zone, kind, status, source_repository, source_sha,
            subject_type, subject_name, channel, observed_at, accepted_at, content_hash, json
        ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
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
            accepted_at = excluded.accepted_at,
            content_hash = excluded.content_hash,
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
            observed_at,
            accepted_at,
            content_hash,
            serialized,
        ],
    )
    .map_err(internal)?;
    Ok(UpsertOutcome::Accepted)
}

fn insert_fact_event(
    db: &Connection,
    fact_id: &str,
    accepted_at: &str,
    content_hash: &str,
    fact: &Value,
    serialized: &str,
) -> Result<(), ApiError> {
    let revision = db
        .query_row(
            "select coalesce(max(revision), 0) + 1 from fact_events where fact_id = ?1",
            [fact_id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(internal)?;
    db.execute(
        "insert into fact_events (
            event_id, fact_id, revision, accepted_at, content_hash,
            source_repository, source_sha, source_ref, json
        ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            event_id(fact_id, revision, accepted_at, content_hash),
            fact_id,
            revision,
            accepted_at,
            content_hash,
            text_at(fact, &["sourceRepository"]).or_else(|| text_at(fact, &["source", "repo"])),
            text_at(fact, &["sourceSha"]).or_else(|| text_at(fact, &["source", "sha"])),
            text_at(fact, &["sourceRef"]).or_else(|| text_at(fact, &["source", "ref"])),
            serialized,
        ],
    )
    .map_err(internal)?;
    Ok(())
}

fn query_facts(db: &Connection, query: &FactQuery) -> Result<FactPage, ApiError> {
    let limit = bounded_limit(query.limit, DEFAULT_FACT_LIMIT);
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
            max_limit: MAX_PAGE_LIMIT,
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
            "select json, accepted_at, content_hash from facts
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
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        },
    )
    .map_err(internal)?
    .map(|row| {
        let (text, accepted_at, content_hash) = row.map_err(internal)?;
        let mut value = serde_json::from_str::<Value>(&text).map_err(internal)?;
        if let Some(object) = value.as_object_mut() {
            if let Some(accepted_at) = accepted_at {
                object
                    .entry("acceptedAt")
                    .or_insert_with(|| Value::String(accepted_at));
            }
            if let Some(content_hash) = content_hash {
                object
                    .entry("contentHash")
                    .or_insert_with(|| Value::String(content_hash));
            }
        }
        Ok(value)
    })
    .collect()
}

fn query_fact_history(
    db: &Connection,
    fact_id: &str,
    query: &FactHistoryQuery,
) -> Result<FactHistoryPage, ApiError> {
    let selected = query.selected_revision().map_err(bad_request)?;
    let limit = if selected.is_some() {
        1
    } else {
        bounded_limit(query.limit, DEFAULT_HISTORY_LIMIT)
    };
    let offset = if selected.is_some() {
        0
    } else {
        query
            .cursor
            .as_deref()
            .map(decode_cursor)
            .transpose()?
            .unwrap_or(0)
    };
    let mut stmt = db
        .prepare(
            "select event_id, fact_id, revision, accepted_at, content_hash,
                    source_repository, source_sha, source_ref, json
             from fact_events
             where fact_id = ?1
             order by revision asc",
        )
        .map_err(internal)?;
    let stored = stmt
        .query_map([fact_id], |row| {
            Ok(StoredFactEvent {
                event_id: row.get(0)?,
                fact_id: row.get(1)?,
                revision: row.get(2)?,
                accepted_at: row.get(3)?,
                content_hash: row.get(4)?,
                source_repository: row.get(5)?,
                source_sha: row.get(6)?,
                source_ref: row.get(7)?,
                json: row.get(8)?,
            })
        })
        .map_err(internal)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(internal)?;
    if stored.is_empty() {
        return Err(not_found(format!("fact {fact_id:?} has no history")));
    }

    let current_revision = stored.last().map(|event| event.revision).unwrap_or(0);
    let selected_index = match selected {
        Some(FactRevisionSelector::Revision(revision)) => Some(
            stored
                .iter()
                .position(|event| event.revision == revision)
                .ok_or_else(|| not_found(format!("fact {fact_id:?} has no revision {revision}")))?,
        ),
        Some(FactRevisionSelector::Event(event_id)) => Some(
            stored
                .iter()
                .position(|event| event.event_id == event_id)
                .ok_or_else(|| not_found(format!("fact {fact_id:?} has no event {event_id:?}")))?,
        ),
        Some(FactRevisionSelector::Relative {
            offset,
            base_revision,
            base_event_id,
        }) => {
            let base_revision = match (base_revision, base_event_id) {
                (Some(revision), None) => revision,
                (None, Some(event_id)) => stored
                    .iter()
                    .find(|event| event.event_id == event_id)
                    .map(|event| event.revision)
                    .ok_or_else(|| {
                        not_found(format!("fact {fact_id:?} has no event {event_id:?}"))
                    })?,
                (None, None) => current_revision,
                (Some(_), Some(_)) => {
                    return Err(bad_request("use only one of fromRevision or fromEvent"));
                }
            };
            let revision = base_revision + offset;
            Some(
                stored
                    .iter()
                    .position(|event| event.revision == revision)
                    .ok_or_else(|| {
                        not_found(format!(
                            "fact {fact_id:?} has no revision {revision} for relative offset {offset}"
                        ))
                    })?,
            )
        }
        Some(FactRevisionSelector::AsOf(as_of)) => {
            let as_of = parse_as_of(&as_of).map_err(bad_request)?;
            Some(
                stored
                    .iter()
                    .rposition(|event| {
                        parse_event_time(&event.accepted_at)
                            .map(|accepted_at| accepted_at <= as_of)
                            .unwrap_or(false)
                    })
                    .ok_or_else(|| {
                        not_found(format!(
                            "fact {fact_id:?} has no revision at or before {as_of}"
                        ))
                    })?,
            )
        }
        None => None,
    };
    let mut events = Vec::new();
    if let Some(index) = selected_index {
        events.push(stored[index].to_json(current_revision, stored.get(index + 1))?);
    } else {
        for index in 0..stored.len() {
            events.push(stored[index].to_json(current_revision, stored.get(index + 1))?);
        }
        events.reverse();
    }

    let total = events.len();
    let events = events
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();
    let next_offset = offset + events.len();
    let next_cursor = (next_offset < total).then(|| encode_cursor(next_offset));
    Ok(FactHistoryPage {
        fact_id: fact_id.to_string(),
        events,
        page: PageInfo {
            limit,
            max_limit: MAX_PAGE_LIMIT,
            total,
            next_cursor,
        },
    })
}

#[derive(Debug)]
struct StoredFactEvent {
    event_id: String,
    fact_id: String,
    revision: i64,
    accepted_at: String,
    content_hash: String,
    source_repository: Option<String>,
    source_sha: Option<String>,
    source_ref: Option<String>,
    json: String,
}

impl StoredFactEvent {
    fn to_json(
        &self,
        current_revision: i64,
        next: Option<&StoredFactEvent>,
    ) -> Result<Value, ApiError> {
        let current = self.revision == current_revision;
        Ok(json!({
            "eventId": self.event_id,
            "factId": self.fact_id,
            "revision": self.revision,
            "acceptedAt": self.accepted_at,
            "contentHash": self.content_hash,
            "sourceRepository": self.source_repository,
            "sourceSha": self.source_sha,
            "sourceRef": self.source_ref,
            "status": if current { "current" } else { "superseded" },
            "current": current,
            "supersededBy": next.map(|event| event.event_id.clone()),
            "supersededAt": next.map(|event| event.accepted_at.clone()),
            "fact": serde_json::from_str::<Value>(&self.json).map_err(internal)?,
        }))
    }
}

fn fact_get_response(fact_id: &str, mut event: Value) -> Value {
    let fact = event.get("fact").cloned().unwrap_or(Value::Null);
    if let Some(object) = event.as_object_mut() {
        object.remove("fact");
    }
    json!({
        "factId": fact_id,
        "event": event,
        "fact": fact,
    })
}

enum FactRevisionSelector {
    Revision(i64),
    Event(String),
    Relative {
        offset: i64,
        base_revision: Option<i64>,
        base_event_id: Option<String>,
    },
    AsOf(String),
}

impl FactHistoryQuery {
    fn selected_revision(&self) -> std::result::Result<Option<FactRevisionSelector>, String> {
        if self.as_of.is_some()
            && (self.revision.is_some() || self.event_id.is_some() || self.relative.is_some())
        {
            return Err("asOf cannot be combined with revision, eventId, or relative".to_string());
        }
        if self.revision.is_some() && self.event_id.is_some() {
            return Err("use only one of revision or eventId".to_string());
        }
        if (self.from_revision.is_some() || self.from_event.is_some()) && self.relative.is_none() {
            return Err("fromRevision and fromEvent require relative".to_string());
        }
        if self.from_revision.is_some() && self.from_event.is_some() {
            return Err("use only one of fromRevision or fromEvent".to_string());
        }
        if let Some(offset) = self.relative {
            Ok(Some(FactRevisionSelector::Relative {
                offset,
                base_revision: self.revision.or(self.from_revision),
                base_event_id: self.event_id.clone().or_else(|| self.from_event.clone()),
            }))
        } else if let Some(revision) = self.revision {
            Ok(Some(FactRevisionSelector::Revision(revision)))
        } else if let Some(event_id) = self.event_id.clone() {
            Ok(Some(FactRevisionSelector::Event(event_id)))
        } else {
            Ok(self.as_of.clone().map(FactRevisionSelector::AsOf))
        }
    }

    fn has_selector(&self) -> bool {
        self.revision.is_some()
            || self.event_id.is_some()
            || self.relative.is_some()
            || self.as_of.is_some()
            || self.from_revision.is_some()
            || self.from_event.is_some()
    }
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

fn add_column_if_missing(
    db: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<()> {
    let mut stmt = db.prepare(&format!("pragma table_info({table})"))?;
    let exists = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .any(|name| name == column);
    if !exists {
        db.execute(
            &format!("alter table {table} add column {column} {definition}"),
            [],
        )?;
    }
    Ok(())
}

fn backfill_fact_events(db: &Connection) -> Result<()> {
    let mut stmt = db.prepare(
        "select id, observed_at, source_repository, source_sha, accepted_at, content_hash, json
         from facts
         where not exists (
           select 1 from fact_events where fact_events.fact_id = facts.id
         )",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (fact_id, observed_at, source_repository, source_sha, accepted_at, existing_hash, json) in
        rows
    {
        let accepted_at = accepted_at
            .or(observed_at)
            .unwrap_or_else(|| Utc::now().to_rfc3339());
        let content_hash = existing_hash.unwrap_or_else(|| content_hash(&json));
        let event_id = event_id(&fact_id, 1, &accepted_at, &content_hash);
        db.execute(
            "insert or ignore into fact_events (
                event_id, fact_id, revision, accepted_at, content_hash,
                source_repository, source_sha, source_ref, json
             ) values (?1, ?2, 1, ?3, ?4, ?5, ?6, null, ?7)",
            params![
                &event_id,
                &fact_id,
                &accepted_at,
                &content_hash,
                &source_repository,
                &source_sha,
                &json,
            ],
        )?;
        db.execute(
            "update facts
             set accepted_at = coalesce(accepted_at, ?2),
                 content_hash = coalesce(content_hash, ?3)
             where id = ?1",
            params![&fact_id, &accepted_at, &content_hash],
        )?;
    }
    Ok(())
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

fn content_hash(serialized: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(serialized.as_bytes());
    format!("sha256:{}", URL_SAFE_NO_PAD.encode(hash.finalize()))
}

fn event_id(fact_id: &str, revision: i64, accepted_at: &str, content_hash: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(fact_id.as_bytes());
    hash.update(b"\0");
    hash.update(revision.to_string().as_bytes());
    hash.update(b"\0");
    hash.update(accepted_at.as_bytes());
    hash.update(b"\0");
    hash.update(content_hash.as_bytes());
    format!("event.{}", URL_SAFE_NO_PAD.encode(hash.finalize()))
}

fn parse_as_of(value: &str) -> std::result::Result<DateTime<Utc>, String> {
    if let Ok(value) = DateTime::parse_from_rfc3339(value) {
        return Ok(value.with_timezone(&Utc));
    }
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| "asOf must be RFC3339 timestamp or YYYY-MM-DD date".to_string())?;
    let date_time = date
        .and_hms_opt(23, 59, 59)
        .ok_or_else(|| "asOf date is invalid".to_string())?;
    Ok(DateTime::from_naive_utc_and_offset(date_time, Utc))
}

fn parse_event_time(value: &str) -> std::result::Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| error.to_string())
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

fn bounded_limit(limit: Option<usize>, default: usize) -> usize {
    limit.unwrap_or(default).clamp(1, MAX_PAGE_LIMIT)
}

fn authorize_write(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let Some(expected) = state.auth_token.as_deref() else {
        return Ok(());
    };
    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim);
    if bearer == Some(expected) {
        return Ok(());
    }
    Err(ApiError {
        status: StatusCode::UNAUTHORIZED,
        code: "unauthorized",
        message: "write authorization is required".to_string(),
    })
}

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError {
        status: StatusCode::BAD_REQUEST,
        code: "bad_request",
        message: message.into(),
    }
}

fn not_found(message: impl Into<String>) -> ApiError {
    ApiError {
        status: StatusCode::NOT_FOUND,
        code: "not_found",
        message: message.into(),
    }
}

fn internal(error: impl std::fmt::Display) -> ApiError {
    ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "internal_error",
        message: error.to_string(),
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({
                "error": {
                    "code": self.code,
                    "message": self.message,
                    "status": self.status.as_u16()
                }
            })),
        )
            .into_response()
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
        assert_eq!(page.page.limit, DEFAULT_FACT_LIMIT);
        assert_eq!(page.page.max_limit, MAX_PAGE_LIMIT);
        assert!(page.facts[0]["acceptedAt"].is_string());
        assert!(page.facts[0]["contentHash"].is_string());
    }

    #[test]
    fn duplicate_fact_content_is_idempotent() {
        let db = Connection::open_in_memory().unwrap();
        init_db(&db).unwrap();
        let fact = json!({"id":"a","zone":"sdk","status":"passed"});

        assert!(matches!(
            upsert_fact(&db, fact.clone()).unwrap(),
            UpsertOutcome::Accepted
        ));
        assert!(matches!(
            upsert_fact(&db, fact).unwrap(),
            UpsertOutcome::Duplicate
        ));

        let history = query_fact_history(
            &db,
            "a",
            &FactHistoryQuery {
                limit: Some(10),
                ..FactHistoryQuery::default()
            },
        )
        .unwrap();
        assert_eq!(history.events.len(), 1);
    }

    #[test]
    fn write_auth_is_optional_but_enforced_when_configured() {
        let db = Connection::open_in_memory().unwrap();
        let unauthenticated = AppState {
            db: Arc::new(Mutex::new(db)),
            auth_token: None,
        };
        assert!(authorize_write(&unauthenticated, &HeaderMap::new()).is_ok());

        let authenticated = AppState {
            db: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            auth_token: Some("secret".to_string()),
        };
        assert!(authorize_write(&authenticated, &HeaderMap::new()).is_err());

        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer secret".parse().unwrap(),
        );
        assert!(authorize_write(&authenticated, &headers).is_ok());
    }

    #[test]
    fn openapi_spec_is_valid_json_and_lists_core_paths() {
        let spec: Value = serde_json::from_str(include_str!("../openapi.json")).unwrap();
        assert_eq!(spec["openapi"], "3.1.0");
        assert!(spec["paths"]["/healthz"].is_object());
        assert!(spec["paths"]["/readyz"].is_object());
        assert!(spec["paths"]["/v1/matrix/facts"].is_object());
        assert!(spec["paths"]["/v1/matrix/facts/{id}/history"].is_object());
        assert!(spec["components"]["schemas"]["ErrorResponse"].is_object());
    }

    #[test]
    fn preserves_fact_history_on_update() {
        let db = Connection::open_in_memory().unwrap();
        init_db(&db).unwrap();
        upsert_fact(
            &db,
            json!({
                "id":"tuple.api.1.0.0",
                "zone":"runtime",
                "status":"candidate",
                "sourceRepository":"example/api",
                "sourceSha":"111",
                "members":[{"component":"api","version":"1.0.0"}]
            }),
        )
        .unwrap();
        upsert_fact(
            &db,
            json!({
                "id":"tuple.api.1.0.0",
                "zone":"runtime",
                "status":"passed",
                "sourceRepository":"example/api",
                "sourceSha":"222",
                "members":[{"component":"api","version":"1.0.1"}]
            }),
        )
        .unwrap();

        let history = query_fact_history(
            &db,
            "tuple.api.1.0.0",
            &FactHistoryQuery {
                limit: Some(10),
                ..FactHistoryQuery::default()
            },
        )
        .unwrap();
        assert_eq!(history.events.len(), 2);
        assert_eq!(history.events[0]["revision"], 2);
        assert_eq!(history.events[0]["status"], "current");
        assert_eq!(history.events[0]["fact"]["status"], "passed");
        assert_eq!(history.events[1]["revision"], 1);
        assert_eq!(history.events[1]["status"], "superseded");
        assert_eq!(history.events[1]["fact"]["status"], "candidate");
        assert!(history.events[1]["supersededBy"].is_string());

        let revision_one = query_fact_history(
            &db,
            "tuple.api.1.0.0",
            &FactHistoryQuery {
                revision: Some(1),
                ..FactHistoryQuery::default()
            },
        )
        .unwrap();
        assert_eq!(revision_one.events.len(), 1);
        assert_eq!(revision_one.events[0]["revision"], 1);
        assert_eq!(revision_one.events[0]["fact"]["status"], "candidate");

        let previous = query_fact_history(
            &db,
            "tuple.api.1.0.0",
            &FactHistoryQuery {
                relative: Some(-1),
                ..FactHistoryQuery::default()
            },
        )
        .unwrap();
        assert_eq!(previous.events[0]["revision"], 1);

        let previous_from_revision = query_fact_history(
            &db,
            "tuple.api.1.0.0",
            &FactHistoryQuery {
                revision: Some(2),
                relative: Some(-1),
                ..FactHistoryQuery::default()
            },
        )
        .unwrap();
        assert_eq!(previous_from_revision.events[0]["revision"], 1);

        let current_event_id = history.events[0]["eventId"].as_str().unwrap().to_string();
        let by_event = query_fact_history(
            &db,
            "tuple.api.1.0.0",
            &FactHistoryQuery {
                event_id: Some(current_event_id.clone()),
                ..FactHistoryQuery::default()
            },
        )
        .unwrap();
        assert_eq!(by_event.events[0]["revision"], 2);

        let previous_from_event = query_fact_history(
            &db,
            "tuple.api.1.0.0",
            &FactHistoryQuery {
                event_id: Some(current_event_id),
                relative: Some(-1),
                ..FactHistoryQuery::default()
            },
        )
        .unwrap();
        assert_eq!(previous_from_event.events[0]["revision"], 1);

        let future = query_fact_history(
            &db,
            "tuple.api.1.0.0",
            &FactHistoryQuery {
                as_of: Some("2999-01-01".to_string()),
                ..FactHistoryQuery::default()
            },
        )
        .unwrap();
        assert_eq!(future.events[0]["revision"], 2);

        let current = query_fact_history(
            &db,
            "tuple.api.1.0.0",
            &FactHistoryQuery {
                relative: Some(0),
                ..FactHistoryQuery::default()
            },
        )
        .unwrap();
        let response = fact_get_response("tuple.api.1.0.0", current.events[0].clone());
        assert_eq!(response["factId"], "tuple.api.1.0.0");
        assert_eq!(response["event"]["revision"], 2);
        assert_eq!(response["fact"]["status"], "passed");
    }
}
