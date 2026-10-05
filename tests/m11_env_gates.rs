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
    env_gate_setup_timeout(implementer, if_missing, org_tail, "2m")
}

/// [`env_gate_setup`] with the environment gate's `timeout:` (M12-06: a few seconds).
fn env_gate_setup_timeout(
    implementer: &str,
    if_missing: &str,
    org_tail: &str,
    timeout: &str,
) -> Env {
    let org = ORGCHART.replace("IMPLEMENTER", implementer).replacen(
        "gates:\n",
        &format!(
            "gates:\n  dbsync: {{ kind: environment, timeout: {timeout}, if_missing: {if_missing} }}\n"
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
        // M11-R12 (item 3): the hint names no machine-config file (a frozen config has no path).
        // M11-R16 (changed): it tells both ways out — fixed on the machine: plain resume; fixed
        // in the machine config: `task config --reload` first (the gate's command is frozen)
        assert!(
            !err.contains("toryo.local.yaml")
                && err.contains(&format!(
                    "fixed on the machine (a value added to Credential Manager, a program installed): `toryo resume {id}`; fixed in a config file (`environment_gates.dbsync` in the machine config — frozen with the task): `toryo task config {id} --reload`, then `toryo resume {id}`"
                )),
            "{id}: {err}"
        );
        let ev = events(&env, id);
        // M11-R16 (X12): found before the build — no check gate ran, no result was written
        assert!(
            !ev.iter().any(|e| e["type"] == "verify_command"),
            "{id}: {ev:?}"
        );
        assert!(
            !task(&env, id)
                .artifacts()
                .join("VERIFY_RESULT.json")
                .exists(),
            "{id}"
        );
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
    probe_then_dbsync_timed(ps1, sh, "2m")
}

/// [`probe_then_dbsync`] with the check gate `probe`'s `timeout:` (M12-06).
fn probe_then_dbsync_timed(ps1: &str, sh: &str, probe_timeout: &str) -> (Env, PathBuf) {
    let (decl, file, body) = if cfg!(windows) {
        (
            format!("  probe: {{ kind: check, program: pwsh, args: [-NoProfile, -NonInteractive, -File, scripts/probe.ps1], timeout: {probe_timeout} }}\n"),
            "scripts/probe.ps1",
            ps1,
        )
    } else {
        (
            format!("  probe: {{ kind: check, program: sh, args: [scripts/probe.sh], timeout: {probe_timeout} }}\n"),
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
    // `co=<path>`, then its files (relative, `/`), hidden ones included; listed with `-Name`, not
    // cut from FullName, so a short (8.3) spelling of the path cannot shift them (M11-ci)
    let seen = fwd(&env.root.join("gates").join("seen.txt"));
    define_gates(
        &env,
        &machine_gate(
            &env,
            "dbsync",
            "d365-env",
            &format!("Set-Content -Path '{m}' -Value x\n\"cwd=$((Get-Location).Path)\"\n\"repo-file=$(Test-Path src/Calculator.psm1)\"\n$c = $env:TORYO_VERIFY_CHECKOUT\nSet-Content -Path '{seen}' -Value \"co=$c\"\nGet-ChildItem -LiteralPath $c -Recurse -Force -Name | Where-Object {{ (Test-Path -LiteralPath (Join-Path $c $_) -PathType Leaf) -or (Split-Path $_ -Leaf) -eq '.git' }} | ForEach-Object {{ $_.Replace('\\', '/') }} | Add-Content -Path '{seen}'\nexit 0\n"),
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

/// M11-R9 AC-3 / R13 (Windows): the forgery R9 is about, made the way Git cannot see it. A check
/// gate overwrites a tracked file with content of the same length and puts its timestamps back —
/// on the **second** verify run, when the file was written by an earlier checkout and the
/// checkout's index is newer than it (no "racy" second look: `verify::mutated` says unchanged).
/// The environment gate therefore **runs**, and the file it reads has the committed SHA-256:
/// it reads the export, never the Low checkout.
#[cfg(windows)]
#[test]
fn a_same_length_forgery_in_the_checkout_is_never_deployed() {
    let (env, marker) = probe_then_dbsync("exit 0\n", "");
    let flag = fwd(&env.root.join("gates").join("armed"));
    // run 1: arm, and let more than a second pass; run 2: forge
    let probe = format!(
        "if (-not (Test-Path '{flag}')) {{ Set-Content -Path '{flag}' -Value x; Start-Sleep -Milliseconds 2200; 'armed'; exit 0 }}\n$f = Join-Path $env:TORYO_VERIFY_CHECKOUT 'src/Calculator.psm1'\n$i = Get-Item $f\n$w = $i.LastWriteTimeUtc\n$c = $i.CreationTimeUtc\n\"orig=$((Get-FileHash $f -Algorithm SHA256).Hash)\"\n$b = [IO.File]::ReadAllBytes($f)\n$b[0] = if ($b[0] -eq 35) {{ 36 }} else {{ 35 }}\n[IO.File]::WriteAllBytes($f, $b)\n$i = Get-Item $f\n$i.LastWriteTimeUtc = $w\n$i.CreationTimeUtc = $c\n\"forged=$((Get-FileHash $f -Algorithm SHA256).Hash)\"\nexit 0\n"
    );
    commit_repo(&env, &[("scripts/probe.ps1", &probe)], "the forging probe");
    // the environment gate records, per run, the hash of the file it would deploy
    let got = env.root.join("gates").join("deployed-hash.txt");
    std::fs::write(
        env.root.join("gates").join("dbsync.ps1"),
        format!(
            "Set-Content -Path '{}' -Value x\n(Get-FileHash (Join-Path $env:TORYO_VERIFY_CHECKOUT 'src/Calculator.psm1') -Algorithm SHA256).Hash | Add-Content -Path '{}'\nexit 0\n",
            fwd(&marker),
            fwd(&got)
        ),
    )
    .unwrap();
    create(&env, "R9-F");
    let s = script(
        &env,
        "r9f.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [
                turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()]),
                turn(vec![write("src/Notes.txt", "second checkpoint\n"), implementation()]),
            ],
            "reviewer": [turn(vec![changes()]), turn(vec![approved()])],
        }),
    );
    let (_, err) = toryo(&env, &["run", "R9-F", "--yes", "--fake-agents", &s], &[]);
    let t = task(&env, "R9-F");
    assert_eq!(
        t.state,
        St::Completed,
        "Git must not see this forgery (else the gate never ran and this test proves nothing): {err}"
    );
    let tail = gate_tail(&env, "R9-F", "probe");
    let line = |k: &str| {
        tail.lines()
            .find_map(|l| l.trim().strip_prefix(k))
            .unwrap_or_else(|| panic!("{k}: {tail}"))
            .to_string()
    };
    let (orig, forged) = (line("orig="), line("forged="));
    assert_ne!(orig, forged, "the probe forged the file");
    // the gate ran on both verify runs, and both times read the committed bytes
    assert!(marker.exists(), "the environment gate ran");
    let hashes: Vec<String> = std::fs::read_to_string(&got)
        .unwrap()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(hashes.len(), 2, "one line per verify run: {hashes:?}");
    assert_eq!(hashes[1], orig, "the export holds the committed bytes");
    assert_ne!(hashes[1], forged);
    let d = deployments(&env);
    assert_eq!((d.len(), d[0].ok), (1, Some(true)));
}

/// M11-R13 AC-2 (Windows): a verify root that Low code can write — a folder under a
/// Low-labelled tree, like `AppData\LocalLow` — would make the export and the `envgate` folder
/// Low-writable. It is refused: at prepare (`verify_boundary_unavailable`, no gate ran), by
/// `doctor` (FAIL) and at `task create`.
#[cfg(windows)]
#[test]
fn a_low_writable_verify_root_is_refused() {
    let (env, marker) = probe_then_dbsync("exit 0\n", "");
    let cfg = std::fs::read_to_string(&env.config)
        .unwrap()
        .replace("boundary: none", "boundary: low_integrity");
    std::fs::write(&env.config, cfg).unwrap();
    // admitted while the root is an ordinary folder …
    create(&env, "R13-L");
    // … then the root gets a Low label (inherited by everything created under it)
    toryo::process::label_low(&[env.root.join("verify")]).unwrap();
    let s = script(
        &env,
        "r13l.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (_, err) = toryo(&env, &["run", "R13-L", "--yes", "--fake-agents", &s], &[]);
    let t = task(&env, "R13-L");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert_eq!(
        t.machine.reason_class(),
        Some("verify_boundary_unavailable")
    );
    assert!(
        err.contains("verify_boundary_unavailable")
            && err.contains("a Low-integrity process can create files in")
            && err.contains("verify.verify_root")
            && err.contains("toryo task config R13-L --reload"),
        "{err}"
    );
    assert!(!marker.exists(), "no gate ran");
    assert!(deployments(&env).is_empty());
    let base = env.root.join("verify").join("R13-L");
    assert!(
        !base.join("R13-L.verify").exists() && !base.join("deploy").exists(),
        "nothing was checked out or exported there"
    );
    // doctor says the same
    let o = command(&env, &["doctor"], &[]).output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    let line = out
        .lines()
        .find(|l| l.contains("a Low-integrity process can create files in"))
        .unwrap_or_else(|| panic!("{out}"));
    assert!(
        line.contains("FAIL") && line.contains("verify root"),
        "{line}"
    );
    // and a new task is not admitted
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "R13-M"],
        &[],
    );
    assert_ne!(code, 0);
    assert!(err.contains("verify_boundary_unavailable"), "{err}");
}

/// M11-R23 AC-1 (X16, Windows): the tree holds `Src/a.txt` next to the fixture's `src/…` — one
/// folder on NTFS. The export check folds names, so the environment gate runs and the
/// after-gate check is clean. (Before the fix no gate ever started for such a repository.)
#[cfg(windows)]
#[test]
fn an_environment_gate_runs_when_a_folder_is_spelled_two_ways() {
    let (env, marker) = probe_then_dbsync("exit 0\n", "exit 0\n");
    let source = env.root.join("cased-source.txt");
    std::fs::write(&source, "cased\n").unwrap();
    let blob = git(
        &env.repo,
        &["hash-object", "-w", &source.display().to_string()],
    );
    git(
        &env.repo,
        &[
            "-c",
            "core.ignorecase=false",
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("100644,{},Src/Cased.txt", blob.trim()),
        ],
    );
    git(
        &env.repo,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "a folder spelled two ways",
        ],
    );
    create(&env, "R23-C");
    let s = script(
        &env,
        "r23c.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (_, err) = toryo(&env, &["run", "R23-C", "--yes", "--fake-agents", &s], &[]);
    let t = task(&env, "R23-C");
    assert_eq!(t.state, St::Completed, "{err}");
    assert!(marker.exists(), "the environment gate ran");
    let d = deployments(&env);
    assert_eq!((d.len(), d[0].ok), (1, Some(true)));
}

/// M11-R20 AC-1 (X15): `.gitattributes` says `text eol=crlf` and the blobs still hold CRLF (never
/// renormalised): the export is accepted, the environment gate runs and the after-gate check is
/// clean. (Round 3 compared Git's view after its clean conversion and blocked every gate.)
#[test]
fn an_environment_gate_runs_when_the_blobs_keep_crlf_under_eol_crlf() {
    let (env, marker) = probe_then_dbsync("exit 0\n", "exit 0\n");
    // `.gitattributes` says `text eol=crlf`; the blob was committed with CRLF through plumbing
    // (`core.autocrlf=false`), as a Visual Studio XML file may be in a repository never
    // renormalised
    let source = env.root.join("crlf-source.xml");
    std::fs::write(&source, "<a>\r\n<b/>\r\n</a>\r\n").unwrap();
    std::fs::write(env.repo.join(".gitattributes"), "*.xml text eol=crlf\n").unwrap();
    let blob = git(
        &env.repo,
        &[
            "hash-object",
            "-w",
            "--no-filters",
            &source.display().to_string(),
        ],
    );
    let blob = blob.trim();
    git(
        &env.repo,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("100644,{blob},data/model.xml"),
        ],
    );
    git(&env.repo, &["add", ".gitattributes"]);
    git(
        &env.repo,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "xml with CRLF blobs",
        ],
    );
    create(&env, "R20-C");
    let s = script(
        &env,
        "r20c.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (_, err) = toryo(&env, &["run", "R20-C", "--yes", "--fake-agents", &s], &[]);
    let t = task(&env, "R20-C");
    assert_eq!(t.state, St::Completed, "{err}");
    assert!(marker.exists(), "the environment gate ran");
    // (whether the checkpoint keeps the CRLF blob or Git renormalises it at the first `add`
    // depends on racy-git timing in a repository this small; the unit test
    // `verify::tests::crlf_blobs_under_eol_crlf_export_clean` pins the case that blocked every
    // gate — this one runs the whole chain over such a repository)
    let (_, files) = seen_by_the_gate(&env);
    assert!(files.iter().any(|f| f == "data/model.xml"), "{files:?}");
    let d = deployments(&env);
    assert_eq!((d.len(), d[0].ok), (1, Some(true)));
}

