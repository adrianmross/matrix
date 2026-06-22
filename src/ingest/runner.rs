use anyhow::{Context, Result};
use serde_json::Value;

use super::{Source, string_field, test_run_fact};

pub(crate) fn normalize_tox(input: &str, source: &Source) -> Result<Vec<Value>> {
    let value: Value = serde_json::from_str(input).context("tox input was not valid JSON")?;
    let mut facts = Vec::new();
    if let Some(envs) = value
        .get("testenvs")
        .or_else(|| value.get("envs"))
        .and_then(Value::as_object)
    {
        for (name, env) in envs {
            facts.push(test_run_fact(source, "tox", "tox-env", name, env));
        }
    }
    if facts.is_empty() {
        facts.push(test_run_fact(
            source,
            "tox",
            "tox-run",
            &source.component,
            &value,
        ));
    }
    Ok(facts)
}

pub(crate) fn normalize_nox(input: &str, source: &Source) -> Result<Vec<Value>> {
    let value: Value = serde_json::from_str(input).context("nox input was not valid JSON")?;
    let sessions = value
        .get("sessions")
        .or_else(|| value.get("results"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if sessions.is_empty() {
        return Ok(vec![test_run_fact(
            source,
            "nox",
            "nox-run",
            &source.component,
            &value,
        )]);
    }
    Ok(sessions
        .iter()
        .enumerate()
        .map(|(index, session)| {
            let name = string_field(session, "name")
                .or_else(|| string_field(session, "session"))
                .unwrap_or_else(|| format!("session-{index}"));
            test_run_fact(source, "nox", "nox-session", &name, session)
        })
        .collect())
}
