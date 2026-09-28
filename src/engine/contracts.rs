//! Output contracts shared by every stage: the JSON schemas the agents must return and
//! the role templates (plan §9). Embedded, so a task never reads them from a repository.

use anyhow::Result;

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
];

pub fn schema(name: &str) -> Result<&'static str> {
    SCHEMAS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, c)| *c)
        .ok_or_else(|| anyhow::anyhow!("no schema {name}"))
}

/// Role template by file name (`planner.md`, `implementer.md`, `reviewer.md`).
pub fn role(name: &str) -> &'static str {
    ROLES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, c)| *c)
        .unwrap_or("")
}