/// M11-R13 AC-3 (R9 AC-7, run): with `verify.checkout: tracked` an environment gate runs and
/// passes — from the export of the commit, on a second verify run too (the checkout and its
/// build folder are kept between runs in this mode).
#[test]
fn an_environment_gate_runs_with_a_tracked_checkout() {
    let (env, marker) = probe_then_dbsync(
        "Set-Content -Path (Join-Path $env:TORYO_VERIFY_BUILD 'built.txt') -Value x\nexit 0\n",
        "echo x > \"$TORYO_VERIFY_BUILD/built.txt\"\nexit 0\n",
    );
    let cfg = std::fs::read_to_string(&env.config)
        .unwrap()
        .replace("boundary: none", "boundary: none, checkout: tracked");
    std::fs::write(&env.config, cfg).unwrap();
    create(&env, "R13-T");
    let s = script(
        &env,
        "r13t.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [
                turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()]),
                turn(vec![write("src/Notes.txt", "second checkpoint\n"), implementation()]),
            ],
            "reviewer": [turn(vec![changes()]), turn(vec![approved()])],
        }),
    );
    let (_, err) = toryo(&env, &["run", "R13-T", "--yes", "--fake-agents", &s], &[]);
    let t = task(&env, "R13-T");
    assert_eq!(t.state, St::Completed, "{err}");
    assert!(marker.exists(), "the environment gate ran");
    let vr = verify_result(&env, "R13-T");
    assert_eq!(vr["pass"], true);
    let gate = vr["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["kind"] == "environment")
        .unwrap()
        .clone();
    assert_eq!(
        (gate["exit"].clone(), gate["source"].clone()),
        (json!(0), json!("export"))
    );
    let (co, files) = seen_by_the_gate(&env);
    assert_eq!(co.file_name().unwrap(), "deploy");
    assert_eq!(
        files,
        checkpoint_files(&env, "R13-T"),
        "exactly the commit's files"
    );
    assert!(
        files.iter().any(|f| f == "src/Notes.txt"),
        "the second checkpoint"
    );
    let d = deployments(&env);
    assert_eq!((d.len(), d[0].ok), (1, Some(true)));
    assert_eq!(d[0].sha, t.machine.checkpoint().unwrap());
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
    // M11-R16 AC-1 (X12): the gate's needs were checked before the build — no check gate ran
    let ev = events(&env, "RS-E");
    assert!(
        !ev.iter().any(|e| e["type"] == "verify_command"),
        "no `verify_command` before the escalation: {ev:?}"
    );
    assert!(deployments(&env).is_empty());
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
    // M11-R16 AC-1: after the fix the check gate and the environment gate ran; AC-4: the flight
    // record calls the plain resume `resume_stage`, not `unpause`
    let ev = events(&env, "RS-E");
    let ran: Vec<&str> = ev
        .iter()
        .filter(|e| e["type"] == "verify_command")
        .map(|e| e["name"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(ran, ["divide-present", "dbsync"], "{ev:?}");
    assert!(
        ev.iter().any(|e| e["event"] == "resume_stage")
            && !ev.iter().any(|e| e["event"] == "unpause"),
        "{ev:?}"
    );
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

/// M11-R16 AC-2: a wrong `program` in the machine config. The gate's command is **frozen with
/// the task**, so correcting the file is not enough: a plain `resume` escalates again — before
/// any check gate — and its text names `task config --reload`. After the reload, `resume` runs
/// the gate. No fix cycle and no implementer turn is spent on the way.
#[test]
fn a_wrong_gate_program_needs_a_reload_before_resume() {
    let env = env_gate_setup(
        "codex-openai",
        "error",
        "limits:\n  max_gate_retries: 3\n  max_review_rounds: 3\n",
    );
    let base = std::fs::read_to_string(&env.machine).unwrap();
    let good = machine_gate(&env, "dbsync", "d365-env", "exit 0\n", "exit 0\n", "");
    let bad = {
        let program = good.lines().find(|l| l.contains("program:")).unwrap();
        let args = good.lines().find(|l| l.contains("args:")).unwrap();
        good.replace(
            program,
            if cfg!(windows) {
                "    program: 'C:/no/such/sync.exe'"
            } else {
                "    program: '/no/such/sync'"
            },
        )
        .replace(args, "    args: []")
    };
    std::fs::write(&env.machine, format!("{base}environment_gates:\n{bad}")).unwrap();
    create(&env, "R16-P");
    let s = script(
        &env,
        "r16p.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let counters = |env: &Env| {
        let t = task(env, "R16-P");
        let l = t.machine.split_limits().unwrap();
        (t.fix_cycle, l.gate_retries, l.review_rounds)
    };
    let (_, err) = toryo(&env, &["run", "R16-P", "--yes", "--fake-agents", &s], &[]);
    let t = task(&env, "R16-P");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert_eq!(
        t.machine.reason_class(),
        Some("environment_gate_unavailable")
    );
    let before = counters(&env);
    // the file is corrected — but the task still runs on its frozen configuration
    std::fs::write(&env.machine, format!("{base}environment_gates:\n{good}")).unwrap();
    let (_, err) = toryo(
        &env,
        &["resume", "R16-P", "--yes", "--fake-agents", &s],
        &[],
    );
    let t = task(&env, "R16-P");
    assert_eq!(t.state, St::Escalated, "the same escalation: {err}");
    assert_eq!(
        t.machine.reason_class(),
        Some("environment_gate_unavailable")
    );
    let why = t.machine.reason().unwrap_or_default().to_string();
    assert!(
        why.contains("program `")
            && why.contains("`toryo task config R16-P --reload`, then `toryo resume R16-P`"),
        "{why}"
    );
    assert!(
        status_out(&env, "R16-P").contains("`toryo task config R16-P --reload`"),
        "status says so too"
    );
    assert_eq!(counters(&env), before);
    assert!(
        !events(&env, "R16-P")
            .iter()
            .any(|e| e["type"] == "verify_command"),
        "still no check gate ran"
    );
    // reload, then resume: the gate runs
    let (code, err) = toryo(&env, &["task", "config", "R16-P", "--reload"], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "R16-P").state, St::Escalated);
    let (code, err) = toryo(
        &env,
        &["resume", "R16-P", "--yes", "--fake-agents", &s],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "R16-P");
    assert_eq!(t.state, St::Completed, "{err}");
    assert_eq!(counters(&env), before, "no cycle was spent");
    let d = deployments(&env);
    assert_eq!((d.len(), d[0].ok), (1, Some(true)));
    assert_eq!(
        calls(&env, "R16-P")
            .iter()
            .filter(|c| c["key"]
                .as_str()
                .is_some_and(|k| k.starts_with("implementer")))
            .count(),
        0,
        "no implementer turn during the last resume"
    );
}

/// M11-R16 AC-3: the escalation and its class are one transaction. A process that dies right
/// after it has escalated leaves a task whose plain `resume` works — there is no window in
/// which the reason says `toryo resume` while the class that makes it work is missing.
#[test]
fn a_crash_right_after_a_machine_caused_escalation_keeps_its_class() {
    let env = env_gate_setup("codex-openai", "error", "");
    define_gates(
        &env,
        &machine_gate(
            &env,
            "dbsync",
            "d365-env",
            "exit 0\n",
            "exit 0\n",
            "    secret_env: { K: 'env:TORYO_M11_R16_SECRET' }\n",
        ),
    );
    create(&env, "R16-C");
    let s = script(
        &env,
        "r16c.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(
        &env,
        &["run", "R16-C", "--yes", "--fake-agents", &s],
        &[("TORYO_TEST_CRASH_AT", "escalate:after")],
    );
    assert_eq!(code, 86, "crashed at the crash point: {err}");
    let t = task(&env, "R16-C");
    assert_eq!(t.state, St::Escalated);
    assert_eq!(
        t.machine.reason_class(),
        Some("environment_gate_unavailable"),
        "written with the escalation"
    );
    let (code, err) = toryo(
        &env,
        &["resume", "R16-C", "--yes", "--fake-agents", &s],
        &[("TORYO_M11_R16_SECRET", "ok")],
    );
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "R16-C");
    assert_eq!(t.state, St::Completed, "{err}");
    assert_eq!(t.fix_cycle, 0);
}

/// M11-R17 (X8): plain resume after `verify_boundary_unavailable` (Windows: the Low label cannot
/// be set — the injected error of a folder without WRITE_OWNER). Once the cause is gone a plain
/// `toryo resume` goes back into verify: no fix cycle, no implementer turn.
#[cfg(windows)]
#[test]
fn plain_resume_after_an_unavailable_boundary_costs_no_fix_cycle() {
    let (env, marker) = probe_then_dbsync("exit 0\n", "");
    let cfg = std::fs::read_to_string(&env.config)
        .unwrap()
        .replace("boundary: none", "boundary: low_integrity");
    std::fs::write(&env.config, cfg).unwrap();
    create(&env, "R17-B");
    let s = script(
        &env,
        "r17b.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (_, err) = toryo(
        &env,
        &["run", "R17-B", "--yes", "--fake-agents", &s],
        &[("TORYO_TEST_LABEL_ERROR", "5")],
    );
    let t = task(&env, "R17-B");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert_eq!(
        t.machine.reason_class(),
        Some("verify_boundary_unavailable")
    );
    let why = t.machine.reason().unwrap_or_default().to_string();
    assert!(
        why.starts_with("verify_boundary_unavailable: ")
            && why.contains("WRITE_OWNER")
            && why.contains("`toryo resume R17-B`")
            && why.contains("`toryo task config R17-B --reload`"),
        "{why}"
    );
    assert_eq!(t.fix_cycle, 0);
    assert!(!marker.exists());
    let (code, err) = toryo(
        &env,
        &["resume", "R17-B", "--yes", "--fake-agents", &s],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "R17-B");
    assert_eq!(t.state, St::Completed, "{err}");
    assert_eq!(t.fix_cycle, 0, "no fix cycle was spent");
    assert!(t.machine.reason_class().is_none());
    assert!(marker.exists(), "the gates ran");
    assert_eq!(
        calls(&env, "R17-B")
            .iter()
            .filter(|c| c["key"]
                .as_str()
                .is_some_and(|k| k.starts_with("implementer")))
            .count(),
        0
    );
}

/// M11-R17 (R12 item 2, end to end): a task frozen before M11-R3 still holds `%X%` in its
/// environment gate's arguments (they are expanded at load now). The gate does not start —
/// found before any check gate, no deployment row, no fix cycle — and the reason says to
/// reload; after `task config --reload` the gate runs.
#[test]
fn a_gate_frozen_with_a_variable_does_not_start() {
    let env = env_gate_setup("codex-openai", "error", "");
    define_gates(
        &env,
        &machine_gate(&env, "dbsync", "d365-env", "exit 0\n", "exit 0\n", ""),
    );
    create(&env, "R17-V");
    // forge what an older binary froze: the script path written with a variable. The frozen
    // configuration is the version whose SHA-256 the task row names; a run store from before
    // the immutable versions has only the view, which is used when its hash matches
    let t = task(&env, "R17-V");
    let (_, text) = toryo::engine::task::frozen_config_text(&t).unwrap();
    let gates = fwd(&env.root.join("gates"));
    assert!(text.contains(&gates), "{text}");
    let old = text.replace(&gates, "%TORYO_R17_GATES%");
    std::fs::write(t.artifacts().join("config.json"), &old).unwrap();
    let sha = toryo::engine::task::sha256_hex(old.as_bytes());
    let (i, p, im, rv, idx) = (
        t.id.clone(),
        t.planner.clone(),
        t.implementers.clone(),
        t.reviewer.clone(),
        t.implementer_index,
    );
    db(&env)
        .call(move |tx| store::set_config(tx, &i, &sha, &p, &im, &rv, idx))
        .unwrap();
    let s = script(
        &env,
        "r17v.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    // the variable is even set: the frozen text is never expanded at run time
    let gates_var = [("TORYO_R17_GATES", gates.as_str())];
    let (_, err) = toryo(
        &env,
        &["run", "R17-V", "--yes", "--fake-agents", &s],
        &gates_var,
    );
    let t = task(&env, "R17-V");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert_eq!(
        t.machine.reason_class(),
        Some("environment_gate_unavailable")
    );
    let why = t.machine.reason().unwrap_or_default().to_string();
    assert!(
        why.contains("environment gate `dbsync`: the command still contains `%TORYO_R17_GATES%`")
            && why.contains("run `toryo task config R17-V --reload`, then `toryo resume R17-V`"),
        "{why}"
    );
    assert_eq!(t.fix_cycle, 0);
    assert!(deployments(&env).is_empty(), "no deployment row");
    assert!(
        !events(&env, "R17-V")
            .iter()
            .any(|e| e["type"] == "verify_command"),
        "no gate ran"
    );
    // the way out the reason names
    let (code, err) = toryo(&env, &["task", "config", "R17-V", "--reload"], &[]);
    assert_eq!(code, 0, "{err}");
    let (code, err) = toryo(
        &env,
        &["resume", "R17-V", "--yes", "--fake-agents", &s],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "R17-V");
    assert_eq!(t.state, St::Completed, "{err}");
    assert_eq!(t.fix_cycle, 0);
    let d = deployments(&env);
    assert_eq!((d.len(), d[0].ok), (1, Some(true)));
}

/// M12-05 AC-3: an environment gate whose `-File` script is not there. `doctor` FAILs and
/// `task create` refuses it (like a gate the machine does not define); when the script
/// disappears after `task create`, Verify stops with `environment_gate_unavailable` — no
/// implementer turn, no gate retry used, no deployment. (Was: PowerShell starts, exits 2, and
/// that counts as a failing gate: a typo in the machine config cost two implementer turns.)
#[test]
fn m12_05_an_environment_gate_with_a_missing_script_is_a_machine_stop() {
    let env = env_gate_setup("codex-openai", "error", "");
    let g = machine_gate(&env, "dbsync", "d365-env", "exit 0\n", "exit 0\n", "");
    define_gates(&env, &g);
    let script_file = env.root.join("gates").join(if cfg!(windows) {
        "dbsync.ps1"
    } else {
        "dbsync.sh"
    });
    assert!(script_file.is_file());
    // the script is there: doctor is quiet, `task create` accepts
    let o = command(&env, &["doctor"], &[]).output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(
        out.lines()
            .any(|l| l.contains("environment gate `dbsync` can start here")),
        "{out}"
    );
    create(&env, "MS-1");
    // … and gone: doctor FAILs, a new task is refused
    std::fs::remove_file(&script_file).unwrap();
    let o = command(&env, &["doctor"], &[]).output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    let line = out
        .lines()
        .find(|l| l.contains("environment gate `dbsync`") && l.contains("FAIL"))
        .unwrap_or_else(|| panic!("{out}"));
    assert!(line.contains("does not exist"), "{line}");
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "MS-2"],
        &[],
    );
    assert_ne!(code, 0);
    assert!(
        err.contains("environment_gate_unavailable: environment gate `dbsync` cannot start")
            && err.contains("does not exist"),
        "{err}"
    );
    // the task made before: Verify stops, nothing was spent on it
    let s = script(
        &env,
        "ms1.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
        }),
    );
    let (_, err) = toryo(&env, &["run", "MS-1", "--yes", "--fake-agents", &s], &[]);
    let t = task(&env, "MS-1");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert_eq!(
        t.machine.reason_class(),
        Some("environment_gate_unavailable"),
        "{err}"
    );
    let why = t.machine.reason().unwrap().to_string();
    assert!(why.contains("does not exist"), "{why}");
    assert_eq!(
        attempts(&env, "MS-1")
            .iter()
            .filter(|a| a.stage == "implement")
            .count(),
        1,
        "no implementer turn"
    );
    assert_eq!(t.fix_cycle, 0, "no gate retry used");
    assert!(deployments(&env).is_empty());
}

