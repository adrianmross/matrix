use std::{fs, path::PathBuf};

use anyhow::{Context, Result, bail};
use glob::glob;
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub(crate) struct Source {
    pub(crate) adapter: String,
    pub(crate) zone: String,
    pub(crate) repo: Option<String>,
    pub(crate) component: String,
    pub(crate) version: Option<String>,
    pub(crate) sha: Option<String>,
    pub(crate) reference: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct Request {
    pub(crate) adapter: String,
    pub(crate) input: String,
    pub(crate) source: Source,
    pub(crate) junit_files: Vec<PathBuf>,
    pub(crate) junit_globs: Vec<String>,
}

pub(crate) fn normalize(request: Request) -> Result<Vec<Value>> {
    let mut facts = normalize_adapter_input(&request.adapter, &request.input, &request.source)?;
    append_junit_facts(&mut facts, &request)?;
    Ok(facts)
}

fn append_junit_facts(facts: &mut Vec<Value>, request: &Request) -> Result<()> {
    let junit_paths = junit_input_paths(request)?;
    if junit_paths.is_empty() {
        return Ok(());
    }
    if !matches!(request.source.adapter.as_str(), "tox" | "nox") {
        bail!("--junit-file and --junit-glob are only supported with tox or nox ingest");
    }

    let mut junit_source = request.source.clone();
    junit_source.adapter = "junit".to_string();
    for path in junit_paths {
        let input = fs::read_to_string(&path)
            .with_context(|| format!("failed to read JUnit file {}", path.display()))?;
        facts.extend(
            junit::normalize(&input, &junit_source)
                .with_context(|| format!("failed to normalize JUnit file {}", path.display()))?,
        );
    }
    Ok(())
}

fn junit_input_paths(request: &Request) -> Result<Vec<PathBuf>> {
    let mut paths = std::collections::BTreeSet::new();
    paths.extend(request.junit_files.iter().cloned());
    for pattern in &request.junit_globs {
        for entry in
            glob(pattern).with_context(|| format!("invalid --junit-glob pattern {pattern:?}"))?
        {
            paths.insert(
                entry.with_context(|| {
                    format!("failed to read --junit-glob match for {pattern:?}")
                })?,
            );
        }
    }
    Ok(paths.into_iter().collect())
}

fn normalize_adapter_input(adapter: &str, input: &str, source: &Source) -> Result<Vec<Value>> {
    match normalize_adapter(adapter)?.as_str() {
        "junit" => junit::normalize(input, source),
        "sbom" => sbom::normalize(input, source),
        "tox" => runner::normalize_tox(input, source),
        "nox" => runner::normalize_nox(input, source),
        "k6" => test_stage::normalize_k6(input, source),
        "microcks" => test_stage::normalize_microcks(input, source),
        _ => unreachable!("normalize_adapter rejects unsupported adapters"),
    }
}

pub(crate) fn normalize_adapter(adapter: &str) -> Result<String> {
    let normalized = adapter.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "junit" | "sbom" | "tox" | "nox" | "k6" | "microcks" => Ok(normalized),
        _ => bail!(
            "unsupported ingest adapter {adapter:?}; supported adapters: junit, sbom, tox, nox, k6, microcks"
        ),
    }
}

pub(crate) fn default_zone(adapter: &str) -> &'static str {
    match adapter {
        "sbom" => "supply-chain",
        "junit" | "tox" | "nox" | "k6" | "microcks" => "test",
        _ => "evidence",
    }
}

mod junit;
mod runner;
mod sbom;
mod test_stage;

fn test_run_fact(
    source: &Source,
    adapter: &str,
    subject_type: &str,
    name: &str,
    evidence: &Value,
) -> Value {
    let status = test_status(evidence);
    let mut fact = base_fact(
        source,
        format!("{}.{}.{}", adapter, slug(&source.component), slug(name)),
        subject_type,
        name.to_string(),
        source.version.clone(),
        status,
    );
    put_array(
        &mut fact,
        "provides",
        vec![json!({
            "capability": format!("test:{adapter}"),
            "version": source.version,
        })],
    );
    put_object(&mut fact, "result", evidence.clone());
    fact
}

