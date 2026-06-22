use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::{Source, base_fact, json_strip_nulls, put_array, put_object, slug, string_field};

pub(crate) fn normalize(input: &str, source: &Source) -> Result<Vec<Value>> {
    let value: Value = serde_json::from_str(input).context("SBOM input was not valid JSON")?;
    if value.get("bomFormat").and_then(Value::as_str) == Some("CycloneDX") {
        return normalize_cyclonedx(&value, source);
    }
    if value.get("spdxVersion").is_some() {
        return normalize_spdx(&value, source);
    }
    bail!("unsupported SBOM JSON; expected CycloneDX or SPDX")
}

fn normalize_cyclonedx(value: &Value, source: &Source) -> Result<Vec<Value>> {
    let root = value
        .pointer("/metadata/component")
        .filter(|component| component.is_object());
    let root_name = root
        .and_then(|component| string_field(component, "name"))
        .unwrap_or_else(|| source.component.clone());
    let root_version = root
        .and_then(|component| string_field(component, "version"))
        .or_else(|| source.version.clone());
    let components = value
        .get("components")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let members = components.iter().map(component_member).collect::<Vec<_>>();
    let requirements = members
        .iter()
        .filter_map(|member| {
            let name = member.get("component").and_then(Value::as_str)?;
            Some(json!({
                "capability": format!("package:{name}"),
                "version": member.get("version").and_then(Value::as_str),
                "purl": member.get("purl").and_then(Value::as_str),
            }))
        })
        .collect::<Vec<_>>();

    let mut facts = Vec::new();
    let mut sbom = base_fact(
        source,
        format!(
            "sbom.{}{}",
            slug(&root_name),
            root_version
                .as_deref()
                .map(|value| format!(".{}", slug(value)))
                .unwrap_or_default()
        ),
        "sbom",
        root_name,
        root_version.clone(),
        "observed",
    );
    put_array(&mut sbom, "members", members);
    put_array(&mut sbom, "requires", requirements);
    put_array(
        &mut sbom,
        "provides",
        vec![json!({
            "capability": format!("sbom:{}", source.component),
            "version": root_version,
        })],
    );
    facts.push(sbom);

    for component in components {
        if let Some(fact) = package_fact(&component, source) {
            facts.push(fact);
        }
    }
    Ok(facts)
}

fn normalize_spdx(value: &Value, source: &Source) -> Result<Vec<Value>> {
    let packages = value
        .get("packages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if packages.is_empty() {
        bail!("SPDX SBOM did not include packages");
    }
    let root_name = value
        .get("name")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .unwrap_or_else(|| source.component.clone());
    let members = packages.iter().map(component_member).collect::<Vec<_>>();
    let mut facts = vec![base_fact(
        source,
        format!("sbom.{}", slug(&root_name)),
        "sbom",
        root_name,
        source.version.clone(),
        "observed",
    )];
    put_array(&mut facts[0], "members", members);
    for package in packages {
        if let Some(fact) = package_fact(&package, source) {
            facts.push(fact);
        }
    }
    Ok(facts)
}

fn component_member(component: &Value) -> Value {
    json_strip_nulls(json!({
        "component": string_field(component, "name")
            .or_else(|| string_field(component, "SPDXID"))
            .unwrap_or_else(|| "unknown".to_string()),
        "version": string_field(component, "version").or_else(|| string_field(component, "versionInfo")),
        "type": string_field(component, "type"),
        "purl": string_field(component, "purl"),
        "bomRef": string_field(component, "bom-ref").or_else(|| string_field(component, "SPDXID")),
        "digest": digest(component),
    }))
}

fn package_fact(component: &Value, source: &Source) -> Option<Value> {
    let name = string_field(component, "name").or_else(|| string_field(component, "SPDXID"))?;
    let version =
        string_field(component, "version").or_else(|| string_field(component, "versionInfo"));
    let mut fact = base_fact(
        source,
        format!(
            "package.{}{}",
            slug(&name),
            version
                .as_deref()
                .map(|value| format!(".{}", slug(value)))
                .unwrap_or_default()
        ),
        "dependency",
        name.clone(),
        version.clone(),
        "observed",
    );
    put_array(
        &mut fact,
        "provides",
        vec![json_strip_nulls(json!({
            "capability": format!("package:{name}"),
            "version": version,
            "purl": string_field(component, "purl"),
            "digest": digest(component),
        }))],
    );
    put_object(&mut fact, "package", component.clone());
    Some(fact)
}

fn digest(component: &Value) -> Option<String> {
    component
        .get("hashes")
        .and_then(Value::as_array)
        .and_then(|hashes| hashes.first())
        .and_then(|hash| {
            let algorithm =
                string_field(hash, "alg").or_else(|| string_field(hash, "algorithm"))?;
            let content =
                string_field(hash, "content").or_else(|| string_field(hash, "checksumValue"))?;
            Some(format!("{}:{}", algorithm.to_ascii_lowercase(), content))
        })
        .or_else(|| {
            component
                .get("checksums")
                .and_then(Value::as_array)
                .and_then(|checksums| checksums.first())
                .and_then(|checksum| {
                    let algorithm = string_field(checksum, "algorithm")?;
                    let content = string_field(checksum, "checksumValue")?;
                    Some(format!("{}:{}", algorithm.to_ascii_lowercase(), content))
                })
        })
}