/// M12-06 AC-1/AC-2 (X18): an environment gate that runs into its timeout is the owner's
/// problem, not the implementer's. The task stops with `environment_gate_timeout` (machine-
/// caused): no implementer turn, no gate retry used, the deployment record says interrupted,
/// the reason says the limit and "the environment may be half-changed". A plain `resume` runs
/// Verify — and the gate — again (a second run of the script is quick). Was: a failing gate,
/// an implementer turn, a retry, and the sync started again unattended.
#[test]
fn m12_06_an_environment_gate_that_times_out_stops_for_the_owner() {
    let env = env_gate_setup_timeout("codex-openai", "error", "", "3s");
    let run_log = fwd(&env.root.join("gates").join("runs"));
    // the first run sleeps past its limit, the second returns at once
    let ps1 = format!(
        "Add-Content -Path '{run_log}' -Value x\nif ((Get-Content '{run_log}').Count -le 1) {{ Start-Sleep -Seconds 60 }}\n'synced'\nexit 0\n"
    );
    let sh = format!(
        "echo x >> '{run_log}'\nn=$(wc -l < '{run_log}')\nif [ \"$n\" -le 1 ]; then sleep 60; fi\necho synced\nexit 0\n"
    );
    define_gates(
        &env,
        &machine_gate(&env, "dbsync", "d365-env", &ps1, &sh, ""),
    );
    create(&env, "TO-1");
    let s = script(
        &env,
        "to1.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (_, err) = toryo(&env, &["run", "TO-1", "--yes", "--fake-agents", &s], &[]);
    let t = task(&env, "TO-1");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert_eq!(
        t.machine.reason_class(),
        Some("environment_gate_timeout"),
        "{err}"
    );
    let why = t.machine.reason().unwrap().to_string();
    assert!(
        why.contains("`dbsync` ran into its timeout (3 s)")
            && why.contains("half-changed")
            && why.contains("`toryo resume TO-1`"),
        "{why}"
    );
    assert_eq!(
        attempts(&env, "TO-1")
            .iter()
            .filter(|a| a.stage == "implement")
            .count(),
        1,
        "no implementer turn"
    );
    assert_eq!(t.fix_cycle, 0, "no gate retry used");
    let d = deployments(&env);
    assert_eq!((d.len(), d[0].ok), (1, None), "interrupted: {d:?}");
    // a plain resume runs Verify and the gate again
    let s2 = script(
        &env,
        "to1b.json",
        json!({ "reviewer": [turn(vec![approved()])] }),
    );
    let (code, err) = toryo(
        &env,
        &["resume", "TO-1", "--yes", "--fake-agents", &s2],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "TO-1");
    assert_eq!(t.state, St::Completed, "{err}");
    assert_eq!(t.fix_cycle, 0, "a machine stop costs no fix cycle");
    // (the deployment table keeps one row per lock: it now says the second run passed)
    let runs = std::fs::read_to_string(env.root.join("gates").join("runs")).unwrap();
    assert_eq!(runs.lines().count(), 2, "the gate ran again");
    assert_eq!(deployments(&env)[0].ok, Some(true));
}

/// M12-06 AC-3: a **check** gate that runs into its timeout is still a gate failure — the
/// implementer is sent back with feedback that says "timed out after 3 s" (the limit it was
/// given), and the fix cycle is counted.
#[test]
fn m12_06_a_check_gate_timeout_is_still_a_gate_failure_and_says_so() {
    let (env, marker) = probe_then_dbsync_timed(
        "Start-Sleep -Seconds 60\nexit 0\n",
        "sleep 60\nexit 0\n",
        "3s",
    );
    create(&env, "TO-2");
    let s = script(
        &env,
        "to2.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [
                turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()]),
                turn(vec![implementation()]),
                turn(vec![implementation()]),
            ],
        }),
    );
    let (_, err) = toryo(&env, &["run", "TO-2", "--yes", "--fake-agents", &s], &[]);
    let t = task(&env, "TO-2");
    assert!(
        t.fix_cycle >= 1,
        "a gate failure: the implementer is sent back: {err}"
    );
    assert!(
        !marker.exists(),
        "no environment gate after a failing check gate"
    );
    let c = calls(&env, "TO-2");
    let second = c
        .iter()
        .filter(|x| x["key"] == "implementer")
        .nth(1)
        .unwrap_or_else(|| panic!("a second implementer turn: {err}"));
    let prompt = second["prompt"].as_str().unwrap();
    assert!(prompt.contains("`probe` timed out after 3 s"), "{prompt}");
    assert_ne!(
        t.machine.reason_class(),
        Some("environment_gate_timeout"),
        "a check gate is not the owner's problem"
    );
}

