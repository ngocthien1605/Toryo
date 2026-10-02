//! M11-E3 end to end, on FakeAgent: environment gates — order after the check gates, the
//! owner's normal token, the machine-wide lock, the deployment record, secrets, `if_missing`,
//! a mutated checkout, a crash during the gate. The gate scripts are written to `<root>/gates/`
//! (outside the repository, the worktrees and the verify root): `pwsh` on Windows, `sh`
//! elsewhere. Nothing touches a real D365FO environment.
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use toryo::engine::store;
use toryo::engine::PipelineState as St;

fn fwd(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

/// The machine name the records use.
fn machine_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "this machine".into())
}

/// `environment_gates.<name>` running a script written to `<root>/gates/<name>.ps1|.sh`.
fn machine_gate(env: &Env, name: &str, lock: &str, ps1: &str, sh: &str, extra: &str) -> String {
    let dir = env.root.join("gates");
    std::fs::create_dir_all(&dir).unwrap();
    let (program, args) = if cfg!(windows) {
        let f = dir.join(format!("{name}.ps1"));
        std::fs::write(&f, ps1).unwrap();
        (
            "pwsh".to_string(),
            format!("[-NoProfile, -NonInteractive, -File, '{}']", fwd(&f)),
        )
    } else {
        let f = dir.join(format!("{name}.sh"));
        std::fs::write(&f, sh).unwrap();
        ("sh".to_string(), format!("['{}']", fwd(&f)))
    };
    format!("  {name}:\n    program: {program}\n    args: {args}\n    lock: {lock}\n{extra}")
}

/// The fixture in org chart mode with the check gate `divide-present` and the environment
/// gate `dbsync` (`if_missing`) after it; `machine_gates`: the machine's `environment_gates:`
/// entries (none: the machine does not define dbsync).
fn env_gate_setup(implementer: &str, if_missing: &str, org_tail: &str) -> Env {
    let org = ORGCHART.replace("IMPLEMENTER", implementer).replacen(
        "gates:\n",
        &format!(
            "gates:\n  dbsync: {{ kind: environment, timeout: 2m, if_missing: {if_missing} }}\n"
        ),
        1,
    ) + org_tail;
    setup_orgchart(
        implementer,
        "",
        &[
            (".toryo/toryo-orgchart.yaml", &org),
            (
                ".toryo/roles/implementer/toryo-role.yaml",
                "toryo: 1\npurpose: Change the code.\nbefore_handoff: [divide-present, dbsync]\n",
            ),
        ],
    )
}

/// Append `environment_gates:` with `entries` to the fixture's machine config.
fn define_gates(env: &Env, entries: &str) {
    let m = std::fs::read_to_string(&env.machine).unwrap();
    std::fs::write(&env.machine, format!("{m}environment_gates:\n{entries}")).unwrap();
}

