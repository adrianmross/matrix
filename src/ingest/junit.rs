use anyhow::{Result, bail};
use quick_xml::{Reader, events::Event};
use serde_json::{Value, json};

use super::{Source, base_fact, put_array, put_object, slug};

#[derive(Clone, Debug, Default)]
struct Suite {
    name: String,
    tests: u64,
    failures: u64,
    errors: u64,
    skipped: u64,
    time: Option<String>,
    cases: Vec<Case>,
}

#[derive(Clone, Debug, Default)]
struct Case {
    name: String,
    classname: Option<String>,
    time: Option<String>,
    status: String,
}

pub(crate) fn normalize(input: &str, source: &Source) -> Result<Vec<Value>> {
    let suites = parse_suites(input)?;
    if suites.is_empty() {
        bail!("JUnit input did not include any testsuite elements");
    }
    Ok(suites
        .into_iter()
        .map(|suite| {
            let failed = suite.failures + suite.errors > 0;
            let suite_name = if suite.name.is_empty() {
                source.component.clone()
            } else {
                suite.name.clone()
            };
            let version = source.version.clone();
            let mut fact = base_fact(
                source,
                format!(
                    "junit.{}.{}{}",
                    slug(&source.component),
                    slug(&suite_name),
                    version
                        .as_deref()
                        .map(|value| format!(".{}", slug(value)))
                        .unwrap_or_default()
                ),
                "junit-suite",
                suite_name,
                version,
                if failed { "failed" } else { "passed" },
            );
            put_array(
                &mut fact,
                "provides",
                vec![json!({
                    "capability": "test:junit",
                    "version": source.version,
                    "suite": suite.name,
                })],
            );
            put_array(
                &mut fact,
                "members",
                suite
                    .cases
                    .into_iter()
                    .map(|case| {
                        json!({
                            "component": case.name,
                            "status": case.status,
                            "className": case.classname,
                            "time": case.time,
                        })
                    })
                    .collect(),
            );
            put_object(
                &mut fact,
                "metrics",
                json!({
                    "tests": suite.tests,
                    "failures": suite.failures,
                    "errors": suite.errors,
                    "skipped": suite.skipped,
                    "time": suite.time,
                }),
            );
            fact
        })
        .collect())
}

fn parse_suites(input: &str) -> Result<Vec<Suite>> {
    let mut reader = Reader::from_str(input);
    reader.config_mut().trim_text(true);
    let mut suites = Vec::new();
    let mut current_suite: Option<Suite> = None;
    let mut current_case: Option<Case> = None;

    loop {
        match reader.read_event()? {
            Event::Start(event) if event.name().as_ref() == b"testsuite" => {
                current_suite = Some(suite_from_attrs(&reader, &event)?);
            }
            Event::Empty(event) if event.name().as_ref() == b"testsuite" => {
                suites.push(suite_from_attrs(&reader, &event)?);
            }
            Event::End(event) if event.name().as_ref() == b"testsuite" => {
                if let Some(suite) = current_suite.take() {
                    suites.push(suite);
                }
            }
            Event::Start(event) if event.name().as_ref() == b"testcase" => {
                current_case = Some(case_from_attrs(&reader, &event)?);
            }
            Event::Empty(event) if event.name().as_ref() == b"testcase" => {
                if let Some(suite) = current_suite.as_mut() {
                    suite.cases.push(case_from_attrs(&reader, &event)?);
                }
            }
            Event::Start(event) | Event::Empty(event)
                if matches!(event.name().as_ref(), b"failure" | b"error" | b"skipped") =>
            {
                if let Some(case) = current_case.as_mut() {
                    case.status = match event.name().as_ref() {
                        b"skipped" => "skipped",
                        _ => "failed",
                    }
                    .to_string();
                }
            }
            Event::End(event) if event.name().as_ref() == b"testcase" => {
                if let (Some(suite), Some(case)) = (current_suite.as_mut(), current_case.take()) {
                    suite.cases.push(case);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(suites)
}

fn suite_from_attrs(
    reader: &Reader<&[u8]>,
    event: &quick_xml::events::BytesStart<'_>,
) -> Result<Suite> {
    let attrs = xml_attrs(reader, event)?;
    Ok(Suite {
        name: attrs.get("name").cloned().unwrap_or_default(),
        tests: parse_u64(attrs.get("tests")),
        failures: parse_u64(attrs.get("failures")),
        errors: parse_u64(attrs.get("errors")),
        skipped: parse_u64(attrs.get("skipped")),
        time: attrs.get("time").cloned(),
        cases: Vec::new(),
    })
}

fn case_from_attrs(
    reader: &Reader<&[u8]>,
    event: &quick_xml::events::BytesStart<'_>,
) -> Result<Case> {
    let attrs = xml_attrs(reader, event)?;
    Ok(Case {
        name: attrs.get("name").cloned().unwrap_or_default(),
        classname: attrs.get("classname").cloned(),
        time: attrs.get("time").cloned(),
        status: "passed".to_string(),
    })
}

fn xml_attrs(
    reader: &Reader<&[u8]>,
    event: &quick_xml::events::BytesStart<'_>,
) -> Result<std::collections::BTreeMap<String, String>> {
    let mut attrs = std::collections::BTreeMap::new();
    for attr in event.attributes().with_checks(false) {
        let attr = attr?;
        let key = std::str::from_utf8(attr.key.as_ref())?.to_string();
        let value = attr
            .decode_and_unescape_value(reader.decoder())?
            .to_string();
        attrs.insert(key, value);
    }
    Ok(attrs)
}

fn parse_u64(value: Option<&String>) -> u64 {
    value.and_then(|value| value.parse().ok()).unwrap_or(0)
}