// ── M12-07: confirm before deploy (X17) ──────────────────────────────────────────────────

/// The `probe_then_dbsync` fixture with `confirm: <mode>` on the machine's `dbsync`. The check
/// gate `probe` logs each run to the returned file, so a test can tell that none ran.
fn confirm_fixture(mode: &str) -> (Env, PathBuf, PathBuf) {
    let log_name = "probe-runs";
    let (env, marker) = {
        // the probe script needs the log path, which needs the env: write it afterwards
        let (env, marker) = probe_then_dbsync("exit 0\n", "exit 0\n");
        (env, marker)
    };
    let log = env.root.join("gates").join(log_name);
    let l = fwd(&log);
    // the probe script is committed in the repository: add the log line and commit it
    let (file, body) = if cfg!(windows) {
        (
            "scripts/probe.ps1",
            format!("Add-Content -Path '{l}' -Value x\nexit 0\n"),
        )
    } else {
        ("scripts/probe.sh", format!("echo x >> '{l}'\nexit 0\n"))
    };
    commit_repo(&env, &[(file, &body)], "the probe logs its runs");
    let m = std::fs::read_to_string(&env.machine).unwrap();
    assert!(m.contains("    lock: d365-env\n"), "{m}");
    std::fs::write(
        &env.machine,
        m.replace(
            "    lock: d365-env\n",
            &format!("    lock: d365-env\n    confirm: {mode}\n"),
        ),
    )
    .unwrap();
    (env, marker, log)
}