fn test_status(value: &Value) -> &'static str {
    if let Some(status) = string_field(value, "status")
        .or_else(|| string_field(value, "result"))
        .or_else(|| string_field(value, "outcome"))
    {
        let status = status.to_ascii_lowercase();
        if matches!(status.as_str(), "pass" | "passed" | "success" | "ok") {
            return "passed";
        }
        if matches!(
            status.as_str(),
            "fail" | "failed" | "failure" | "error" | "errored"
        ) {
            return "failed";
        }
    }
    if let Some(success) = value.get("success").and_then(Value::as_bool) {
        return if success { "passed" } else { "failed" };
    }
    if let Some(retcode) = value
        .get("retcode")
        .or_else(|| value.get("returncode"))
        .or_else(|| value.get("exit_code"))
        .and_then(Value::as_i64)
    {
        return if retcode == 0 { "passed" } else { "failed" };
    }
    "observed"
}

fn base_fact(
    source: &Source,
    id: String,
    subject_type: &str,
    subject_name: String,
    subject_version: Option<String>,
    status: &str,
) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("id".to_string(), json!(id));
    map.insert("zone".to_string(), json!(source.zone));
    map.insert("kind".to_string(), json!("evidence"));
    map.insert("status".to_string(), json!(status));
    map.insert("subjectType".to_string(), json!(subject_type));
    map.insert("subjectName".to_string(), json!(subject_name));
    map.insert("canonicalComponent".to_string(), json!(source.component));
    map.insert(
        "subject".to_string(),
        json_strip_nulls(json!({
            "type": subject_type,
            "name": subject_name,
            "version": subject_version,
            "canonicalComponent": source.component,
            "repo": source.repo,
        })),
    );
    map.insert(
        "source".to_string(),
        json_strip_nulls(json!({
            "adapter": source.adapter,
            "repo": source.repo,
            "sha": source.sha,
            "ref": source.reference,
        })),
    );
    put_optional(&mut map, "subjectVersion", subject_version);
    put_optional(&mut map, "sourceRepository", source.repo.clone());
    put_optional(&mut map, "sourceSha", source.sha.clone());
    put_optional(&mut map, "sourceRef", source.reference.clone());
    Value::Object(map)
}

fn put_optional(map: &mut serde_json::Map<String, Value>, key: &str, value: Option<String>) {
    if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
        map.insert(key.to_string(), Value::String(value));
    }
}

fn put_array(fact: &mut Value, key: &str, value: Vec<Value>) {
    if !value.is_empty()
        && let Some(map) = fact.as_object_mut()
    {
        map.insert(key.to_string(), Value::Array(value));
    }
}

