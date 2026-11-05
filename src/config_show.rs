//! `toryo config show [--json]` (M11-B2): the effective configuration with the origin of each
//! seat, profile, gate, toolchain, `skills_dir` and verify setting, the files read with their
//! SHA-256, and the mode (`toryo.yaml only` / `org chart`). The `--json` document is
//! described in the README (schema 1).

use serde_json::{json, Value};

use crate::config::{Effective, Origin};
use crate::team::GateType;

/// The two modes of a configuration.
pub fn mode(cfg: &Effective) -> &'static str {
    if cfg.team.is_some() {
        "org chart"
    } else {
        "toryo.yaml only"
    }
}

fn source(o: Option<Origin>) -> Value {
    match o {
        Some(o) => json!(o),
        None => Value::Null,
    }
}

/// The `--json` document (schema 1).
pub fn json(cfg: &Effective) -> Value {
    let team = cfg.team.as_ref();
    let seats: Vec<Value> = cfg
        .repo
        .roles
        .iter()
        .map(|(seat, r)| {
            let mut v = json!({
                "seat": seat,
                "profile": r.profile,
                "model": r.model,
                "fallback": r.fallback,
                "source": source(cfg.origins.roles.get(seat).copied()),
            });
            // M17-B1 (AC-12): only when set — every other seat keeps its shape
            if let Some(c) = &r.candidates {
                v["candidates"] = json!(c);
            }
            v
        })
        .collect();
    let profiles: Vec<Value> = cfg
        .repo
        .profiles
        .iter()
        .map(|(name, p)| {
            json!({
                "name": name,
                "kind": p.kind,
                "billing": p.billing,
                "exe": p.exe,
                "source": source(cfg.origins.profiles.get(name).copied()),
            })
        })
        .collect();
    let gates: Vec<Value> = match team {
        Some(t) => t
            .gates
            .iter()
            .map(|(name, g)| {
                let skipped = g.decl.kind == GateType::Environment && g.machine.is_none();
                let mut v = json!({
                    "name": name,
                    "kind": g.decl.kind,
                    "timeout": g.decl.timeout,
                    "lock": g.machine.as_ref().map(|m| m.lock.clone()),
                    // M11-R3: what runs — program and args as expanded at load
                    "command": g.machine.as_ref().map(|m| {
                        std::iter::once(&m.program).chain(&m.args).collect::<Vec<_>>()
                    }),
                    "if_missing": g.decl.if_missing,
                    // M12-07: does it ask before it deploys (`never`, `first`, `always`)
                    "confirm": g.machine.as_ref().map(|m| m.confirm.as_str()),
                    "defined_on_this_machine": g.decl.kind == GateType::Check || !skipped,
                    "source": source(cfg.origins.gates.get(name).copied()),
                });
                // M17-A1 (AC-1): only when set — every other gate keeps its shape
                if let Some(rg) = &g.decl.red_green {
                    v["red_green"] = red_green_json(rg, g.decl.program.as_deref());
                }
                v
            })
            .collect(),
        None => cfg
            .repo
            .commands
            .verify
            .iter()
            .map(|c| {
                let mut v = json!({
                    "name": c.name,
                    "kind": "check",
                    "timeout": c.timeout_min.map(|m| format!("{m}m")),
                    "lock": null,
                    "command": null,
                    "if_missing": null,
                    "confirm": null,
                    "defined_on_this_machine": true,
                    "source": source((!cfg.origins.roles.is_empty()).then_some(Origin::Repo)),
                });
                if let Some(rg) = &c.red_green {
                    v["red_green"] = red_green_json(rg, c.program.as_deref());
                }
                v
            })
            .collect(),
    };
    let toolchains: Vec<Value> = cfg
        .toolchains
        .names()
        .into_iter()
        .map(|n| json!({"name": n, "source": source(cfg.origins.toolchains.get(n).copied())}))
        .collect();
    let verify: serde_json::Map<String, Value> = [
        ("boundary", Some(cfg.repo.verify.boundary.clone())),
        ("min_boundary", cfg.machine_verify.min_boundary.clone()),
        (
            "verify_root",
            Some(crate::engine::verify::verify_root_setting(cfg).to_string()),
        ),
    ]
    .into_iter()
    .map(|(k, v)| {
        (
            k.to_string(),
            json!({"value": v, "source": source(cfg.origins.verify.get(k).copied())}),
        )
    })
    .collect();
    json!({
        "schema": 1,
        "mode": mode(cfg),
        "config_sha256": cfg.sha256(),
        "team": team.map(|t| t.name.clone()),
        "files": cfg.files,
        "seats": seats,
        "profiles": profiles,
        "gates": gates,
        "before_handoff": team
            .and_then(|t| t.role("implementer"))
            .map(|r| r.before_handoff.clone()),
        "toolchains": toolchains,
        "skills_dir": team.and_then(|t| t.skill_roots.machine.clone()).map(|p| {
            json!({"path": p, "source": source(cfg.origins.skills_dir)})
        }),
        "verify": verify,
        // M12-08 (X20): keys the file sets that no engine code reads
        "no_effect": cfg.keys_without_effect(),
        "limits": team.map(|t| json!(t.limits)),
        "checks": team.map(|t| json!(t.checks)),
    })
}