fn confirm_stops(env: &Env, id: &str) -> usize {
    events(env, id)
        .iter()
        .filter(|e| e["type"] == "escalated" && e["reason_class"] == "environment_gate_confirm")
        .count()
}

/// AC-1/AC-2/AC-4, `first`: the first Verify stops **before any gate ran** (no check gate, no
/// deploy); the reason has the gate, the lock, the commit, the `git diff --stat` lines, the
/// command and "then `toryo resume`"; `run --yes` does not skip the stop; a plain `resume`
/// confirms, costs no fix cycle, and the gates run.
#[test]
fn m12_07_confirm_first_stops_before_any_gate_and_resume_is_the_yes() {
    let (env, marker, probe) = confirm_fixture("first");
    create(&env, "CF-1");
    let s = script(
        &env,
        "cf1.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (_, err) = toryo(&env, &["run", "CF-1", "--yes", "--fake-agents", &s], &[]);
    let t = task(&env, "CF-1");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert_eq!(
        t.machine.reason_class(),
        Some("environment_gate_confirm"),
        "{err}"
    );
    let why = t.machine.reason().unwrap().to_string();
    let head = t.machine.checkpoint().map(str::to_string);
    assert!(
        why.contains("gate `dbsync` (lock d365-env, confirm: first)")
            && why.contains("src/Calculator.psm1")
            && why.contains("`toryo resume CF-1` allows the deploy")
            && why.contains("show --stat"),
        "{why}"
    );
    let _ = head;
    assert!(!probe.exists(), "no check gate ran");
    assert!(
        !marker.exists() && deployments(&env).is_empty(),
        "nothing deployed"
    );
    assert_eq!(
        attempts(&env, "CF-1")
            .iter()
            .filter(|a| a.stage == "implement")
            .count(),
        1
    );
    // `--yes` is no confirmation: `run --yes` finds the task still waiting
    let (_, err) = toryo(&env, &["run", "CF-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(task(&env, "CF-1").state, St::Escalated, "{err}");
    assert!(!probe.exists() && !marker.exists());
    // the plain resume is the yes
    let s2 = script(
        &env,
        "cf1b.json",
        json!({ "reviewer": [turn(vec![approved()])] }),
    );
    // … but not `resume --yes` (it is for the gates, never a deploy), and not a resume of
    // everything (no id)
    let (code, err) = toryo(
        &env,
        &["resume", "CF-1", "--yes", "--fake-agents", &s2],
        &[],
    );
    assert_ne!(code, 0, "{err}");
    assert!(err.contains("--yes never confirms a deploy"), "{err}");
    assert_eq!(task(&env, "CF-1").state, St::Escalated);
    let (_, err) = toryo(&env, &["resume", "--fake-agents", &s2], &[]);
    assert!(
        err.contains("CF-1: waits for your yes before it deploys"),
        "{err}"
    );
    assert_eq!(task(&env, "CF-1").state, St::Escalated);
    assert!(!probe.exists() && !marker.exists());
    let (_, err) = toryo(&env, &["resume", "CF-1", "--fake-agents", &s2], &[]);
    assert!(err.contains("deploy confirmed (dbsync"), "{err}");
    assert_eq!(task(&env, "CF-1").state, St::WaitingFinalApproval, "{err}");
    let (code, err) = toryo(&env, &["approve", "CF-1", "--gate", "final"], &[]);
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "CF-1");
    assert_eq!(t.state, St::Completed, "{err}");
    assert_eq!(t.fix_cycle, 0, "no fix cycle for the question");
    assert!(
        probe.exists() && marker.exists(),
        "the gates ran after the yes"
    );
    assert_eq!(confirm_stops(&env, "CF-1"), 1);
}

/// AC-2 (the second half): with `first` a later checkpoint of the same task does not stop;
/// AC-3: with `always` every new checkpoint stops once.
#[test]
fn m12_07_first_asks_once_per_task_and_always_once_per_checkpoint() {
    for (mode, stops) in [("first", 1), ("always", 2)] {
        let (env, _marker, probe) = confirm_fixture(mode);
        let id = format!("CM-{mode}");
        create(&env, &id);
        // the reviewer asks for changes once: a second checkpoint
        let s = script(
            &env,
            &format!("{id}.json"),
            json!({
                "planner": [turn(vec![plan()])],
                "implementer": [
                    turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()]),
                    turn(vec![write("src/Notes.txt", "second checkpoint\n"), implementation()]),
                ],
                "reviewer": [turn(vec![changes()]), turn(vec![approved()])],
            }),
        );
        let (_, err) = toryo(&env, &["run", &id, "--yes", "--fake-agents", &s], &[]);
        assert_eq!(task(&env, &id).state, St::Escalated, "{mode}: {err}");
        // each process starts its script from the top: what the rest of the run needs
        let s1 = script(
            &env,
            &format!("{id}-1.json"),
            json!({
                "implementer": [turn(vec![write("src/Notes.txt", "second checkpoint\n"), implementation()])],
                "reviewer": [turn(vec![changes()]), turn(vec![approved()])],
            }),
        );
        let s2 = script(
            &env,
            &format!("{id}-2.json"),
            json!({ "reviewer": [turn(vec![approved()])] }),
        );
        let (code, err) = toryo(&env, &["resume", &id, "--fake-agents", &s1], &[]);
        let mut err = err;
        if mode == "always" {
            // the second checkpoint stops again; one more yes
            assert_eq!(code, 1, "{mode}: {err}");
            assert_eq!(task(&env, &id).state, St::Escalated, "{mode}: {err}");
            let (_, e2) = toryo(&env, &["resume", &id, "--fake-agents", &s2], &[]);
            err = e2;
        }
        assert_eq!(
            task(&env, &id).state,
            St::WaitingFinalApproval,
            "{mode}: {err}"
        );
        let (code, e3) = toryo(&env, &["approve", &id, "--gate", "final"], &[]);
        assert_eq!(code, 0, "{mode}: {e3}");
        let t = task(&env, &id);
        assert_eq!(t.state, St::Completed, "{mode}: {err}");
        assert_eq!(confirm_stops(&env, &id), stops, "{mode}");
        let runs = std::fs::read_to_string(&probe).unwrap();
        assert_eq!(
            runs.lines().count(),
            2,
            "{mode}: both checkpoints were verified"
        );
    }
}

