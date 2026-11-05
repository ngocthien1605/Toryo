//! Output contracts shared by every stage: the JSON schemas the agents must return and
//! the role templates (plan §9). Embedded, so a task never reads them from a repository.
//!
//! Plan §9 (R22): every structured output is validated against its schema **in Rust**,
//! never trusting the CLI's own validation alone. [`validate`] implements the JSON Schema
//! subset the embedded schemas use ([`KEYWORDS`]) without a dependency; a schema that uses
//! anything else fails closed, and a unit test keeps the embedded schemas inside the subset.

use anyhow::Result;
use serde_json::{Map, Value};

pub const SCHEMAS: &[(&str, &str)] = &[
    (
        "plan.schema.json",
        include_str!("../../schemas/plan.schema.json"),
    ),
    (
        "review.schema.json",
        include_str!("../../schemas/review.schema.json"),
    ),
    (
        "implementation.schema.json",
        include_str!("../../schemas/implementation.schema.json"),
    ),
    (
        "probe.schema.json",
        include_str!("../../schemas/probe.schema.json"),
    ),
    (
        "implementer-probe.schema.json",
        include_str!("../../schemas/implementer-probe.schema.json"),
    ),
    // M16-03: the answer of `toryo goal split`
    (
        "goal-split.schema.json",
        include_str!("../../schemas/goal-split.schema.json"),
    ),
];

pub const ROLES: &[(&str, &str)] = &[
    (
        "planner.md",
        include_str!("../../templates/roles/planner.md"),
    ),
    (
        "implementer.md",
        include_str!("../../templates/roles/implementer.md"),
    ),
    (
        "reviewer.md",
        include_str!("../../templates/roles/reviewer.md"),
    ),
    // M16-03: the role text of the `toryo goal split` turn (after the three seats: the M0 kit
    // indexes them)
    (
        "goal-splitter.md",
        include_str!("../../templates/roles/goal-splitter.md"),
    ),
];

pub fn schema(name: &str) -> Result<&'static str> {
    SCHEMAS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, c)| *c)
        .ok_or_else(|| anyhow::anyhow!("no schema {name}"))
}

/// M17-A3 (AC-3): an implementation report as the engine checks it against
/// `implementation.schema.json`. `proof` is required by the schema (strict style, safe for Codex
/// structured output), but without a red-green gate in the verify commands a report that has no
/// `proof` is read as `"proof": []` — every fake script, recorded stream and agent from before M17
/// keeps passing unchanged. With a red-green gate it is checked as it is: a missing map is a schema
/// violation (recorded, EC-25) and the gate's map check fails it.
pub fn implementation_as_checked(v: &Value, red_green: bool) -> Value {
    match v {
        Value::Object(m) if !red_green && !m.contains_key("proof") => {
            let mut m = m.clone();
            m.insert("proof".into(), Value::Array(Vec::new()));
            Value::Object(m)
        }
        _ => v.clone(),
    }
}

/// Role template by file name (`planner.md`, `implementer.md`, `reviewer.md`).
pub fn role(name: &str) -> &'static str {
    ROLES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, c)| *c)
        .unwrap_or("")
}

/// The JSON Schema keywords [`validate`] implements (EC-25): exactly what the embedded
/// schemas use.
pub const KEYWORDS: &[&str] = &[
    "type",
    "enum",
    "required",
    "properties",
    "additionalProperties",
    "items",
    "minItems",
    "minLength",
    "pattern",
];

/// Annotation keywords: allowed in a schema, no effect on validation.
const ANNOTATIONS: &[&str] = &["$schema", "$id", "title", "description"];

/// The values of `type`.
const TYPES: &[&str] = &[
    "object", "array", "string", "integer", "number", "boolean", "null",
];

/// Validate `value` against the embedded schema `schema_name` (EC-25, plan §9). Every
/// problem is one line `<path>: <what>`, the path in JSONPath form (`$.issues[0].severity`),
/// ready for a correction turn (R22). A schema outside the supported subset is itself an
/// error: validation fails closed.
pub fn validate(schema_name: &str, value: &Value) -> Result<(), Vec<String>> {
    let s = parse_schema(schema_name)?;
    let unsupported = schema_problems(&s);
    if !unsupported.is_empty() {
        return Err(unsupported
            .into_iter()
            .map(|p| format!("{schema_name}: {p}"))
            .collect());
    }
    let mut errs = Vec::new();
    check(&s, value, "$", &mut errs);
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs)
    }
}