/// M17-A1: a gate's `red_green` block in `config show --json`: the block as written, the cap in
/// force and the line `gate list` prints.
fn red_green_json(rg: &crate::config::RedGreen, program: Option<&str>) -> Value {
    json!({
        "tests": rg.tests,
        "args": rg.args,
        "green_output_must_match": rg.green_output_must_match,
        "max_tests": rg.max_tests(),
        "words": rg.words(program),
    })
}

/// An argument as `config show` prints it: quoted when it is empty or has a space.
fn shown_arg(a: &str) -> String {
    if a.is_empty() || a.contains(char::is_whitespace) {
        format!("'{a}'")
    } else {
        a.to_string()
    }
}

fn place(o: &Value) -> String {
    match o.as_str() {
        Some("repo") => "repo".into(),
        Some("machine") => "machine".into(),
        Some("repo_overridden_by_machine") => "repo, overridden by machine".into(),
        _ => "frozen".into(),
    }
}

/// The text form: one line per entry, `— <source>` at the end.
pub fn text(cfg: &Effective) -> String {
    let v = json(cfg);
    let mut s = format!(
        "configuration: {}{} · sha256 {}\n",
        v["mode"].as_str().unwrap_or_default(),
        v["team"]
            .as_str()
            .map(|t| format!(" (team {t})"))
            .unwrap_or_default(),
        crate::git::short(v["config_sha256"].as_str().unwrap_or_default())
    );
    // (M11-R21) the digest of a task also covers the build-output entry scripts of its base
    // commit, which only `task create` / `task config --reload` can know
    s.push_str(
        "  (a task's digest also covers its build-output entry scripts — gate scripts that a gate builds, missing at the task's base commit and ignored — and then differs from this one)\n",
    );
    s.push_str("\nfiles read:\n");
    for f in &cfg.files {
        s.push_str(&format!(
            "  {:<16} {}  sha256 {}\n",
            f.what,
            f.path,
            crate::git::short(&f.sha256)
        ));
    }
    s.push_str("\nseats:\n");
    for x in v["seats"].as_array().into_iter().flatten() {
        let fb: Vec<&str> = x["fallback"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        s.push_str(&format!(
            "  {:<12} {}{}{} — {}\n",
            x["seat"].as_str().unwrap_or_default(),
            x["profile"].as_str().unwrap_or_default(),
            x["model"]
                .as_str()
                .map(|m| format!(" (model {m})"))
                .unwrap_or_default(),
            if fb.is_empty() {
                String::new()
            } else {
                format!(" → fallback {}", fb.join(" → "))
            },
            place(&x["source"])
        ));
        // M17-B1: the best-of-N candidates of the first Implementing stage
        if let Some(c) = x["candidates"].as_array() {
            let c: Vec<String> = c
                .iter()
                .enumerate()
                .map(|(i, p)| format!("c{} {}", i + 1, p.as_str().unwrap_or_default()))
                .collect();
            s.push_str(&format!(
                "               candidates (best-of-N, first Implementing stage): {}\n",
                c.join(", ")
            ));
        }
    }
    s.push_str("\nprofiles:\n");
    for x in v["profiles"].as_array().into_iter().flatten() {
        s.push_str(&format!(
            "  {:<16} {}, {} — {}\n",
            x["name"].as_str().unwrap_or_default(),
            x["kind"].as_str().unwrap_or_default(),
            x["billing"].as_str().unwrap_or_default(),
            place(&x["source"])
        ));
    }
    s.push_str(if cfg.team.is_some() {
        "\ngates:\n"
    } else {
        "\nverify commands:\n"
    });
    for x in v["gates"].as_array().into_iter().flatten() {
        let name = x["name"].as_str().unwrap_or_default();
        let env = x["kind"] == "environment";
        let detail = match (env, x["lock"].as_str()) {
            (true, Some(lock)) => {
                format!(" — command: machine config `environment_gates.{name}`, lock {lock}")
            }
            (true, None) => format!(
                " — NOT DEFINED on this machine (if_missing: {})",
                x["if_missing"].as_str().unwrap_or("error")
            ),
            (false, _) => format!(" — {}", place(&x["source"])),
        };
        s.push_str(&format!(
            "  {:<12} {:<12} {:>5}{detail}\n",
            name,
            x["kind"].as_str().unwrap_or_default(),
            x["timeout"].as_str().unwrap_or("-"),
        ));
        if let Some(argv) = x["command"].as_array() {
            let argv: Vec<String> = argv
                .iter()
                .filter_map(Value::as_str)
                .map(shown_arg)
                .collect();
            s.push_str(&format!("      runs: {}\n", argv.join(" ")));
        }
        if let Some(w) = x["red_green"]["words"].as_str() {
            s.push_str(&format!("      {w}\n"));
        }
        if let Some(c) = x["confirm"].as_str() {
            s.push_str(&format!(
                "      confirm: {c}{}\n",
                match c {
                    "never" => " (deploys without asking)",
                    "first" => " (asks once per task before it deploys)",
                    _ => " (asks for every new checkpoint before it deploys)",
                }
            ));
        }
    }
    if let Some(order) = v["before_handoff"].as_array() {
        let order: Vec<&str> = order.iter().filter_map(Value::as_str).collect();
        s.push_str(&format!("  before_handoff: {}\n", order.join(" → ")));
    }
    if let Some(t) = v["toolchains"].as_array().filter(|t| !t.is_empty()) {
        s.push_str("\ntoolchains:\n");
        for x in t {
            s.push_str(&format!(
                "  {} — {}\n",
                x["name"].as_str().unwrap_or_default(),
                place(&x["source"])
            ));
        }
    }
    if cfg.team.is_some() {
        s.push_str(&format!(
            "\nskills_dir: {}\n",
            match v["skills_dir"]["path"].as_str() {
                Some(p) => format!("{p} — {}", place(&v["skills_dir"]["source"])),
                None => "(none: repository skills only)".into(),
            }
        ));
    }
    s.push_str("\nverify:\n");
    for k in ["boundary", "min_boundary", "verify_root"] {
        let x = &v["verify"][k];
        match x["value"].as_str() {
            Some(val) => s.push_str(&format!("  {k:<13} {val} — {}\n", place(&x["source"]))),
            None => s.push_str(&format!("  {k:<13} low_integrity (default)\n")),
        }
    }
    if let Some(l) = v["limits"].as_object() {
        let l: Vec<String> = l.iter().map(|(k, v)| format!("{k} {v}")).collect();
        s.push_str(&format!(
            "\nlimits: {}\n",
            if l.is_empty() {
                "(defaults)".into()
            } else {
                l.join(", ")
            }
        ));
    }
    if let Some(c) = v["checks"].as_object() {
        let c: Vec<String> = c
            .iter()
            .map(|(k, v)| format!("{k} {}", v.as_str().unwrap_or_default()))
            .collect();
        s.push_str(&format!("checks: {}\n", c.join(", ")));
    }
    for key in cfg.keys_without_effect() {
        s.push_str(&format!("\nnote: {}\n", Effective::no_effect_line(key)));
    }
    s
}