/// M12-15 (X23): every check gate and every environment gate gets the UI language of `tool: xpp`
/// (`DOTNET_CLI_UI_LANGUAGE`, `VSLANG`), replacing what the process inherited — except a name
/// the environment gate's own `env:` sets. (Was: MSBuild runs only.)
#[test]
fn m12_15_every_gate_gets_the_ui_language() {
    let probe_ps1 = "\"UI=$env:VSLANG/$env:DOTNET_CLI_UI_LANGUAGE\"\nexit 0\n";
    let probe_sh = "echo \"UI=$VSLANG/$DOTNET_CLI_UI_LANGUAGE\"\nexit 0\n";
    let (env, _marker) = probe_then_dbsync(probe_ps1, probe_sh);
    // the environment gate prints the same two values (its script replaced; the owner's `env:`
    // puts VSLANG to 1042 for it)
    let gate = env.root.join("gates").join(if cfg!(windows) {
        "dbsync.ps1"
    } else {
        "dbsync.sh"
    });
    std::fs::write(&gate, if cfg!(windows) { probe_ps1 } else { probe_sh }).unwrap();
    let m = std::fs::read_to_string(&env.machine).unwrap();
    assert!(m.contains("    lock: d365-env\n"), "{m}");
    std::fs::write(
        &env.machine,
        m.replace(
            "    lock: d365-env\n",
            "    lock: d365-env\n    env: { VSLANG: '1042' }\n",
        ),
    )
    .unwrap();
    create(&env, "UI-1");
    let s = script(
        &env,
        "ui1.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    // the owner's shell has a Japanese Visual Studio
    let (_, err) = toryo(
        &env,
        &["run", "UI-1", "--yes", "--fake-agents", &s],
        &[("VSLANG", "1041"), ("DOTNET_CLI_UI_LANGUAGE", "ja-JP")],
    );
    assert_eq!(task(&env, "UI-1").state, St::Completed, "{err}");
    let vr = verify_result(&env, "UI-1");
    let tail = |n: &str| {
        vr["commands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == n)
            .unwrap_or_else(|| panic!("{n} in {vr}"))["output_tail"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert!(tail("probe").contains("UI=1033/en-US"), "{}", tail("probe"));
    assert!(
        tail("dbsync").contains("UI=1042/en-US"),
        "{}",
        tail("dbsync")
    );
}

/// M12-16 AC-1: the whole output of an environment gate is in a log file of the run store, named
/// in `VERIFY_RESULT.json`, and it is redacted like the rest.
#[test]
fn m12_16_the_gate_log_is_redacted() {
    const SECRET: &str = "d365-sync-s3cr3t-88";
    let env = env_gate_setup("codex-openai", "error", "");
    define_gates(
        &env,
        &machine_gate(
            &env,
            "dbsync",
            "d365-env",
            "\"using $env:D365_SYNC_SECRET\"\nexit 0\n",
            "echo \"using $D365_SYNC_SECRET\"\nexit 0\n",
            "    secret_env: { D365_SYNC_SECRET: 'env:TORYO_M12_SYNC_SECRET' }\n",
        ),
    );
    create(&env, "LG-1");
    let s = script(
        &env,
        "lg1.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(
        &env,
        &["run", "LG-1", "--yes", "--fake-agents", &s],
        &[("TORYO_M12_SYNC_SECRET", SECRET)],
    );
    assert_eq!(code, 0, "{err}");
    let vr = verify_result(&env, "LG-1");
    let entry = &vr["commands"][1];
    assert_eq!(entry["name"], "dbsync");
    // M13-04: relative to the run store (was the absolute path)
    let rel = entry["log"].as_str().unwrap_or_else(|| panic!("{vr}"));
    assert!(rel.starts_with("gate-logs/"), "{rel}");
    let log = task(&env, "LG-1").run_store.join(rel);
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("using [REDACTED:D365_SYNC_SECRET]"), "{text}");
    assert!(!text.contains(SECRET), "{text}");
    // the check gate has its log too
    assert!(vr["commands"][0]["log"].as_str().is_some(), "{vr}");
}

// ── M13-05 (X26): the deploy confirmation is a database row ───────────────────────────────

/// AC-1: a `confirmations.json` in the run store that names the gate as confirmed — what any
/// process that can write the run store could leave there — confirms nothing. (Was: with
/// `confirm: first` only the gate name was compared, and the deploy ran unasked.)
#[test]
fn m13_05_a_confirmation_file_in_the_run_store_confirms_nothing() {
    let (env, marker, probe) = confirm_fixture("first");
    create(&env, "CD-1");
    let forged = task(&env, "CD-1").run_store.join("confirmations.json");
    std::fs::write(
        &forged,
        r#"{"confirmed":[{"gate":"dbsync","commit":"x"}],"pending":[{"gate":"dbsync","commit":"x"}]}"#,
    )
    .unwrap();
    let s = script(
        &env,
        "cd1.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (_, err) = toryo(&env, &["run", "CD-1", "--yes", "--fake-agents", &s], &[]);
    let t = task(&env, "CD-1");
    assert!(!marker.exists(), "deployed without the owner's yes: {err}");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert_eq!(
        t.machine.reason_class(),
        Some("environment_gate_confirm"),
        "{err}"
    );
    assert!(!probe.exists(), "no gate ran");
    // the yes is a row: the resume confirms, and the file is still not what decides
    let s2 = script(
        &env,
        "cd1b.json",
        json!({ "reviewer": [turn(vec![approved()])] }),
    );
    let (_, err) = toryo(&env, &["resume", "CD-1", "--fake-agents", &s2], &[]);
    assert!(err.contains("deploy confirmed (dbsync @ "), "{err}");
    assert_eq!(task(&env, "CD-1").state, St::WaitingFinalApproval, "{err}");
    assert!(marker.exists());
}

/// Send the task at its final gate into another fix cycle whose implementer writes `file`,
/// and drive it until it waits again.
fn another_checkpoint(env: &Env, id: &str, file: &str) -> String {
    let (code, err) = toryo(
        env,
        &["reject", id, "--gate", "final", "--reason", "one more"],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let s = script(
        env,
        &format!("{id}-{}.json", file.replace('/', "-")),
        json!({
            "implementer": [turn(vec![write(file, "more\n"), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (_, err) = toryo(env, &["run", id, "--fake-agents", &s], &[]);
    err
}

/// AC-2: the yes is bound to the task's frozen configuration. `task config --reload` with
/// nothing changed keeps it; a reload that changes the gate's command makes the next Verify
/// ask again, and the reason says why. (Was: keyed by the gate's name — the yes given for the
/// printed command was good for any later one.)
#[test]
fn m13_05_a_reload_that_changes_the_configuration_asks_again() {
    let (env, _marker, _probe) = confirm_fixture("first");
    create(&env, "CD-2");
    let s = script(
        &env,
        "cd2.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (_, err) = toryo(&env, &["run", "CD-2", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(task(&env, "CD-2").state, St::Escalated, "{err}");
    let s2 = script(
        &env,
        "cd2b.json",
        json!({ "reviewer": [turn(vec![approved()])] }),
    );
    let (_, err) = toryo(&env, &["resume", "CD-2", "--fake-agents", &s2], &[]);
    assert_eq!(task(&env, "CD-2").state, St::WaitingFinalApproval, "{err}");
    assert_eq!(confirm_stops(&env, "CD-2"), 1);
    // a reload that changes nothing: the next checkpoint is deployed without a question
    let (code, err) = toryo(&env, &["task", "config", "CD-2", "--reload"], &[]);
    assert_eq!(code, 0, "{err}");
    let err = another_checkpoint(&env, "CD-2", "src/Notes.txt");
    assert_eq!(task(&env, "CD-2").state, St::WaitingFinalApproval, "{err}");
    assert_eq!(confirm_stops(&env, "CD-2"), 1, "{err}");
    // the gate's command changes: one more argument
    let m = std::fs::read_to_string(&env.machine).unwrap();
    assert!(m.contains("']\n    lock: d365-env\n"), "{m}");
    std::fs::write(
        &env.machine,
        m.replace(
            "']\n    lock: d365-env\n",
            "', again]\n    lock: d365-env\n",
        ),
    )
    .unwrap();
    let digest = task(&env, "CD-2").config_sha256;
    let (code, err) = toryo(&env, &["task", "config", "CD-2", "--reload"], &[]);
    assert_eq!(code, 0, "{err}");
    assert_ne!(task(&env, "CD-2").config_sha256, digest, "{err}");
    let err = if task(&env, "CD-2").state == St::WaitingFinalApproval {
        another_checkpoint(&env, "CD-2", "src/More.txt")
    } else {
        // the reload withdrew the final gate: the checkpoint is verified again
        toryo(&env, &["run", "CD-2", "--fake-agents", &s2], &[]).1
    };
    let t = task(&env, "CD-2");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert_eq!(
        t.machine.reason_class(),
        Some("environment_gate_confirm"),
        "{err}"
    );
    let why = t.machine.reason().unwrap_or_default().to_string();
    assert!(
        why.contains("asked again: the task's configuration changed since your last yes")
            && why.contains("again"),
        "{why}"
    );
    assert_eq!(confirm_stops(&env, "CD-2"), 2);
}

// ── M13-08: one gate resolver; the timeout stop names every gate that runs again ──────────

/// One run of `id` with agents that would complete it.
fn run_to_the_gates(env: &Env, id: &str) -> String {
    let s = script(
        env,
        &format!("{id}.json"),
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    toryo(env, &["run", id, "--yes", "--fake-agents", &s], &[]).1
}

/// AC-1 (and item 2 of M13-14): a text file where the program of an **environment** gate
/// should be passes the preflight (it exists) and fails at the start — the same machine stop
/// as for a check gate (`m12_05_a_program_that_exists_but_cannot_start_is_the_same_stop`): no
/// implementer turn, no gate retry, and the deployment record says it did not run.
#[test]
fn m13_08_an_environment_gate_program_that_cannot_start_is_a_machine_stop() {
    let env = env_gate_setup("codex-openai", "error", "");
    let bad = env.root.join(if cfg!(windows) {
        "not-a-program.exe"
    } else {
        "not-a-program"
    });
    std::fs::write(&bad, "this is text, not a program\n").unwrap();
    define_gates(
        &env,
        &format!(
            "  dbsync:\n    program: '{}'\n    args: []\n    lock: d365-env\n",
            fwd(&bad)
        ),
    );
    create(&env, "SP-1");
    let err = run_to_the_gates(&env, "SP-1");
    let t = task(&env, "SP-1");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert_eq!(
        t.machine.reason_class(),
        Some("environment_gate_unavailable"),
        "{err}"
    );
    let why = t.machine.reason().unwrap();
    assert!(
        why.contains("could not be started") && why.contains("not-a-program"),
        "{why}"
    );
    assert_eq!(t.fix_cycle, 0, "a machine stop costs no fix cycle");
    assert_eq!(
        attempts(&env, "SP-1")
            .iter()
            .filter(|a| a.stage == "implement")
            .count(),
        1,
        "no implementer turn"
    );
    let d = deployments(&env);
    assert_eq!(
        (d.len(), d[0].ok),
        (1, Some(false)),
        "it did not run: {d:?}"
    );
}

/// AC-3: a gate that is killed by its timeout **after** it wrote into the export it deploys
/// from. Was: the export was looked at first — `verify_mutated_tree`, which only a
/// `resume --note` (a fix cycle) leaves. Now it is the timeout it is: a plain `resume`.
#[test]
fn m13_08_a_gate_killed_by_its_timeout_after_writing_into_the_export_is_a_timeout() {
    let env = env_gate_setup_timeout("codex-openai", "error", "", "8s");
    define_gates(
        &env,
        &machine_gate(
            &env,
            "dbsync",
            "d365-env",
            "Add-Content -Path (Join-Path $env:TORYO_VERIFY_CHECKOUT 'src/Calculator.psm1') -Value '# synced'\n'written'\nStart-Sleep -Seconds 120\nexit 0\n",
            "echo '# synced' >> \"$TORYO_VERIFY_CHECKOUT/src/Calculator.psm1\"\necho written\nsleep 120\nexit 0\n",
            "",
        ),
    );
    create(&env, "TO-3");
    let err = run_to_the_gates(&env, "TO-3");
    let t = task(&env, "TO-3");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert_eq!(
        t.machine.reason_class(),
        Some("environment_gate_timeout"),
        "{err}"
    );
    assert!(
        t.machine.reason().unwrap().contains("written"),
        "(the gate had written into the export before it was stopped): {err}"
    );
    assert_eq!(t.fix_cycle, 0);
}

/// AC-4: two environment gates, the second runs into its timeout. A `resume` runs Verify from
/// the start — the first gate deploys again. Was: the reason said "the gate" runs again. Now it
/// names both.
#[test]
fn m13_08_a_timeout_names_every_environment_gate_that_runs_again() {
    let org = ORGCHART.replace("IMPLEMENTER", "codex-openai").replacen(
        "gates:\n",
        "gates:\n  seed: { kind: environment, timeout: 2m, if_missing: error }\n  dbsync: { kind: environment, timeout: 4s, if_missing: error }\n",
        1,
    );
    let env = setup_orgchart(
        "codex-openai",
        "",
        &[
            (".toryo/toryo-orgchart.yaml", &org),
            (
                ".toryo/roles/implementer/toryo-role.yaml",
                "toryo: 1\npurpose: Change the code.\nbefore_handoff: [divide-present, seed, dbsync]\n",
            ),
        ],
    );
    let seed = machine_gate(
        &env,
        "seed",
        "seed-env",
        "'seeded'\nexit 0\n",
        "echo seeded\nexit 0\n",
        "",
    );
    let sync = machine_gate(
        &env,
        "dbsync",
        "d365-env",
        "Start-Sleep -Seconds 120\nexit 0\n",
        "sleep 120\nexit 0\n",
        "",
    );
    define_gates(&env, &format!("{seed}{sync}"));
    create(&env, "TO-4");
    let err = run_to_the_gates(&env, "TO-4");
    let t = task(&env, "TO-4");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert_eq!(
        t.machine.reason_class(),
        Some("environment_gate_timeout"),
        "{err}"
    );
    let why = t.machine.reason().unwrap().to_string();
    assert!(
        why.contains("`dbsync` ran into its timeout (4 s)")
            && why.contains(
                "`toryo resume TO-4` runs Verify and every environment gate again: `seed`"
            )
            && why.contains("deploys again), then `dbsync`"),
        "{why}"
    );
    let d = deployments(&env);
    let of = |gate: &str| d.iter().find(|d| d.gate == gate).map(|d| d.ok);
    assert_eq!(
        (of("seed"), of("dbsync")),
        (Some(Some(true)), Some(None)),
        "{d:?}"
    );
}

// ── M13-09: `environment_gate_confirm` says what to type, and how to say no ───────────────

/// AC-2: `doctor` prints `confirm:` on every environment-gate line — also on a FAIL — and
/// says what really happens: only a missing **script** is refused by `task create`; a missing
/// program is admitted and stops the run at Verify. (Was: `confirm:` on OK lines only, and
/// "`task create` refuses it" for every problem.)
#[test]
fn m13_09_doctor_says_confirm_on_every_environment_gate_line() {
    let env = confirm_fixture("first").0;
    let lines = |env: &Env| -> Vec<String> {
        let o = command(env, &["doctor"], &[]).output().unwrap();
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter(|l| l.contains("environment gate `dbsync`"))
            .map(str::to_string)
            .collect()
    };
    let ok = lines(&env);
    assert!(
        ok.len() == 1 && ok[0].contains("[OK  ]") && ok[0].contains("confirm: first"),
        "{ok:?}"
    );
    // the program is not on this machine
    let m = std::fs::read_to_string(&env.machine).unwrap();
    let program = if cfg!(windows) {
        "program: pwsh"
    } else {
        "program: sh"
    };
    assert!(m.contains(program), "{m}");
    std::fs::write(
        &env.machine,
        m.replace(program, "program: no-such-program-r9"),
    )
    .unwrap();
    let bad = lines(&env);
    assert!(
        bad.len() == 1
            && bad[0].contains("[FAIL]")
            && bad[0].contains("no-such-program-r9")
            && bad[0].contains("confirm: first"),
        "{bad:?}"
    );
    assert!(
        !bad[0].contains("refuses") && bad[0].contains("environment_gate_unavailable"),
        "a missing program is not refused by `task create`: {bad:?}"
    );
    // the script is not there: this one `task create` does refuse
    std::fs::write(&env.machine, &m).unwrap();
    let script_file = env.root.join("gates").join(if cfg!(windows) {
        "dbsync.ps1"
    } else {
        "dbsync.sh"
    });
    std::fs::remove_file(&script_file).unwrap();
    let bad = lines(&env);
    assert!(
        bad.len() == 1
            && bad[0].contains("[FAIL]")
            && bad[0].contains("`task create` refuses it")
            && bad[0].contains("confirm: first"),
        "{bad:?}"
    );
}

/// AC-3: the README and the guide name the right class for a gate that cannot start, and say
/// how to say **no** to a deploy.
#[test]
fn m13_09_the_readme_and_the_guide_say_which_class_and_how_to_say_no() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let readme = std::fs::read_to_string(root.join("README.md")).unwrap();
    let guide = std::fs::read_to_string(root.join("docs").join("HUONG-DAN.md")).unwrap();
    let flat = readme.replace("\r\n", "\n").replace("\n  ", " ");
    // README: an environment gate is not under `gate_unavailable`
    assert!(
        !flat.contains("(missing program or script) → the task stops with `gate_unavailable`"),
        "the old sentence"
    );
    assert!(
        flat.contains("a **check gate** whose program is not on the machine → `gate_unavailable`")
            && flat.contains("→ `environment_gate_unavailable`"),
        "README: which class"
    );
    assert!(
        flat.contains("To say no: `toryo resume <ID> --note \"…\"`")
            && flat.contains("`toryo task cleanup <ID>`"),
        "README: how to say no"
    );
    // guide §9.1: `gate_unavailable` does not claim a check gate's `-File` is checked
    let row = |class: &str| -> &str {
        guide
            .lines()
            .find(|l| l.starts_with(&format!("| `{class}` |")))
            .unwrap_or_else(|| panic!("no row for {class}"))
    };
    assert!(
        !row("gate_unavailable").contains("script `-File` không có)"),
        "{}",
        row("gate_unavailable")
    );
    let confirm = row("environment_gate_confirm");
    assert!(
        confirm.contains("--note") && confirm.contains("task cleanup"),
        "the guide: how to say no — {confirm}"
    );
}

// ── M13-14 (3): `task config --reload` refuses a missing environment-gate script ──────────

/// The refusal of `task create` (M12-05) holds for a reload too: with the gate's script gone,
/// `task config --reload` stops with `environment_gate_unavailable` and changes nothing.
#[test]
fn m13_14_a_reload_refuses_a_missing_environment_gate_script() {
    let env = env_gate_setup("codex-openai", "error", "");
    define_gates(
        &env,
        &machine_gate(&env, "dbsync", "d365-env", "exit 0\n", "exit 0\n", ""),
    );
    create(&env, "RL-1");
    // a waiting state: the plan gate (no `--yes`)
    let s = script(&env, "rl1.json", json!({ "planner": [turn(vec![plan()])] }));
    let (_, err) = toryo(&env, &["run", "RL-1", "--fake-agents", &s], &[]);
    let before = task(&env, "RL-1");
    assert_eq!(before.state, St::WaitingPlanApproval, "{err}");
    std::fs::remove_file(env.root.join("gates").join(if cfg!(windows) {
        "dbsync.ps1"
    } else {
        "dbsync.sh"
    }))
    .unwrap();
    let (code, err) = toryo(&env, &["task", "config", "RL-1", "--reload"], &[]);
    assert_ne!(code, 0, "{err}");
    assert!(
        err.contains(
            "environment_gate_unavailable: environment gate `dbsync` cannot start on this machine"
        ) && err.contains("does not exist"),
        "{err}"
    );
    let after = task(&env, "RL-1");
    assert_eq!(
        after.config_sha256, before.config_sha256,
        "nothing was changed"
    );
    assert_eq!(after.state, St::WaitingPlanApproval);
}