fn put_object(fact: &mut Value, key: &str, value: Value) {
    if !value.is_null()
        && let Some(map) = fact.as_object_mut()
    {
        map.insert(key.to_string(), value);
    }
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn json_strip_nulls(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter_map(|(key, value)| {
                    let value = json_strip_nulls(value);
                    (!value.is_null()).then_some((key, value))
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(json_strip_nulls).collect()),
        value => value,
    }
}

fn slug(value: &str) -> String {
    let slug = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if slug.is_empty() {
        "unknown".to_string()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MatrixContext, build_facts_db, execute_readonly_sql};

    fn fixture_source(adapter: &str) -> Source {
        Source {
            adapter: adapter.to_string(),
            zone: default_zone(adapter).to_string(),
            repo: Some("example/payments-api".to_string()),
            component: "payments-api".to_string(),
            version: Some("1.2.3".to_string()),
            sha: Some("abc123".to_string()),
            reference: Some("refs/heads/main".to_string()),
        }
    }

    #[test]
    fn normalizes_junit_as_validation_facts() {
        let source = fixture_source("junit");
        let facts =
            junit::normalize(include_str!("../../fixtures/ingest/junit.xml"), &source).unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0]["zone"], "test");
        assert_eq!(facts[0]["status"], "failed");
        assert_eq!(facts[0]["subjectType"], "junit-suite");
        assert_eq!(facts[0]["subjectName"], "api-contract");
        assert_eq!(facts[0]["members"].as_array().unwrap().len(), 2);

        let db = build_facts_db(&facts, &MatrixContext::default()).unwrap();
        let rows = execute_readonly_sql(
            &db,
            "select component, version, status from components where type==junit-suite",
        )
        .unwrap();
        assert_eq!(rows["rows"][0]["component"], "api-contract");
        assert_eq!(rows["rows"][0]["version"], "1.2.3");
        assert_eq!(rows["rows"][0]["status"], "failed");
    }

    #[test]
    fn normalizes_cyclonedx_sbom_to_root_and_package_facts() {
        let source = fixture_source("sbom");
        let facts = sbom::normalize(
            include_str!("../../fixtures/ingest/cyclonedx.json"),
            &source,
        )
        .unwrap();
        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0]["zone"], "supply-chain");
        assert_eq!(facts[0]["subjectType"], "sbom");
        assert_eq!(facts[0]["members"].as_array().unwrap().len(), 1);
        assert_eq!(facts[1]["subjectType"], "dependency");
        assert_eq!(facts[1]["subjectName"], "serde");

        let db = build_facts_db(&facts, &MatrixContext::default()).unwrap();
        let capabilities = execute_readonly_sql(
            &db,
            "select capability, capability_version from capabilities order by capability",
        )
        .unwrap();
        assert!(
            capabilities["rows"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["capability"] == "package:serde"
                    && row["capability_version"] == "1.0.219")
        );
    }

    #[test]
    fn normalizes_tox_and_nox_sessions() {
        let tox = runner::normalize_tox(
            include_str!("../../fixtures/ingest/tox.json"),
            &fixture_source("tox"),
        )
        .unwrap();
        assert_eq!(tox.len(), 2);
        assert!(tox.iter().any(|fact| fact["status"] == "passed"));
        assert!(tox.iter().any(|fact| fact["status"] == "failed"));

        let nox = runner::normalize_nox(
            include_str!("../../fixtures/ingest/nox.json"),
            &fixture_source("nox"),
        )
        .unwrap();
        assert_eq!(nox.len(), 2);
        assert_eq!(nox[0]["subjectType"], "nox-session");
        assert_eq!(nox[1]["status"], "failed");
    }

    #[test]
    fn tox_nox_can_attach_canonical_junit_facts() {
        let source = fixture_source("tox");
        let request = Request {
            adapter: "tox".to_string(),
            input: include_str!("../../fixtures/ingest/tox.json").to_string(),
            source,
            junit_files: vec![PathBuf::from("fixtures/ingest/junit.xml")],
            junit_globs: Vec::new(),
        };
        let facts = normalize(request).unwrap();

        assert!(facts.iter().any(|fact| fact["subjectType"] == "tox-env"));
        assert!(
            facts
                .iter()
                .any(|fact| fact["subjectType"] == "junit-suite")
        );

        let db = build_facts_db(&facts, &MatrixContext::default()).unwrap();
        let rows = execute_readonly_sql(
            &db,
            "select type, component, status from components where type in ('tox-env', 'junit-suite') order by type, component",
        )
        .unwrap();
        assert_eq!(rows["rows"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn normalizes_test_stage_adapters() {
        let k6 = test_stage::normalize_k6(
            include_str!("../../fixtures/ingest/k6.json"),
            &fixture_source("k6"),
        )
        .unwrap();
        assert_eq!(k6.len(), 1);
        assert_eq!(k6[0]["subjectType"], "load-test");
        assert_eq!(k6[0]["status"], "failed");
        assert_eq!(k6[0]["members"].as_array().unwrap().len(), 2);

        let microcks = test_stage::normalize_microcks(
            include_str!("../../fixtures/ingest/microcks.json"),
            &fixture_source("microcks"),
        )
        .unwrap();
        assert_eq!(microcks.len(), 1);
        assert_eq!(microcks[0]["subjectType"], "api-contract-test");
        assert_eq!(microcks[0]["status"], "passed");
    }
}
