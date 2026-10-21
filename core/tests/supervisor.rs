//! Supervisor tests against a deliberately misbehaving worker. Windows only.
#![cfg(windows)]

use std::time::{Duration, Instant};

use flick_core::supervisor::{run, CancelToken, Limits, SupervisorError, Worker};
use serde_json::{json, Value};
use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};

fn misbehave(mode: &str) -> Worker {
    Worker {
        exe: env!("CARGO_BIN_EXE_flick-misbehave").into(),
        args: vec![mode.into()],
    }
}

fn limits(memory_mb: usize, seconds: u64) -> Limits {
    Limits {
        memory_mb,
        cpu_percent: 50,
        time: Duration::from_secs(seconds),
    }
}

fn run_mode(worker: &Worker, limits: &Limits) -> Result<Value, SupervisorError> {
    run(
        worker,
        &json!({ "op": "test" }),
        limits,
        &CancelToken::default(),
    )
}

#[test]
fn well_behaved_worker_round_trips_json() {
    let response = run_mode(&misbehave("echo"), &limits(256, 30)).unwrap();
    assert_eq!(response, json!({ "ok": true, "echo": { "op": "test" } }));
}

#[test]
fn memory_bomb_is_stopped_at_the_cap() {
    let started = Instant::now();
    let err = run_mode(&misbehave("memory-bomb"), &limits(128, 60)).unwrap_err();
    assert!(matches!(err, SupervisorError::TooHeavy(128)), "{err:?}");
    assert!(err.to_string().contains("too heavy for the safe limit"));
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn infinite_loop_is_killed_at_the_deadline() {
    let started = Instant::now();
    let err = run_mode(&misbehave("spin"), &limits(64, 2)).unwrap_err();
    assert!(matches!(err, SupervisorError::TimedOut(2)), "{err:?}");
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_secs(2) && elapsed < Duration::from_secs(12));
}

#[test]
fn child_processes_die_with_the_job() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("child.pid");
    let mut worker = misbehave("spawn-spin");
    worker.args.push(pid_file.clone().into());

    let err = run_mode(&worker, &limits(128, 2)).unwrap_err();
    assert!(matches!(err, SupervisorError::TimedOut(_)), "{err:?}");

    let pid: u32 = std::fs::read_to_string(&pid_file).unwrap().parse().unwrap();
    unsafe {
        // Either the process is already gone, or its handle is signalled (exited).
        if let Ok(handle) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            let state = WaitForSingleObject(handle, 2000);
            let _ = CloseHandle(handle);
            assert_eq!(state, WAIT_OBJECT_0, "grandchild {pid} survived the job");
        }
    }
}

#[test]
fn cancel_stops_a_running_job() {
    let cancel = CancelToken::default();
    let trigger = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        trigger.cancel();
    });
    let started = Instant::now();
    let err =
        run::<_, Value>(&misbehave("spin"), &json!({}), &limits(64, 60), &cancel).unwrap_err();
    assert!(matches!(err, SupervisorError::Cancelled), "{err:?}");
    // Generous bound: a cold start can include an antivirus scan of the test exe.
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn crash_and_garbage_are_reported() {
    let err = run_mode(&misbehave("crash"), &limits(64, 30)).unwrap_err();
    match err {
        SupervisorError::Crashed { code, stderr } => {
            assert_eq!(code, 3);
            assert_eq!(stderr, "simulated crash");
        }
        other => panic!("{other:?}"),
    }

    let err = run_mode(&misbehave("garbage"), &limits(64, 30)).unwrap_err();
    assert!(matches!(err, SupervisorError::BadResponse(_)), "{err:?}");
}

#[test]
fn missing_worker_is_a_spawn_error() {
    let worker = Worker {
        exe: "does-not-exist-flick-worker.exe".into(),
        args: vec![],
    };
    let err = run_mode(&worker, &limits(64, 5)).unwrap_err();
    assert!(matches!(err, SupervisorError::Spawn(_)), "{err:?}");
}
