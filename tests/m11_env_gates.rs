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