fn parse_schema(name: &str) -> Result<Value, Vec<String>> {
    let text = schema(name).map_err(|e| vec![format!("{e:#}")])?;
    serde_json::from_str(text).map_err(|e| vec![format!("{name}: not JSON: {e}")])
}

/// What in `schema` [`validate`] cannot enforce: unknown keywords and keyword values of the
/// wrong shape, as `<schema path>: <what>`. Empty for every embedded schema (a unit test).
pub fn schema_problems(schema: &Value) -> Vec<String> {
    let mut out = Vec::new();
    walk_schema(schema, "#", &mut out);
    out
}

fn walk_schema(s: &Value, at: &str, out: &mut Vec<String>) {
    let Some(obj) = s.as_object() else {
        out.push(format!("{at}: a schema must be an object"));
        return;
    };
    for (k, v) in obj {
        let here = format!("{at}/{k}");
        let ok = match k.as_str() {
            k if ANNOTATIONS.contains(&k) => true,
            "type" => match v {
                Value::String(t) => TYPES.contains(&t.as_str()),
                Value::Array(ts) => {
                    !ts.is_empty()
                        && ts
                            .iter()
                            .all(|t| t.as_str().is_some_and(|t| TYPES.contains(&t)))
                }
                _ => false,
            },
            "enum" => v.as_array().is_some_and(|a| !a.is_empty()),
            "required" => v.as_array().is_some_and(|a| a.iter().all(Value::is_string)),
            "properties" => match v.as_object() {
                Some(props) => {
                    for (name, p) in props {
                        walk_schema(p, &format!("{here}/{name}"), out);
                    }
                    true
                }
                None => false,
            },
            "additionalProperties" => match v {
                Value::Bool(_) => true,
                Value::Object(_) => {
                    walk_schema(v, &here, out);
                    true
                }
                _ => false,
            },
            "items" => {
                // one schema for every item; the tuple form (an array of schemas) is not supported
                let one = v.is_object();
                if one {
                    walk_schema(v, &here, out);
                }
                one
            }
            "minItems" | "minLength" => v.is_u64(),
            "pattern" => v.as_str().is_some_and(|p| regex::Regex::new(p).is_ok()),
            other => {
                out.push(format!("{here}: unsupported keyword `{other}`"));
                continue;
            }
        };
        if !ok {
            out.push(format!("{here}: unsupported value {}", brief(v)));
        }
    }
}