fn verify_result(env: &Env, id: &str) -> Value {
    let p = task(env, id).artifacts().join("VERIFY_RESULT.json");
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

fn deployments(env: &Env) -> Vec<store::Deployment> {
    db(env).call(store::deployments).unwrap()
}

fn events(env: &Env, id: &str) -> Vec<Value> {
    std::fs::read_to_string(task(env, id).run_store.join("events.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// A gate script that logs each run (`<root>/gates/<log>`: one line with the SHA) and fails
/// the first `fail_first` runs with an error line.
fn counting_gate(env: &Env, log: &str, fail_first: usize) -> (String, String) {
    let log = fwd(&env.root.join("gates").join(log));
    (
        format!(
            "Add-Content -Path '{log}' -Value $env:TORYO_SHA\n$n = (Get-Content '{log}').Count\nif ($n -le {fail_first}) {{ 'SYNC ERROR: table CustTable could not be synchronized'; exit 1 }}\n'sync ok'\nexit 0\n"
        ),
        format!(
            "echo \"$TORYO_SHA\" >> '{log}'\nn=$(wc -l < '{log}')\nif [ \"$n\" -le {fail_first} ]; then echo 'SYNC ERROR: table CustTable could not be synchronized'; exit 1; fi\necho 'sync ok'\nexit 0\n"
        ),
    )
}

/// M11-E3 AC-1, AC-4, AC-6: the environment gate runs only after the check gate passed, in
/// order, and its exit code decides; a failure goes back to the (Claude) implementer as an
/// `<untrusted_data>` block and counts against `max_gate_retries`; the deployment record is
/// shown in `status`, `task show`, both `--json` documents and the final gate notes, and
/// survives `task cleanup`.
#[test]
fn an_environment_gate_runs_after_the_checks_and_its_exit_code_decides() {
    let env = env_gate_setup(
        "claude-max",
        "error",
        "limits:\n  max_gate_retries: 3\n  max_review_rounds: 3\n",
    );
    let (ps1, sh) = counting_gate(&env, "sync.log", 1);
    define_gates(
        &env,
        &machine_gate(&env, "dbsync", "d365-env", &ps1, &sh, ""),
    );
    create(&env, "EG-1");
    let s = script(
        &env,
        "eg1.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [
                turn(vec![implementation()]),
                turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()]),
                turn(vec![implementation()]),
            ],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "EG-1", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let (_, err) = toryo(
        &env,
        &[
            "approve",
            "EG-1",
            "--gate",
            "plan",
            "--continue",
            "--fake-agents",
            &s,
        ],
        &[],
    );
    let t = task(&env, "EG-1");
    assert_eq!(t.state, St::WaitingFinalApproval, "{err}");
    // the first checkpoint failed the check gate: no sync; then a failed sync, then a pass
    let log = std::fs::read_to_string(env.root.join("gates").join("sync.log")).unwrap();
    let runs: Vec<&str> = log.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(runs.len(), 2, "{log}");
    let head = t.machine.checkpoint().unwrap().to_string();
    assert_eq!(runs[1].trim(), head);
    assert_eq!(t.machine.split_limits().unwrap().gate_retries, 2);
    // the failed sync went to the Claude implementer as untrusted data
    let fix = calls(&env, "EG-1")
        .into_iter()
        .rev()
        .find(|c| {
            c["key"]
                .as_str()
                .unwrap_or_default()
                .starts_with("implementer")
        })
        .unwrap();
    let prompt = fix["prompt"].as_str().unwrap();
    let block = prompt.find("<untrusted_data").expect("a data block");
    assert!(
        prompt[block..].contains("SYNC ERROR: table CustTable"),
        "{prompt}"
    );
    // VERIFY_RESULT.json: the check gate, then the environment gate entry
    let vr = verify_result(&env, "EG-1");
    let cmds = vr["commands"].as_array().unwrap();
    assert_eq!(cmds[0]["name"], "divide-present");
    let e = &cmds[1];
    assert_eq!(
        (
            e["name"].clone(),
            e["kind"].clone(),
            e["lock"].clone(),
            e["exit"].clone()
        ),
        (
            json!("dbsync"),
            json!("environment"),
            json!("d365-env"),
            json!(0)
        )
    );
    assert_eq!(e["machine"], machine_name());
    assert!(e["argv"][0].as_str().unwrap().ends_with("sh"), "{e}");
    // the record, before approval
    let d = deployments(&env);
    assert_eq!(d.len(), 1);
    assert_eq!(
        (d[0].task.as_str(), d[0].sha.as_str(), d[0].ok),
        ("EG-1", head.as_str(), Some(true))
    );
    let line = format!("d365-env holds EG-1 @ {}", &head[..12]);
    assert!(
        err.contains(&format!(
            "final gate — verify: dbsync on {} (environment gate)",
            machine_name()
        )),
        "{err}"
    );
    assert!(
        err.contains(&format!("final gate — environment: {line}")),
        "{err}"
    );
    assert!(status_out(&env, "EG-1").contains(&format!("{line} (not approved yet)")));
    let ev = events(&env, "EG-1");
    assert!(ev
        .iter()
        .any(|e| e["type"] == "verify_command" && e["kind"] == "environment"));
    assert!(ev
        .iter()
        .any(|e| e["type"] == "environment_deployed" && e["ok"] == true));
    assert!(
        ev.iter()
            .filter(|e| e["type"] == "verify_command" && e["name"] == "divide-present")
            .all(|e| e.get("kind").is_none()),
        "the M10 shape for check gates"
    );
    // approved: every view says so
    let (code, err) = toryo(&env, &["approve", "EG-1", "--gate", "final"], &[]);
    assert_eq!(code, 0, "{err}");
    let all = String::from_utf8(command(&env, &["status"], &[]).output().unwrap().stdout).unwrap();
    assert!(
        all.contains(&format!("environment    {line} (approved)")),
        "{all}"
    );
    let show = String::from_utf8(
        command(&env, &["task", "show", "EG-1"], &[])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(show.contains(&format!("{line} (approved)")), "{show}");
    for args in [
        vec!["status", "--json"],
        vec!["task", "show", "EG-1", "--json"],
    ] {
        let o = command(&env, &args, &[]).output().unwrap();
        let v: Value = serde_json::from_slice(&o.stdout).unwrap();
        let envs = v["environments"]
            .as_array()
            .unwrap_or_else(|| panic!("{args:?}: {v}"));
        assert_eq!(envs[0]["lock"], "d365-env");
        assert_eq!(envs[0]["sha"], head);
        assert_eq!(envs[0]["approved"], true);
        assert_eq!(envs[0]["ok"], true);
    }
    // `task cleanup` does not clear it: the environment still runs that code
    let (code, err) = toryo(&env, &["task", "cleanup", "EG-1"], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(deployments(&env).len(), 1);
    let all = String::from_utf8(command(&env, &["status"], &[]).output().unwrap().stdout).unwrap();
    assert!(all.contains(&line), "{all}");
}

/// M11-E3 AC-6: cancelling a task does not clear its deployment record either; a legacy
/// task's `status --json` has no `environments`.
#[test]
fn cancelling_keeps_the_deployment_record() {
    let env = env_gate_setup("codex-openai", "error", "");
    let (ps1, sh) = counting_gate(&env, "c.log", 0);
    define_gates(
        &env,
        &machine_gate(&env, "dbsync", "d365-env", &ps1, &sh, ""),
    );
    create(&env, "EG-2");
    let s = script(
        &env,
        "eg2.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "EG-2", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let (_, err) = toryo(
        &env,
        &[
            "approve",
            "EG-2",
            "--gate",
            "plan",
            "--continue",
            "--fake-agents",
            &s,
        ],
        &[],
    );
    assert_eq!(task(&env, "EG-2").state, St::WaitingFinalApproval, "{err}");
    let (code, err) = toryo(&env, &["task", "cleanup", "EG-2", "--force"], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "EG-2").state, St::Cancelled);
    let d = deployments(&env);
    assert_eq!(d[0].task, "EG-2");
    let all = String::from_utf8(command(&env, &["status"], &[]).output().unwrap().stdout).unwrap();
    assert!(
        all.contains("(not approved — the task was cancelled)"),
        "{all}"
    );
    let legacy = setup("codex-openai");
    create(&legacy, "EG-3");
    let o = command(&legacy, &["status", "--json"], &[])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(v.get("environments").is_none(), "{v}");
}

/// M11-E3 AC-2 (Windows): the environment gate runs with the owner's normal token — it can
/// write outside the verify root — while a check gate runs at Low integrity and cannot.
#[cfg(windows)]
#[test]
fn an_environment_gate_has_the_normal_token_a_check_gate_low_integrity() {
    const PROBE: &str = "param($Target)\ntry { Set-Content -Path (Join-Path $Target ('marker-' + $env:TORYO_GATE_NAME + '.txt')) -Value hit -ErrorAction Stop; \"WROTE $Target\" } catch { \"DENIED $Target\" }\nexit 0\n";
    let org_tail = "";
    let env = env_gate_setup("codex-openai", "error", org_tail);
    let outside = env.root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    // the check gate `probe` in the repository, before dbsync
    let org = std::fs::read_to_string(env.repo.join(".toryo/toryo-orgchart.yaml"))
        .unwrap()
        .replacen(
            "gates:\n",
            &format!("gates:\n  probe: {{ kind: check, program: pwsh, args: [-NoProfile, -NonInteractive, -File, scripts/probe.ps1, '{}'], timeout: 2m }}\n", fwd(&outside)),
            1,
        );
    commit_repo(
        &env,
        &[
            (".toryo/toryo-orgchart.yaml", &org),
            (
                ".toryo/roles/implementer/toryo-role.yaml",
                "toryo: 1\npurpose: Change the code.\nbefore_handoff: [divide-present, probe, dbsync]\n",
            ),
            ("scripts/probe.ps1", &PROBE.replace("$env:TORYO_GATE_NAME", "'check'")),
        ],
        "probe",
    );
    let dbsync = PROBE.replace("$env:TORYO_GATE_NAME", "'env'").replace(
        "param($Target)\n",
        &format!("$Target = '{}'\n", fwd(&outside)),
    );
    define_gates(
        &env,
        &machine_gate(&env, "dbsync", "d365-env", &dbsync, "", ""),
    );
    // Low integrity for the check gates, with a verify root where a Low label can be set
    let cfg = std::fs::read_to_string(&env.config)
        .unwrap()
        .replace("boundary: none", "boundary: low_integrity");
    std::fs::write(&env.config, cfg).unwrap();
    create(&env, "EG-4");
    let s = script(
        &env,
        "eg4.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "EG-4", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let vr = verify_result(&env, "EG-4");
    assert_eq!(vr["boundary"], "low_integrity");
    let tail = |n: &str| {
        vr["commands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == n)
            .unwrap()["output_tail"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert!(tail("probe").contains("DENIED"), "{}", tail("probe"));
    assert!(tail("dbsync").contains("WROTE"), "{}", tail("dbsync"));
    assert!(outside.join("marker-env.txt").exists());
    assert!(!outside.join("marker-check.txt").exists());
}

/// A gate script that waits until `<root>/gates/<release>` exists (at most 90 s).
fn waiting_gate(env: &Env, release: &str) -> (String, String) {
    let r = fwd(&env.root.join("gates").join(release));
    (
        format!("$i = 0\nwhile (-not (Test-Path '{r}') -and $i -lt 900) {{ Start-Sleep -Milliseconds 100; $i++ }}\n'synced'\nexit 0\n"),
        format!("i=0\nwhile [ ! -e '{r}' ] && [ $i -lt 900 ]; do sleep 0.1; i=$((i+1)); done\necho synced\nexit 0\n"),
    )
}

fn spawn_run(env: &Env, args: &[&str], log: &str) -> std::process::Child {
    let f = std::fs::File::create(env.root.join(log)).unwrap();
    command(env, args, &[("TORYO_TEST_ENV_LOCK_POLL_MS", "200")])
        .stdout(std::process::Stdio::null())
        .stderr(f)
        .spawn()
        .unwrap()
}

/// Plan approved, so that `run` goes straight to implement → checkpoint → verify.
fn to_implementing(env: &Env, id: &str, s: &str) {
    create(env, id);
    let (code, err) = toryo(env, &["run", id, "--fake-agents", s], &[]);
    assert_eq!(code, 0, "{err}");
    let (code, err) = toryo(env, &["approve", id, "--gate", "plan"], &[]);
    assert_eq!(code, 0, "{err}");
}

/// M11-E3 AC-3: two tasks verifying at once with the same lock — the second waits, `status`
/// shows the holder, both finish; `toryo stop` while waiting stops cleanly.
#[test]
fn two_tasks_share_one_environment_lock() {
    let env = env_gate_setup("codex-openai", "error", "");
    let (ps1, sh) = waiting_gate(&env, "release");
    define_gates(
        &env,
        &machine_gate(&env, "dbsync", "d365-env", &ps1, &sh, ""),
    );
    let s = script(
        &env,
        "lk.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    for id in ["LK-A", "LK-B", "LK-C"] {
        to_implementing(&env, id, &s);
    }
    let mut a = spawn_run(
        &env,
        &["run", "LK-A", "--yes", "--fake-agents", &s],
        "a.log",
    );
    wait_until("LK-A holds the lock", 60, || {
        deployments(&env).iter().any(|d| d.task == "LK-A")
    });
    let mut b = spawn_run(
        &env,
        &["run", "LK-B", "--yes", "--fake-agents", &s],
        "b.log",
    );
    wait_until("LK-B waits", 60, || {
        status_out(&env, "LK-B").contains("waiting for lock d365-env (held by LK-A)")
    });
    assert!(
        status_out(&env, "LK-A").contains("d365-env holds LK-A @")
            && status_out(&env, "LK-A").contains("(deploying now)"),
        "{}",
        status_out(&env, "LK-A")
    );
    // a third task waits too, and is stopped while it waits
    let mut c = spawn_run(
        &env,
        &["run", "LK-C", "--yes", "--fake-agents", &s],
        "c.log",
    );
    wait_until("LK-C waits", 60, || {
        status_out(&env, "LK-C").contains("waiting for lock d365-env")
    });
    let (code, err) = toryo(&env, &["stop", "LK-C"], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(c.wait().unwrap().success());
    let tc = task(&env, "LK-C");
    assert_eq!(tc.state, St::Verifying);
    assert!(attempts(&env, "LK-C")
        .iter()
        .any(|x| x.stage == "verify" && x.status == store::AttemptStatus::Interrupted));
    assert!(!status_out(&env, "LK-C").contains("waiting for lock"));
    let ev = events(&env, "LK-B");
    assert!(
        ev.iter().any(|e| e["type"] == "lock_waiting"
            && e["lock"] == "d365-env"
            && e["holder"] == "LK-A"),
        "{ev:?}"
    );
    // release: A finishes, then B takes the lock and finishes
    std::fs::write(env.root.join("gates").join("release"), "go").unwrap();
    assert!(a.wait().unwrap().success());
    assert!(b.wait().unwrap().success());
    assert_eq!(task(&env, "LK-A").state, St::Completed);
    assert_eq!(task(&env, "LK-B").state, St::Completed);
    let d = deployments(&env);
    assert_eq!(
        (d.len(), d[0].task.as_str(), d[0].ok),
        (1, "LK-B", Some(true))
    );
    // and LK-C finishes on resume
    let (code, err) = toryo(&env, &["resume", "LK-C", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "LK-C").state, St::Completed, "{err}");
}

/// M11-E3 AC-5: a `secret_env` value the gate prints is redacted everywhere.
#[test]
fn an_environment_gate_secret_is_redacted_everywhere() {
    const SECRET: &str = "d365-sync-s3cr3t-77";
    let env = env_gate_setup("codex-openai", "error", "");
    define_gates(
        &env,
        &machine_gate(
            &env,
            "dbsync",
            "d365-env",
            "\"using $env:D365_SYNC_SECRET\"\nexit 0\n",
            "echo \"using $D365_SYNC_SECRET\"\nexit 0\n",
            "    secret_env: { D365_SYNC_SECRET: 'env:TORYO_M11_SYNC_SECRET' }\n    env: { D365_PLAIN: plain-value }\n",
        ),
    );
    create(&env, "EG-5");
    let s = script(
        &env,
        "eg5.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(
        &env,
        &["run", "EG-5", "--yes", "--fake-agents", &s],
        &[("TORYO_M11_SYNC_SECRET", SECRET)],
    );
    assert_eq!(code, 0, "{err}");
    assert!(!err.contains(SECRET), "{err}");
    let vr = verify_result(&env, "EG-5");
    let tail = vr["commands"][1]["output_tail"].as_str().unwrap();
    assert!(tail.contains("using [REDACTED:D365_SYNC_SECRET]"), "{tail}");
    assert!(!vr.to_string().contains("secret_env"), "argv only: {vr}");
    let rs = task(&env, "EG-5").run_store;
    let mut stack = vec![rs];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let text = String::from_utf8_lossy(&std::fs::read(&p).unwrap()).to_string();
                assert!(!text.contains(SECRET), "{} leaks", p.display());
            }
        }
    }
}

/// M11-E3 AC-7: `if_missing: skip` on a machine without the gate — a skipped entry and a
/// final-gate note; the check gates still run.
#[test]
fn a_skipped_environment_gate_is_recorded_and_noted() {
    let env = env_gate_setup("codex-openai", "skip", "");
    create(&env, "EG-6");
    let s = script(
        &env,
        "eg6.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "EG-6", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let (_, err) = toryo(
        &env,
        &[
            "approve",
            "EG-6",
            "--gate",
            "plan",
            "--continue",
            "--fake-agents",
            &s,
        ],
        &[],
    );
    assert_eq!(task(&env, "EG-6").state, St::WaitingFinalApproval, "{err}");
    let vr = verify_result(&env, "EG-6");
    let e = &vr["commands"][1];
    assert_eq!(
        (
            e["name"].clone(),
            e["kind"].clone(),
            e["skipped"].clone(),
            e["reason"].clone()
        ),
        (
            json!("dbsync"),
            json!("environment"),
            json!(true),
            json!("not defined on this machine")
        )
    );
    assert!(
        err.contains("final gate — dbsync did not run on this machine"),
        "{err}"
    );
    assert!(deployments(&env).is_empty());
}

/// M11-E3 AC-8: a gate script that changes a tracked file of the verify checkout →
/// `verify_mutated_tree`.
#[test]
fn an_environment_gate_that_writes_into_the_checkout_mutates_the_tree() {
    let env = env_gate_setup("codex-openai", "error", "");
    define_gates(
        &env,
        &machine_gate(
            &env,
            "dbsync",
            "d365-env",
            "Add-Content -Path (Join-Path $env:TORYO_VERIFY_CHECKOUT 'src/Calculator.psm1') -Value '# synced'\nexit 0\n",
            "echo '# synced' >> \"$TORYO_VERIFY_CHECKOUT/src/Calculator.psm1\"\nexit 0\n",
            "",
        ),
    );
    create(&env, "EG-7");
    let s = script(
        &env,
        "eg7.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
        }),
    );
    let (_, err) = toryo(&env, &["run", "EG-7", "--yes", "--fake-agents", &s], &[]);
    let t = task(&env, "EG-7");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert!(err.contains("verify_mutated_tree"), "{err}");
}

/// M11-E3 AC-9: toryo killed during the environment gate — the deployment record shows the
/// interrupted run (`ok` empty); `resume` runs verify again from the check gates.
#[test]
fn a_crash_during_the_environment_gate_is_recovered_by_resume() {
    let env = env_gate_setup("codex-openai", "error", "");
    let started = fwd(&env.root.join("gates").join("started"));
    define_gates(
        &env,
        &machine_gate(
            &env,
            "dbsync",
            "d365-env",
            &format!("if (Test-Path '{started}') {{ 'second run'; exit 0 }}\nSet-Content -Path '{started}' -Value x\nStart-Sleep -Seconds 120\nexit 0\n"),
            &format!("if [ -e '{started}' ]; then echo 'second run'; exit 0; fi\necho x > '{started}'\nsleep 120\nexit 0\n"),
            "",
        ),
    );
    let s = script(
        &env,
        "eg8.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    to_implementing(&env, "EG-8", &s);
    let mut run = spawn_run(
        &env,
        &["run", "EG-8", "--yes", "--fake-agents", &s],
        "eg8.log",
    );
    let marker = PathBuf::from(&started);
    wait_until("the gate started", 90, || marker.exists());
    run.kill().unwrap();
    let _ = run.wait();
    let d = deployments(&env);
    assert_eq!(d.len(), 1);
    assert_eq!(
        (d[0].finished_at.clone(), d[0].ok),
        (None, None),
        "interrupted"
    );
    let out = status_out(&env, "EG-8");
    assert!(
        out.contains("(interrupted — the environment may hold a partial deploy)"),
        "{out}"
    );
    let (code, err) = toryo(&env, &["resume", "EG-8", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "EG-8").state, St::Completed, "{err}");
    let vr = verify_result(&env, "EG-8");
    assert_eq!(
        vr["commands"][0]["name"], "divide-present",
        "the check gates ran again"
    );
    assert!(vr["commands"][1]["output_tail"]
        .as_str()
        .unwrap()
        .contains("second run"));
    let d = deployments(&env);
    assert_eq!(d[0].ok, Some(true));
}

/// Group E review: an environment gate this machine cannot run — an unresolved `secret_env`,
/// a program that does not exist — escalates with `environment_gate_unavailable` and leaves
/// the deployment record alone.
#[test]
fn an_environment_gate_that_cannot_run_escalates() {
    for (id, extra, program) in [
        (
            "UN-1",
            "    secret_env: { K: 'env:TORYO_M11_NEVER_SET_SECRET' }\n",
            None,
        ),
        (
            "UN-2",
            "",
            Some(if cfg!(windows) {
                "'C:/no/such/sync.exe'"
            } else {
                "'/no/such/sync'"
            }),
        ),
    ] {
        let env = env_gate_setup("codex-openai", "error", "");
        let mut g = machine_gate(&env, "dbsync", "d365-env", "exit 0\n", "exit 0\n", extra);
        if let Some(p) = program {
            let line = g
                .lines()
                .find(|l| l.contains("program:"))
                .unwrap()
                .to_string();
            g = g.replace(&line, &format!("    program: {p}"));
            g = g.replace(
                &g.lines().find(|l| l.contains("args:")).unwrap().to_string(),
                "    args: []",
            );
        }
        define_gates(&env, &g);
        create(&env, id);
        let s = script(
            &env,
            &format!("{id}.json"),
            json!({
                "planner": [turn(vec![plan()])],
                "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            }),
        );
        let (_, err) = toryo(&env, &["run", id, "--yes", "--fake-agents", &s], &[]);
        assert_eq!(task(&env, id).state, St::Escalated, "{id}: {err}");
        assert!(
            err.contains("environment_gate_unavailable: environment gate `dbsync`"),
            "{id}: {err}"
        );
        // M11-R12 (item 3): the hint names no machine-config file (a frozen config has no path)
        assert!(
            !err.contains("toryo.local.yaml")
                && err.contains("fix the machine config, then `toryo resume"),
            "{id}: {err}"
        );
        let ev = events(&env, id);
        assert!(
            ev.iter()
                .any(|e| e["type"] == "escalated"
                    && e["reason_class"] == "environment_gate_unavailable"),
            "{id}: {ev:?}"
        );
        assert!(deployments(&env).is_empty(), "{id}: nothing ran");
    }
}

/// Group E review: `toryo stop` while an environment gate runs — the gate is killed, the
/// attempt is interrupted, and the record shows an interrupted deploy (`ok` empty).
#[test]
fn a_stop_during_an_environment_gate_interrupts_it() {
    let env = env_gate_setup("codex-openai", "error", "");
    let (ps1, sh) = waiting_gate(&env, "never");
    define_gates(
        &env,
        &machine_gate(&env, "dbsync", "d365-env", &ps1, &sh, ""),
    );
    let s = script(
        &env,
        "st.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
        }),
    );
    to_implementing(&env, "ST-1", &s);
    let mut run = spawn_run(
        &env,
        &["run", "ST-1", "--yes", "--fake-agents", &s],
        "st.log",
    );
    wait_until("the gate runs", 60, || {
        deployments(&env).iter().any(|d| d.task == "ST-1")
    });
    let (code, err) = toryo(&env, &["stop", "ST-1", "--wait-secs", "30"], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(run.wait().unwrap().success());
    assert_eq!(task(&env, "ST-1").state, St::Verifying);
    let d = deployments(&env);
    assert!(d[0].finished_at.is_some() && d[0].ok.is_none(), "{d:?}");
    assert!(
        status_out(&env, "ST-1").contains("(interrupted"),
        "{}",
        status_out(&env, "ST-1")
    );
}

/// The fixture with a repository check gate `probe` (the script `ps1` / `sh`) between
/// `divide-present` and the environment gate `dbsync`, whose machine script writes a marker.
fn probe_then_dbsync(ps1: &str, sh: &str) -> (Env, PathBuf) {
    let (decl, file, body) = if cfg!(windows) {
        (
            "  probe: { kind: check, program: pwsh, args: [-NoProfile, -NonInteractive, -File, scripts/probe.ps1], timeout: 2m }\n",
            "scripts/probe.ps1",
            ps1,
        )
    } else {
        (
            "  probe: { kind: check, program: sh, args: [scripts/probe.sh], timeout: 2m }\n",
            "scripts/probe.sh",
            sh,
        )
    };
    let org = ORGCHART.replace("IMPLEMENTER", "codex-openai").replacen(
        "gates:\n",
        &format!(
            "gates:\n{decl}  dbsync: {{ kind: environment, timeout: 2m, if_missing: error }}\n"
        ),
        1,
    );
    let env = setup_orgchart(
        "codex-openai",
        "",
        &[
            (".toryo/toryo-orgchart.yaml", &org),
            (
                ".toryo/roles/implementer/toryo-role.yaml",
                "toryo: 1\npurpose: Change the code.\nbefore_handoff: [divide-present, probe, dbsync]\n",
            ),
            (file, body),
        ],
    );
    let marker = env.root.join("gates").join("deployed");
    let m = fwd(&marker);
    // M11-R9: the gate also writes what it sees as TORYO_VERIFY_CHECKOUT to `gates/seen.txt` —
    // `co=<path>`, then its files (relative, `/`), hidden ones included
    let seen = fwd(&env.root.join("gates").join("seen.txt"));
    define_gates(
        &env,
        &machine_gate(
            &env,
            "dbsync",
            "d365-env",
            &format!("Set-Content -Path '{m}' -Value x\n\"cwd=$((Get-Location).Path)\"\n\"repo-file=$(Test-Path src/Calculator.psm1)\"\n$c = $env:TORYO_VERIFY_CHECKOUT\nSet-Content -Path '{seen}' -Value \"co=$c\"\nGet-ChildItem -LiteralPath $c -Recurse -Force | Where-Object {{ -not $_.PSIsContainer -or $_.Name -eq '.git' }} | ForEach-Object {{ $_.FullName.Substring($c.Length + 1).Replace('\\', '/') }} | Add-Content -Path '{seen}'\nexit 0\n"),
            &format!("echo x > '{m}'\necho \"cwd=$(pwd)\"\nif [ -e src/Calculator.psm1 ]; then echo repo-file=True; else echo repo-file=False; fi\nc=\"$TORYO_VERIFY_CHECKOUT\"\necho \"co=$c\" > '{seen}'\n(cd \"$c\" && find . \\( -type f -o -name .git \\) | sed 's|^\\./||') >> '{seen}'\nexit 0\n"),
            "",
        ),
    );
    (env, marker)
}

/// What the environment gate of [`probe_then_dbsync`] saw: its TORYO_VERIFY_CHECKOUT and the
/// files there, sorted.
fn seen_by_the_gate(env: &Env) -> (PathBuf, Vec<String>) {
    let text = std::fs::read_to_string(env.root.join("gates").join("seen.txt")).unwrap();
    let mut lines = text
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty());
    let co = PathBuf::from(lines.next().unwrap().strip_prefix("co=").unwrap());
    let mut files: Vec<String> = lines.collect();
    files.sort();
    (co, files)
}

/// The files of the task's checkpoint commit, sorted.
fn checkpoint_files(env: &Env, id: &str) -> Vec<String> {
    let t = task(env, id);
    let head = t.machine.checkpoint().unwrap().to_string();
    let mut files: Vec<String> = git(&wt(env, id), &["ls-tree", "-r", "--name-only", &head])
        .lines()
        .map(str::to_string)
        .collect();
    files.sort();
    files
}

fn one_turn(env: &Env, id: &str) -> String {
    create(env, id);
    let s = script(
        env,
        &format!("{id}.json"),
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (_, err) = toryo(env, &["run", id, "--yes", "--fake-agents", &s], &[]);
    err
}

/// M11-R9 AC-2 (X6; was `a_check_gate_that_adds_a_file_stops_the_deploy`, R2 AC-1): a check gate
/// that adds a file to the Low checkout no longer stops the deploy — the environment gate runs
/// from a fresh export of the commit: its TORYO_VERIFY_CHECKOUT is under `deploy`, differs from
/// the check gate's, holds exactly the commit's files and no `.git`. The extra file is a warning
/// and `checkout_extra` in VERIFY_RESULT.json.
#[test]
fn a_check_gate_that_adds_a_file_is_not_deployed() {
    let (env, marker) = probe_then_dbsync(
        "Set-Content -Path (Join-Path $env:TORYO_VERIFY_CHECKOUT 'stray.txt') -Value x\nexit 0\n",
        "echo x > \"$TORYO_VERIFY_CHECKOUT/stray.txt\"\nexit 0\n",
    );
    let err = one_turn(&env, "R2-1");
    let t = task(&env, "R2-1");
    assert_eq!(t.state, St::Completed, "{err}");
    assert!(marker.exists(), "the environment gate ran");
    assert!(
        err.contains("!! a check gate left files in the verify checkout — they are not deployed")
            && err.contains("stray.txt"),
        "{err}"
    );
    let vr = verify_result(&env, "R2-1");
    assert_eq!(vr["pass"], true);
    assert_eq!(vr["checkout_extra"], json!(["stray.txt"]));
    let gate = vr["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["kind"] == "environment")
        .unwrap()
        .clone();
    assert_eq!(gate["source"], "export");
    let head = t.machine.checkpoint().unwrap().to_string();
    let tree = git(
        &wt(&env, "R2-1"),
        &["rev-parse", &format!("{head}^{{tree}}")],
    );
    assert_eq!(gate["source_tree"].as_str().unwrap(), tree.trim());
    let (co, files) = seen_by_the_gate(&env);
    assert_eq!(co.file_name().unwrap(), "deploy", "{}", co.display());
    let checkout = PathBuf::from(vr["verify_checkout"].as_str().unwrap());
    assert_ne!(
        std::fs::canonicalize(&co).ok(),
        std::fs::canonicalize(&checkout).ok(),
        "not the check gates' checkout"
    );
    assert_eq!(
        files,
        checkpoint_files(&env, "R2-1"),
        "exactly the commit's files"
    );
    assert!(!files.iter().any(|f| f == ".git" || f.contains("stray")));
    let d = deployments(&env);
    assert_eq!((d.len(), d[0].ok), (1, Some(true)));
}

/// M11-R2 AC-2: a check gate that edits a tracked file → `verify_mutated_tree`, no environment
/// gate.
#[test]
fn a_check_gate_that_edits_a_tracked_file_stops_the_deploy() {
    let (env, marker) = probe_then_dbsync(
        "Add-Content -Path (Join-Path $env:TORYO_VERIFY_CHECKOUT 'src/Calculator.psm1') -Value '# edited'\nexit 0\n",
        "echo '# edited' >> \"$TORYO_VERIFY_CHECKOUT/src/Calculator.psm1\"\nexit 0\n",
    );
    let err = one_turn(&env, "R2-2");
    assert_eq!(task(&env, "R2-2").state, St::Escalated, "{err}");
    assert!(err.contains("verify_mutated_tree"), "{err}");
    assert!(!marker.exists());
    assert!(deployments(&env).is_empty());
}

/// M11-R2 AC-3 (X1): the environment gate runs in the empty `envgate` folder — a file that
/// exists only in the checkout is not found relative to it.
#[test]
fn an_environment_gate_runs_in_a_neutral_folder() {
    let (env, marker) = probe_then_dbsync("exit 0\n", "exit 0\n");
    let err = one_turn(&env, "R2-3");
    assert_eq!(task(&env, "R2-3").state, St::Completed, "{err}");
    assert!(marker.exists());
    let vr = verify_result(&env, "R2-3");
    let tail = vr["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["kind"] == "environment")
        .unwrap()["output_tail"]
        .as_str()
        .unwrap()
        .to_string();
    let checkout = PathBuf::from(vr["verify_checkout"].as_str().unwrap());
    let envgate = checkout.parent().unwrap().join("envgate");
    let cwd = tail
        .lines()
        .find_map(|l| l.strip_prefix("cwd="))
        .unwrap()
        .trim()
        .to_string();
    assert_eq!(
        std::fs::canonicalize(&cwd).unwrap(),
        std::fs::canonicalize(&envgate).unwrap(),
        "{tail}"
    );
    assert!(tail.contains("repo-file=False"), "{tail}");
}

/// M11-R2 AC-4 (Windows): a check gate at Low integrity cannot create a file in the `envgate`
/// folder (nor the folder itself).
#[cfg(windows)]
#[test]
fn a_low_integrity_check_gate_cannot_write_the_envgate_folder() {
    let (env, _) = probe_then_dbsync(
        "$d = Join-Path (Split-Path $env:TORYO_VERIFY_BUILD -Parent) 'envgate'\ntry { New-Item -ItemType Directory -Force -Path $d -ErrorAction Stop | Out-Null; Set-Content -Path (Join-Path $d 'planted.exe') -Value x -ErrorAction Stop; 'PLANTED' } catch { 'DENIED' }\nexit 0\n",
        "",
    );
    let cfg = std::fs::read_to_string(&env.config)
        .unwrap()
        .replace("boundary: none", "boundary: low_integrity");
    std::fs::write(&env.config, cfg).unwrap();
    let err = one_turn(&env, "R2-4");
    assert_eq!(task(&env, "R2-4").state, St::Completed, "{err}");
    let vr = verify_result(&env, "R2-4");
    assert_eq!(vr["boundary"], "low_integrity");
    let probe = vr["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "probe")
        .unwrap()["output_tail"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(probe.contains("DENIED"), "{probe}");
    let checkout = PathBuf::from(vr["verify_checkout"].as_str().unwrap());
    assert!(!checkout
        .parent()
        .unwrap()
        .join("envgate")
        .join("planted.exe")
        .exists());
}

/// M11-R3 AC-1 (X3): `%VAR%` in an environment gate's args is expanded once, when the task is
/// created — the frozen config holds the expanded path, and `run` (without the variable)
/// runs the script from it.
#[test]
fn an_environment_gate_variable_is_expanded_once_at_create() {
    let env = env_gate_setup("codex-openai", "error", "");
    let gates = env.root.join("gates");
    let marker = fwd(&gates.join("deployed"));
    let entry = machine_gate(
        &env,
        "dbsync",
        "d365-env",
        &format!("Set-Content -Path '{marker}' -Value x\nexit 0\n"),
        &format!("echo x > '{marker}'\nexit 0\n"),
        "",
    );
    let entry = entry.replace(&fwd(&gates), "%TORYO_R3_GATES%");
    assert!(entry.contains("'%TORYO_R3_GATES%/dbsync."), "{entry}");
    define_gates(&env, &entry);
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "R3-1"],
        &[("TORYO_R3_GATES", &fwd(&gates))],
    );
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "R3-1");
    let frozen = std::fs::read_to_string(t.artifacts().join("config.json")).unwrap();
    assert!(!frozen.contains("%TORYO_R3_GATES%"), "{frozen}");
    let ext = if cfg!(windows) { "ps1" } else { "sh" };
    let want = serde_json::to_string(&format!("{}/dbsync.{ext}", fwd(&gates))).unwrap();
    assert!(frozen.contains(&want), "{want}\n{frozen}");
    let s = script(
        &env,
        "r3-1.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (_, err) = toryo(&env, &["run", "R3-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(task(&env, "R3-1").state, St::Completed, "{err}");
    assert!(gates.join("deployed").exists(), "the script ran: {err}");
}

/// M11-R2 review, M11-R9 (was `files_git_cannot_see_stop_the_deploy`): what `git status` cannot
/// see is not deployed — a gitlink in the checkpoint (Git never looks inside it) that a check
/// gate fills, and an entry named `.git` below the root (Git skips it).
#[test]
fn files_git_cannot_see_are_not_deployed() {
    let (env, marker) = probe_then_dbsync(
        "$c = $env:TORYO_VERIFY_CHECKOUT\nNew-Item -ItemType Directory -Force (Join-Path $c 'deploy') | Out-Null\nSet-Content -Path (Join-Path $c 'deploy/evil.xml') -Value x\nNew-Item -ItemType Directory -Force (Join-Path $c 'src/.git') | Out-Null\nSet-Content -Path (Join-Path $c 'src/.git/payload') -Value x\nexit 0\n",
        "c=\"$TORYO_VERIFY_CHECKOUT\"\nmkdir -p \"$c/deploy\" \"$c/src/.git\"\necho x > \"$c/deploy/evil.xml\"\necho x > \"$c/src/.git/payload\"\nexit 0\n",
    );
    // the checkpoint holds a gitlink `deploy` (a nested repository, no .gitmodules)
    let head = git(&env.repo, &["rev-parse", "HEAD"]);
    git(
        &env.repo,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{},deploy", head.trim()),
        ],
    );
    git(&env.repo, &["commit", "-q", "-m", "a gitlink"]);
    let err = one_turn(&env, "R2-G");
    let t = task(&env, "R2-G");
    // M11-R9 (X6; this R2-review test stopped the deploy): what Git cannot see is listed and
    // not deployed — the gate reads the export, which has neither file
    assert_eq!(t.state, St::Completed, "{err}");
    assert!(marker.exists(), "the environment gate ran");
    let vr = verify_result(&env, "R2-G");
    let extra: Vec<String> = vr["checkout_extra"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    // (R9 review: a checkpoint gitlink itself is not something a check gate left; what the gate
    // wrote inside it, and the `.git` entry, are)
    assert!(
        extra.iter().any(|x| x == "deploy/evil.xml")
            && extra.iter().any(|x| x == "src/.git (a `.git` entry)"),
        "{extra:?}"
    );
    let (_, files) = seen_by_the_gate(&env);
    assert!(
        !files
            .iter()
            .any(|f| f.contains("evil") || f.contains("payload") || f.ends_with(".git")),
        "{files:?}"
    );
}

/// M11-R8: an environment gate that runs past its timeout (6 s: room for two pwsh start-ups)
/// is killed with its whole process tree, and the deployment record says interrupted (the
/// environment may hold a partial deploy) — not "the deploy failed".
#[test]
fn a_timed_out_environment_gate_is_recorded_as_interrupted() {
    let env = env_gate_setup(
        "codex-openai",
        "error",
        "limits:\n  max_gate_retries: 1\n  max_review_rounds: 1\n",
    );
    let org_path = ".toryo/toryo-orgchart.yaml";
    let org = std::fs::read_to_string(env.repo.join(org_path)).unwrap();
    assert!(org.contains("timeout: 2m, if_missing"), "{org}");
    commit_repo(
        &env,
        &[(
            org_path,
            &org.replace("timeout: 2m, if_missing", "timeout: 6s, if_missing"),
        )],
        "a short gate timeout",
    );
    let hb = fwd(&env.root.join("gates").join("heartbeat.txt"));
    let ps1 = "Start-Process -NoNewWindow -FilePath pwsh -ArgumentList '-NoProfile','-Command',\"while (`$true) { Add-Content -Path 'HB' -Value x; Start-Sleep -Milliseconds 200 }\"\nStart-Sleep -Seconds 120\n".replace("HB", &hb);
    let sh = "( while :; do echo x >> 'HB'; sleep 0.2; done ) &\nsleep 120\n".replace("HB", &hb);
    define_gates(
        &env,
        &machine_gate(&env, "dbsync", "d365-env", &ps1, &sh, ""),
    );
    create(&env, "TO-1");
    let s = script(
        &env,
        "to.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [
                turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()]),
                turn(vec![implementation()]),
                turn(vec![implementation()]),
            ],
        }),
    );
    let (_, err) = toryo(&env, &["run", "TO-1", "--yes", "--fake-agents", &s], &[]);
    let d = deployments(&env);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(
        d[0].finished_at.is_some() && d[0].ok.is_none(),
        "{d:?}\n{err}"
    );
    let out = status_out(&env, "TO-1");
    assert!(
        out.contains("(interrupted — the environment may hold a partial deploy)"),
        "{out}"
    );
    // the gate's child kept writing until the tree was killed: nothing writes any more
    let size = || {
        std::fs::metadata(env.root.join("gates").join("heartbeat.txt"))
            .map(|m| m.len())
            .unwrap_or(0)
    };
    assert!(size() > 0, "the child ran");
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let before = size();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert_eq!(size(), before, "the whole process tree is gone");
}

/// M11-R8 (E3 AC-7 end to end): `if_missing: error` and a machine config without the gate →
/// `task create` is refused, naming `environment_gates.dbsync` — no task, no worktree.
#[test]
fn a_missing_environment_gate_refuses_the_task() {
    let env = env_gate_setup("codex-openai", "error", "");
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "IM-1"],
        &[],
    );
    assert_ne!(code, 0);
    assert!(
        err.contains("environment gate 'dbsync' is not defined on this machine")
            && err.contains("add `environment_gates.dbsync` to the machine config")
            && err.contains(&env.machine.display().to_string()),
        "{err}"
    );
    assert!(!env.data.join("runs").join("IM-1").exists());
    assert!(!wt(&env, "IM-1").exists());
}

