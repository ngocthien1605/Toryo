//! Render structured outputs to the Markdown artifacts (plan §9).

use anyhow::{bail, Result};
use serde_json::Value;

fn list(v: &Value, key: &str) -> String {
    let items: Vec<String> = v[key]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .map(|s| format!("- {s}"))
                .collect()
        })
        .unwrap_or_default();
    if items.is_empty() {
        "- (none)".into()
    } else {
        items.join("\n")
    }
}

pub fn render(kind: &str, v: &Value) -> Result<String> {
    Ok(match kind {
        "plan" => format!(
            "# PLAN\n\n## Understanding\n{}\n\n## Files likely involved\n{}\n\n## Steps\n{}\n\n## Validation\n{}\n\n## Risks\n{}\n\n## Assumptions\n{}\n\n## Open questions\n{}\n\n---\n\n{}\n",
            v["understanding"].as_str().unwrap_or(""),
            list(v, "files_likely_involved"),
            v["steps"].as_array().map(|a| a.iter().enumerate().map(|(i, s)| format!("{}. {}", i + 1, s.as_str().unwrap_or(""))).collect::<Vec<_>>().join("\n")).unwrap_or_default(),
            list(v, "validation"),
            list(v, "risks"),
            list(v, "assumptions"),
            list(v, "open_questions"),
            v["plan_markdown"].as_str().unwrap_or("")
        ),
        "review" => {
            let mut s = format!("# REVIEW — {}\n\n{}\n\n## Acceptance criteria\n\n| AC | Met | Evidence |\n|---|---|---|\n",
                v["decision"].as_str().unwrap_or("?"), v["summary"].as_str().unwrap_or(""));
            for ac in v["acceptance_criteria"].as_array().into_iter().flatten() {
                s.push_str(&format!("| {} | {} | {} |\n", ac["id"].as_str().unwrap_or(""), if ac["met"].as_bool() == Some(true) { "yes" } else { "**no**" },
                    ac["evidence"].as_str().unwrap_or("").replace('|', "\\|").replace('\n', " ")));
            }
            s.push_str("\n## Issues\n\n");
            let issues = v["issues"].as_array().cloned().unwrap_or_default();
            if issues.is_empty() {
                s.push_str("- (none)\n");
            }
            for i in issues {
                s.push_str(&format!("- **{}** [{}{}] {}{}\n", i["id"].as_str().unwrap_or(""), i["severity"].as_str().unwrap_or(""),
                    if i["blocking"].as_bool() == Some(true) { ", blocking" } else { "" },
                    i["file"].as_str().map(|f| format!("`{f}`: ")).unwrap_or_default(), i["description"].as_str().unwrap_or("")));
            }
            s
        }
        "implementation" => format!(
            "# IMPLEMENTATION\n\n{}\n\n## Deviations from plan\n{}\n\n## Validation performed\n{}\n\n## Open issues\n{}\n\n## Resolved\n{}\n",
            v["summary"].as_str().unwrap_or(""),
            list(v, "deviations"),
            list(v, "validation_performed"),
            list(v, "open_issues"),
            list(v, "resolved")
        ),
        other => bail!("unknown kind `{other}` (plan|review|implementation)"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_review_table() {
        let md = render("review", &json!({"decision":"changes_requested","summary":"s","acceptance_criteria":[{"id":"AC-1","met":false,"evidence":"a|b"}],"issues":[{"id":"I-1","severity":"high","blocking":true,"description":"d"}]})).unwrap();
        assert!(md.contains("| AC-1 | **no** | a\\|b |"));
        assert!(md.contains("**I-1** [high, blocking]"));
    }
}