/// Does `v` have JSON Schema type `t`? An integer is a number without a fraction (`1.0` too).
fn is_type(v: &Value, t: &str) -> bool {
    match t {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        "number" => v.is_number(),
        "integer" => {
            v.is_i64()
                || v.is_u64()
                || v.as_f64()
                    .is_some_and(|f| f.is_finite() && f.fract() == 0.0)
        }
        _ => false,
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) if is_type(v, "integer") => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// A value in an error line: its JSON, cut after 60 characters.
fn brief(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 60 {
        format!("{}…", s.chars().take(60).collect::<String>())
    } else {
        s
    }
}

/// `$.name`, or `$["odd name"]` for a key that is not a plain identifier.
fn member(path: &str, key: &str) -> String {
    let plain = key
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if plain {
        format!("{path}.{key}")
    } else {
        format!("{path}[{}]", Value::String(key.to_string()))
    }
}

/// Validate `v` against the (well-formed, supported) schema `s`; problems go to `errs`.
fn check(s: &Value, v: &Value, path: &str, errs: &mut Vec<String>) {
    let empty = Map::new();
    let s = s.as_object().unwrap_or(&empty);
    if let Some(t) = s.get("type") {
        let types: Vec<&str> = match t {
            Value::Array(ts) => ts.iter().filter_map(Value::as_str).collect(),
            t => t.as_str().into_iter().collect(),
        };
        if !types.iter().any(|t| is_type(v, t)) {
            errs.push(format!(
                "{path}: expected {}, got {}",
                types.join(" or "),
                type_name(v)
            ));
            return; // the other keywords assume the type
        }
    }
    if let Some(Value::Array(options)) = s.get("enum") {
        if !options.contains(v) {
            let list: Vec<String> = options.iter().map(brief).collect();
            errs.push(format!(
                "{path}: {} is not one of {}",
                brief(v),
                list.join(", ")
            ));
        }
    }
    match v {
        Value::String(text) => {
            if let Some(min) = s.get("minLength").and_then(Value::as_u64) {
                if (text.chars().count() as u64) < min {
                    errs.push(format!("{path}: shorter than {min} character(s)"));
                }
            }
            if let Some(p) = s.get("pattern").and_then(Value::as_str) {
                // a pattern is not anchored: the schema anchors it (`^…$`)
                if !regex::Regex::new(p).is_ok_and(|re| re.is_match(text)) {
                    errs.push(format!("{path}: {} does not match {p}", brief(v)));
                }
            }
        }
        Value::Array(items) => {
            if let Some(min) = s.get("minItems").and_then(Value::as_u64) {
                if (items.len() as u64) < min {
                    errs.push(format!("{path}: fewer than {min} item(s)"));
                }
            }
            if let Some(item) = s.get("items") {
                for (i, x) in items.iter().enumerate() {
                    check(item, x, &format!("{path}[{i}]"), errs);
                }
            }
        }
        Value::Object(obj) => {
            for r in s
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                if !obj.contains_key(r) {
                    errs.push(format!("{path}: missing required property `{r}`"));
                }
            }
            let props = s.get("properties").and_then(Value::as_object);
            for (k, x) in obj {
                let at = member(path, k);
                match (props.and_then(|p| p.get(k)), s.get("additionalProperties")) {
                    (Some(p), _) => check(p, x, &at, errs),
                    (None, Some(Value::Bool(false))) => errs.push(format!(
                        "{at}: not allowed (the schema lists no such property)"
                    )),
                    (None, Some(extra @ Value::Object(_))) => check(extra, x, &at, errs),
                    (None, _) => {}
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// EC-25: a keyword the validator does not implement would be silently ignored — so an
    /// embedded schema may use only [`KEYWORDS`] (and annotations), with well-formed values.
    #[test]
    fn every_embedded_schema_uses_only_supported_keywords() {
        // M16-03: goal-split.schema.json is the sixth
        assert_eq!(SCHEMAS.len(), 6);
        for (name, text) in SCHEMAS {
            let s: Value = serde_json::from_str(text).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(schema_problems(&s), Vec::<String>::new(), "{name}");
        }
        // the check itself sees what it must refuse
        let odd = json!({"type": "object", "oneOf": [], "properties": {
            "a": {"type": "strng", "format": "date"},
            "b": {"items": [{"type": "string"}], "minItems": -1, "pattern": "("},
            "c": {"additionalProperties": {"const": 1}, "required": "c"}}});
        assert_eq!(
            schema_problems(&odd),
            [
                "#/oneOf: unsupported keyword `oneOf`",
                "#/properties/a/format: unsupported keyword `format`",
                "#/properties/a/type: unsupported value \"strng\"",
                "#/properties/b/items: unsupported value [{\"type\":\"string\"}]",
                "#/properties/b/minItems: unsupported value -1",
                "#/properties/b/pattern: unsupported value \"(\"",
                "#/properties/c/additionalProperties/const: unsupported keyword `const`",
                "#/properties/c/required: unsupported value \"c\"",
            ]
        );
    }

    #[test]
    fn the_fixture_outputs_are_valid() {
        let plan = json!({"understanding": "u", "files_likely_involved": ["src/Calculator.psm1"], "steps": ["add"], "validation": ["tests"], "plan_markdown": "md"});
        assert_eq!(validate("plan.schema.json", &plan), Ok(()));
        let review = json!({"decision": "changes_requested", "summary": "s",
            "acceptance_criteria": [{"id": "AC-1", "met": true, "evidence": "e1"}, {"id": "AC-2", "met": false, "evidence": "no zero test"}],
            "issues": [{"id": "I-1", "severity": "high", "blocking": true, "file": "src/a.ps1", "line": 3, "description": "add the zero test"}],
            "resolved_from_previous": ["I-7"], "extra": "allowed: no additionalProperties"});
        assert_eq!(validate("review.schema.json", &review), Ok(()));
        // M17-A3: `proof` is required by the schema (was: the report without it was valid as it
        // is); the engine reads a report without it as `proof: []` when no red-green gate runs
        let imp = json!({"summary": "s", "deviations": [], "validation_performed": ["read"], "open_issues": [], "resolved": []});
        assert_eq!(
            validate("implementation.schema.json", &imp),
            Err(vec!["$: missing required property `proof`".to_string()])
        );
        assert_eq!(
            validate(
                "implementation.schema.json",
                &implementation_as_checked(&imp, false)
            ),
            Ok(())
        );
        let with_proof = json!({"summary": "s", "deviations": [], "validation_performed": ["read"], "open_issues": [], "resolved": [], "proof": [{"ac": "AC-1", "tests": ["calc::adds_two"]}]});
        assert_eq!(validate("implementation.schema.json", &with_proof), Ok(()));
    }

    /// M17-A3 (AC-3): without a red-green gate a report without `proof` is read as `proof: []`;
    /// with one it is checked as returned; a report that has `proof` is never changed.
    #[test]
    fn m17_ac03_a_report_without_proof_passes_only_without_a_red_green_gate() {
        let imp = json!({"summary": "s", "deviations": [], "validation_performed": [], "open_issues": [], "resolved": []});
        assert_eq!(implementation_as_checked(&imp, false)["proof"], json!([]));
        assert!(implementation_as_checked(&imp, true).get("proof").is_none());
        assert!(validate(
            "implementation.schema.json",
            &implementation_as_checked(&imp, true)
        )
        .is_err());
        let p = json!({"summary": "s", "proof": [{"ac": "AC-1", "tests": ["t"]}]});
        assert_eq!(implementation_as_checked(&p, false), p);
        assert_eq!(implementation_as_checked(&json!("x"), false), json!("x"));
        // the map's entries are strict too
        let bad = json!({"summary": "s", "deviations": [], "validation_performed": [], "open_issues": [], "resolved": [], "proof": [{"ac": "AC-1", "tests": ["t"], "why": "x"}, {"ac": "AC-2"}]});
        assert_eq!(
            validate("implementation.schema.json", &bad),
            Err(vec![
                "$.proof[0].why: not allowed (the schema lists no such property)".to_string(),
                "$.proof[1]: missing required property `tests`".into(),
            ])
        );
    }

    #[test]
    fn plan_problems_name_the_path() {
        // the probe's plan (EC-25): not a plan at all
        assert_eq!(
            validate("plan.schema.json", &json!({"note": "not a plan at all"})),
            Err(vec![
                "$: missing required property `understanding`".to_string(),
                "$: missing required property `files_likely_involved`".into(),
                "$: missing required property `steps`".into(),
                "$: missing required property `validation`".into(),
                "$: missing required property `plan_markdown`".into(),
            ])
        );
        assert_eq!(
            validate(
                "plan.schema.json",
                &json!({"understanding": "u", "files_likely_involved": "src/a.ps1", "steps": ["a", 2], "validation": [], "plan_markdown": null, "risks": [null]})
            ),
            Err(vec![
                "$.files_likely_involved: expected array, got string".to_string(),
                "$.plan_markdown: expected string, got null".into(),
                "$.risks[0]: expected string, got null".into(),
                "$.steps[1]: expected string, got integer".into(),
            ])
        );
        for v in [json!(null), json!([]), json!("plan"), json!(1.5)] {
            let e = validate("plan.schema.json", &v).unwrap_err();
            assert_eq!(e.len(), 1, "{v}: {e:?}");
            assert!(e[0].starts_with("$: expected object, got "), "{e:?}");
        }
    }

    #[test]
    fn review_problems_cover_enum_pattern_min_and_types() {
        // the old test fixtures (TS-03): issue id `R-1`, severity `major`
        let review = json!({"decision": "maybe", "summary": "s",
            "acceptance_criteria": [{"id": "AC-01", "met": "yes", "evidence": ""}, {"id": "AC-2", "met": true}],
            "issues": [{"id": "R-1", "severity": "major", "blocking": false, "description": "d", "line": 3.5}, {"id": "I-2", "severity": "low", "blocking": true, "description": "d", "line": 7.0}]});
        assert_eq!(
            validate("review.schema.json", &review),
            Err(vec![
                "$.acceptance_criteria[0].evidence: shorter than 1 character(s)".to_string(),
                "$.acceptance_criteria[0].id: \"AC-01\" does not match ^AC-[1-9][0-9]*$".into(),
                "$.acceptance_criteria[0].met: expected boolean, got string".into(),
                "$.acceptance_criteria[1]: missing required property `evidence`".into(),
                "$.decision: \"maybe\" is not one of \"approved\", \"changes_requested\", \"blocked\"".into(),
                "$.issues[0].id: \"R-1\" does not match ^I-[1-9][0-9]*$".into(),
                "$.issues[0].line: expected integer, got number".into(),
                "$.issues[0].severity: \"major\" is not one of \"high\", \"medium\", \"low\"".into(),
            ])
        );
        let empty = json!({"decision": "approved", "summary": "s", "acceptance_criteria": [], "issues": []});
        assert_eq!(
            validate("review.schema.json", &empty),
            Err(vec![
                "$.acceptance_criteria: fewer than 1 item(s)".to_string()
            ])
        );
        // a pattern is searched, not anchored by the validator: the schema's `^…$` anchors it
        let e = validate("review.schema.json", &json!({"decision": "approved", "summary": "s", "issues": [],
            "acceptance_criteria": [{"id": "xAC-1", "met": true, "evidence": "e"}, {"id": "AC-1 ", "met": true, "evidence": "é"}]}))
        .unwrap_err();
        assert_eq!(e.len(), 2, "{e:?}");
    }

    #[test]
    fn strict_schemas_refuse_additional_properties_and_long_values_are_cut() {
        let long = "x".repeat(100);
        // (M17-A3: `proof` added — the schema requires it now; the assertions are unchanged)
        let imp = json!({"summary": "s", "deviations": [], "validation_performed": [], "open_issues": [], "resolved": [], "proof": [], "bonus": long, "odd key": 1});
        assert_eq!(
            validate("implementation.schema.json", &imp),
            Err(vec![
                "$.bonus: not allowed (the schema lists no such property)".to_string(),
                "$[\"odd key\"]: not allowed (the schema lists no such property)".into(),
            ])
        );
        let imp = json!({"summary": long, "deviations": [1], "validation_performed": [], "open_issues": [], "resolved": [], "proof": []});
        assert_eq!(
            validate("implementation.schema.json", &imp),
            Err(vec![
                "$.deviations[0]: expected string, got integer".to_string()
            ])
        );
        let e = validate("review.schema.json", &json!({"decision": long})).unwrap_err();
        assert!(
            e.contains(&format!(
                "$.decision: \"{}… is not one of \"approved\", \"changes_requested\", \"blocked\"",
                "x".repeat(59)
            )),
            "{e:?}"
        );
    }

    #[test]
    fn an_unknown_schema_fails_closed() {
        assert_eq!(
            validate("nope.schema.json", &json!({})),
            Err(vec!["no schema nope.schema.json".to_string()])
        );
    }

    #[test]
    fn integers_and_type_lists() {
        for (v, integer) in [
            (json!(3), true),
            (json!(-3), true),
            (json!(3.0), true),
            (json!(u64::MAX), true),
            (json!(3.25), false),
            (json!("3"), false),
            (json!(null), false),
        ] {
            assert_eq!(is_type(&v, "integer"), integer, "{v}");
        }
        let mut errs = Vec::new();
        let nullable = json!({"type": ["string", "null"]});
        check(&nullable, &json!(null), "$", &mut errs);
        check(&nullable, &json!("x"), "$", &mut errs);
        assert!(errs.is_empty(), "{errs:?}");
        check(&nullable, &json!(1), "$.x", &mut errs);
        assert_eq!(errs, ["$.x: expected string or null, got integer"]);
    }
}