/// M11-R8: lock names are one lock whatever their case — `D365-Env` is stored as `d365-env`
/// (the deployment row, `status`).
#[test]
fn a_lock_name_is_stored_in_lower_case() {
    let env = env_gate_setup("codex-openai", "error", "");
    define_gates(
        &env,
        &machine_gate(&env, "dbsync", "D365-Env", "exit 0\n", "exit 0\n", ""),
    );
    create(&env, "LC-1");
    let s = script(
        &env,
        "lc.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (_, err) = toryo(&env, &["run", "LC-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(task(&env, "LC-1").state, St::Completed, "{err}");
    let d = deployments(&env);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].lock, "d365-env");
    let out = status_out(&env, "LC-1");
    assert!(
        out.contains("d365-env holds LC-1 @") && out.contains("(approved)"),
        "{out}"
    );
}

/// The `output_tail` of the gate `name` in the latest VERIFY_RESULT.json of `id`.
#[cfg(windows)]
fn gate_tail(env: &Env, id: &str, name: &str) -> String {
    verify_result(env, id)["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .map(|c| c["output_tail"].as_str().unwrap_or_default().to_string())
        .unwrap_or_default()
}

/// M11-R9 AC-3 (Windows): the forgery R9 is about — a check gate overwrites a tracked file with
/// content of the same length and puts its timestamps back. Whatever `mutated` says, what the
/// environment gate reads has the committed bytes: either the task escalates before any deploy,
/// or the deployed file's SHA-256 is the original's, never the forged one's. (Whether Git for
/// Windows catches this forgery depends on index raciness; the export never carries the
/// checkout's bytes regardless — the deterministic proof is `the_export_holds_the_commit_not_the
/// _checkout` and `a_check_gate_that_adds_a_file_is_not_deployed`.)
#[cfg(windows)]
#[test]
fn a_same_length_forgery_in_the_checkout_is_never_deployed() {
    let (env, marker) = probe_then_dbsync(
        "$f = Join-Path $env:TORYO_VERIFY_CHECKOUT 'src/Calculator.psm1'\n$i = Get-Item $f\n$w = $i.LastWriteTimeUtc\n$c = $i.CreationTimeUtc\n\"orig=$((Get-FileHash $f -Algorithm SHA256).Hash)\"\n$b = [IO.File]::ReadAllBytes($f)\n$b[0] = if ($b[0] -eq 35) { 36 } else { 35 }\n[IO.File]::WriteAllBytes($f, $b)\n$i = Get-Item $f\n$i.LastWriteTimeUtc = $w\n$i.CreationTimeUtc = $c\n\"forged=$((Get-FileHash $f -Algorithm SHA256).Hash)\"\nexit 0\n",
        "",
    );
    // the environment gate records the hash of the file it would deploy
    let got = env.root.join("gates").join("deployed-hash.txt");
    std::fs::write(
        env.root.join("gates").join("dbsync.ps1"),
        format!(
            "Set-Content -Path '{}' -Value x\n(Get-FileHash (Join-Path $env:TORYO_VERIFY_CHECKOUT 'src/Calculator.psm1') -Algorithm SHA256).Hash | Set-Content -Path '{}'\nexit 0\n",
            fwd(&marker),
            fwd(&got)
        ),
    )
    .unwrap();
    let err = one_turn(&env, "R9-F");
    let tail = gate_tail(&env, "R9-F", "probe");
    let line = |k: &str| {
        tail.lines()
            .find_map(|l| l.trim().strip_prefix(k))
            .unwrap_or_else(|| panic!("{k}: {tail}"))
            .to_string()
    };
    let (orig, forged) = (line("orig="), line("forged="));
    assert_ne!(orig, forged, "the probe forged the file");
    let t = task(&env, "R9-F");
    if t.state == St::Escalated {
        // Git saw it: no environment gate started
        assert!(err.contains("verify_mutated_tree"), "{err}");
        assert!(!marker.exists() && deployments(&env).is_empty());
    } else {
        assert_eq!(t.state, St::Completed, "{err}");
        let deployed = std::fs::read_to_string(&got).unwrap().trim().to_string();
        assert_eq!(deployed, orig, "the export holds the committed bytes");
    }
}

/// M11-R9 AC-4 (Windows): a check gate at Low integrity cannot create a file in `deploy` (also
/// once it exists, on the second verify run) nor next to the scratch index.
#[cfg(windows)]
#[test]
fn a_low_integrity_check_gate_cannot_write_the_export() {
    let (env, _) = probe_then_dbsync(
        "$b = Split-Path $env:TORYO_VERIFY_BUILD -Parent\nforeach ($p in @((Join-Path $b 'deploy/planted.exe'), (Join-Path $b 'deploy.index'), (Join-Path $b 'planted.index'))) {\n  try { New-Item -ItemType Directory -Force -Path (Split-Path $p) -ErrorAction Stop | Out-Null; Set-Content -Path $p -Value x -ErrorAction Stop; \"PLANTED $p\" } catch { 'DENIED' }\n}\nexit 0\n",
        "",
    );
    let cfg = std::fs::read_to_string(&env.config)
        .unwrap()
        .replace("boundary: none", "boundary: low_integrity");
    std::fs::write(&env.config, cfg).unwrap();
    create(&env, "R9-L");
    // two verify runs: the second one finds `deploy` and `deploy.index` from the first
    let s = script(
        &env,
        "r9l.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [
                turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()]),
                turn(vec![implementation()]),
            ],
            "reviewer": [turn(vec![changes()]), turn(vec![approved()])],
        }),
    );
    let (_, err) = toryo(&env, &["run", "R9-L", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(task(&env, "R9-L").state, St::Completed, "{err}");
    let vr = verify_result(&env, "R9-L");
    assert_eq!(vr["boundary"], "low_integrity");
    let probe = gate_tail(&env, "R9-L", "probe");
    assert!(
        probe.matches("DENIED").count() == 3 && !probe.contains("PLANTED"),
        "{probe}"
    );
    let base = PathBuf::from(vr["verify_checkout"].as_str().unwrap())
        .parent()
        .unwrap()
        .to_path_buf();
    assert!(base.join("deploy").is_dir() && base.join("deploy.index").is_file());
    assert!(!base.join("deploy").join("planted.exe").exists());
    assert!(!base.join("planted.index").exists());
}

/// M11-R11 AC-2 (X8): an environment gate the machine cannot run (an unresolved secret) →
/// `environment_gate_unavailable`; once the machine config is fixed, a plain `toryo resume
/// <id>` goes back into verify — the check gates run again, no implementer turn, no fix cycle.
#[test]
fn plain_resume_after_an_unavailable_gate_costs_no_fix_cycle() {
    let env = env_gate_setup("codex-openai", "error", "");
    define_gates(
        &env,
        &machine_gate(
            &env,
            "dbsync",
            "d365-env",
            "exit 0\n",
            "exit 0\n",
            "    secret_env: { K: 'env:TORYO_M11_RS_SECRET' }\n",
        ),
    );
    create(&env, "RS-E");
    let s = script(
        &env,
        "rse.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    // the secret is not set on this first run
    let (_, err) = toryo(&env, &["run", "RS-E", "--yes", "--fake-agents", &s], &[]);
    let t = task(&env, "RS-E");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert_eq!(
        t.machine.reason_class(),
        Some("environment_gate_unavailable")
    );
    assert_eq!(t.fix_cycle, 0);
    // fix the cause (the secret is now resolvable) and plain-resume
    let (code, err) = toryo(
        &env,
        &["resume", "RS-E", "--yes", "--fake-agents", &s],
        &[("TORYO_M11_RS_SECRET", "ok")],
    );
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "RS-E");
    assert_eq!(t.state, St::Completed, "state={:?} err={err}", t.state);
    assert_eq!(t.fix_cycle, 0, "no fix cycle was spent");
    assert!(t.machine.reason_class().is_none(), "the class is cleared");
    // no implementer turn ran during the resume (calls are per lock generation); the check
    // gates ran again
    assert_eq!(
        calls(&env, "RS-E")
            .iter()
            .filter(|c| c["key"]
                .as_str()
                .is_some_and(|k| k.starts_with("implementer")))
            .count(),
        0
    );
    let vr = verify_result(&env, "RS-E");
    assert_eq!(vr["commands"][0]["name"], "divide-present");
    assert_eq!(
        deployments(&env)
            .iter()
            .filter(|d| d.task == "RS-E")
            .count(),
        1
    );
    let dec = std::fs::read_to_string(t.artifacts().join("DECISIONS.md")).unwrap_or_default();
    assert!(
        dec.contains("resumed after environment_gate_unavailable"),
        "{dec}"
    );
}
