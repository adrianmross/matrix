use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::{Source, base_fact, put_array, put_object, slug, string_field};

pub(crate) fn normalize_k6(input: &str, source: &Source) -> Result<Vec<Value>> {
    let value: Value = serde_json::from_str(input).context("k6 input was not valid JSON")?;
    let metrics = value
        .get("metrics")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let failed = metrics.values().any(metric_threshold_failed);
    let mut fact = base_fact(
        source,
        format!("k6.{}", slug(&source.component)),
        "load-test",
        source.component.clone(),
        source.version.clone(),
        if failed { "failed" } else { "passed" },
    );
    put_array(
        &mut fact,
        "provides",
        vec![json!({
            "capability": "test:k6",
            "version": source.version,
        })],
    );
    put_array(
        &mut fact,
        "members",
        metrics
            .iter()
            .map(|(name, metric)| {
                json!({
                    "component": name,
                    "status": if metric_threshold_failed(metric) { "failed" } else { "passed" },
                    "value": metric.get("value"),
                    "rate": metric.get("rate"),
                    "thresholds": metric.get("thresholds"),
                })
            })
            .collect(),
    );
    Ok(vec![fact])
}

pub(crate) fn normalize_microcks(input: &str, source: &Source) -> Result<Vec<Value>> {
    let value: Value = serde_json::from_str(input).context("Microcks input was not valid JSON")?;
    let name = string_field(&value, "name")
        .or_else(|| string_field(&value, "serviceName"))
        .or_else(|| {
            value
                .pointer("/testResult/serviceName")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| source.component.clone());
    let success = value
        .get("success")
        .and_then(Value::as_bool)
        .or_else(|| {
            value
                .pointer("/testResult/success")
                .and_then(Value::as_bool)
        })
        .unwrap_or_else(|| {
            !matches!(
                string_field(&value, "status")
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .as_str(),
                "failed" | "failure" | "error"
            )
        });
    let mut fact = base_fact(
        source,
        format!("microcks.{}", slug(&name)),
        "api-contract-test",
        name.clone(),
        source.version.clone(),
        if success { "passed" } else { "failed" },
    );
    put_array(
        &mut fact,
        "provides",
        vec![json!({
            "capability": "test:microcks",
            "version": source.version,
            "service": name,
        })],
    );
    put_object(&mut fact, "result", value);
    Ok(vec![fact])
}

fn metric_threshold_failed(metric: &Value) -> bool {
    metric
        .get("thresholds")
        .and_then(Value::as_object)
        .map(|thresholds| {
            thresholds.values().any(|threshold| {
                threshold
                    .get("ok")
                    .and_then(Value::as_bool)
                    .map(|ok| !ok)
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}
