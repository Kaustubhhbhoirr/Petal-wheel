//! Runs worker processes inside Windows Job Objects so a bad file can never hang the PC.
//!
//! Each job gets its own Job Object with a memory cap, below-normal priority, a hard CPU rate
//! cap and `KILL_ON_JOB_CLOSE`, so nothing outlives the supervisor. The wall-clock limit and
//! cancellation are enforced here by terminating the whole job, which also kills any child
//! processes a worker started.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::Serialize;
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectAssociateCompletionPortInformation,
    JobObjectCpuRateControlInformation, JobObjectExtendedLimitInformation, SetInformationJobObject,
    TerminateJobObject, JOBOBJECTINFOCLASS, JOBOBJECT_ASSOCIATE_COMPLETION_PORT,
    JOBOBJECT_CPU_RATE_CONTROL_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_CPU_RATE_CONTROL_ENABLE, JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP,
    JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_JOB_MEMORY,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PRIORITY_CLASS,
};
use windows::Win32::System::SystemServices::{
    JOB_OBJECT_MSG_ACTIVE_PROCESS_ZERO, JOB_OBJECT_MSG_JOB_MEMORY_LIMIT,
    JOB_OBJECT_MSG_PROCESS_MEMORY_LIMIT,
};
use windows::Win32::System::Threading::{
    OpenThread, ResumeThread, BELOW_NORMAL_PRIORITY_CLASS, CREATE_NO_WINDOW, CREATE_SUSPENDED,
    THREAD_SUSPEND_RESUME,
};
use windows::Win32::System::IO::{CreateIoCompletionPort, GetQueuedCompletionStatus, OVERLAPPED};

/// Workers answer with one small JSON document. Anything bigger is a bug, not a response.
const MAX_RESPONSE_BYTES: usize = 1 << 20;
/// Only the end of stderr is kept, for error reports.
const MAX_STDERR_BYTES: usize = 8 << 10;
/// How often the supervisor wakes up to check for cancellation while a job runs.
const POLL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
pub struct Limits {
    pub memory_mb: usize,
    /// Hard cap on the share of total machine CPU, 1 to 100.
    pub cpu_percent: u32,
    pub time: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            memory_mb: 512,
            cpu_percent: 50,
            time: Duration::from_secs(120),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Worker {
    pub exe: PathBuf,
    pub args: Vec<OsString>,
}

/// Set to cancel a running job. Cloning shares the same flag.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    #[error("This file was too heavy for the safe limit ({0} MB). The original is untouched.")]
    TooHeavy(usize),
    #[error(
        "This took longer than the safe limit ({0} s) and was stopped. The original is untouched."
    )]
    TimedOut(u64),
    #[error("Cancelled.")]
    Cancelled,
    #[error("The worker stopped unexpectedly (exit code {code:#x}). {stderr}")]
    Crashed { code: u32, stderr: String },
    #[error("The worker sent a response Flick could not understand ({0}).")]
    BadResponse(String),
    #[error("Could not start the worker ({0}).")]
    Spawn(std::io::Error),
    #[error("Windows refused to set up the safety limits ({0}).")]
    Os(#[from] windows::core::Error),
}

/// Starts `worker`, sends `request` as JSON on stdin and returns its parsed JSON response.
///
/// Blocks until the worker exits, a limit is hit or `cancel` is set, so call it from a
/// background thread.
pub fn run<Req, Resp>(
    worker: &Worker,
    request: &Req,
    limits: &Limits,
    cancel: &CancelToken,
) -> Result<Resp, SupervisorError>
where
    Req: Serialize,
    Resp: DeserializeOwned,
{
    let request = serde_json::to_vec(request)
        .map_err(|e| SupervisorError::BadResponse(format!("request: {e}")))?;
    let job = Job::new(limits)?;

    let mut child = Command::new(&worker.exe)
        .args(&worker.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Suspended so the worker cannot run a single instruction outside the job.
        .creation_flags((CREATE_SUSPENDED | CREATE_NO_WINDOW).0)
        .spawn()
        .map_err(SupervisorError::Spawn)?;

    if let Err(e) = job.assign_and_resume(&child) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(e.into());
    }

    let mut stdin = child.stdin.take().expect("piped stdin");
    let writer = std::thread::spawn(move || {
        // A worker that exits without reading closes the pipe; that is reported elsewhere.
        let _ = stdin.write_all(&request);
    });
    let stdout = read_in_background(child.stdout.take().expect("piped stdout"), false);
    let stderr = read_in_background(child.stderr.take().expect("piped stderr"), true);

    let stop = job.wait(limits.time, cancel);
    if stop != Stop::Exited {
        job.terminate();
    }
    let status = child.wait().map_err(SupervisorError::Spawn)?;
    let _ = writer.join();
    let (stdout, stdout_overflow) = stdout.join().unwrap_or_default();
    let (stderr, _) = stderr.join().unwrap_or_default();

    match stop {
        Stop::Memory => return Err(SupervisorError::TooHeavy(limits.memory_mb)),
        Stop::Time => return Err(SupervisorError::TimedOut(limits.time.as_secs())),
        Stop::Cancelled => return Err(SupervisorError::Cancelled),
        Stop::Exited => {}
    }
    if !status.success() {
        return Err(SupervisorError::Crashed {
            code: status.code().unwrap_or(-1) as u32,
            stderr: String::from_utf8_lossy(&stderr).trim().to_string(),
        });
    }
    if stdout_overflow {
        return Err(SupervisorError::BadResponse(format!(
            "more than {MAX_RESPONSE_BYTES} bytes"
        )));
    }
    serde_json::from_slice(&stdout).map_err(|e| SupervisorError::BadResponse(e.to_string()))
}

#[derive(Debug, PartialEq, Eq)]
enum Stop {
    Exited,
    Memory,
    Time,
    Cancelled,
}

/// A Job Object plus the completion port that receives its notifications.
struct Job {
    job: Handle,
    port: Handle,
}

impl Job {
    fn new(limits: &Limits) -> windows::core::Result<Self> {
        unsafe {
            let job = Handle(CreateJobObjectW(None, None)?);
            let port = Handle(CreateIoCompletionPort(INVALID_HANDLE_VALUE, None, 0, 1)?);

            let mut extended = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            extended.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_JOB_MEMORY
                | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION
                | JOB_OBJECT_LIMIT_PRIORITY_CLASS;
            extended.BasicLimitInformation.PriorityClass = BELOW_NORMAL_PRIORITY_CLASS.0;
            extended.JobMemoryLimit = limits.memory_mb << 20;
            set_info(job.0, JobObjectExtendedLimitInformation, &extended)?;

            let mut cpu = JOBOBJECT_CPU_RATE_CONTROL_INFORMATION {
                ControlFlags: JOB_OBJECT_CPU_RATE_CONTROL_ENABLE
                    | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP,
                ..Default::default()
            };
            // Expressed in hundredths of a percent of the whole machine.
            cpu.Anonymous.CpuRate = limits.cpu_percent.clamp(1, 100) * 100;
            set_info(job.0, JobObjectCpuRateControlInformation, &cpu)?;

            let association = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
                CompletionKey: std::ptr::null_mut(),
                CompletionPort: port.0,
            };
            set_info(
                job.0,
                JobObjectAssociateCompletionPortInformation,
                &association,
            )?;

            Ok(Self { job, port })
        }
    }

    fn assign_and_resume(&self, child: &Child) -> windows::core::Result<()> {
        unsafe {
            AssignProcessToJobObject(self.job.0, HANDLE(child.as_raw_handle()))?;
            resume_main_thread(child.id())
        }
    }

    /// Waits for the job to finish, a memory limit message, the deadline or cancellation.
    fn wait(&self, limit: Duration, cancel: &CancelToken) -> Stop {
        let deadline = Instant::now() + limit;
        loop {
            if cancel.is_cancelled() {
                return Stop::Cancelled;
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return Stop::Time;
            };
            let timeout = left.min(POLL).as_millis() as u32;
            let mut message = 0u32;
            let mut key = 0usize;
            let mut overlapped: *mut OVERLAPPED = std::ptr::null_mut();
            let got = unsafe {
                GetQueuedCompletionStatus(
                    self.port.0,
                    &mut message,
                    &mut key,
                    &mut overlapped,
                    timeout,
                )
            };
            if got.is_ok() {
                match message {
                    JOB_OBJECT_MSG_JOB_MEMORY_LIMIT | JOB_OBJECT_MSG_PROCESS_MEMORY_LIMIT => {
                        return Stop::Memory
                    }
                    JOB_OBJECT_MSG_ACTIVE_PROCESS_ZERO => return Stop::Exited,
                    _ => {}
                }
            }
        }
    }

    fn terminate(&self) {
        unsafe {
            let _ = TerminateJobObject(self.job.0, 1);
        }
    }
}

unsafe fn set_info<T>(
    job: HANDLE,
    class: JOBOBJECTINFOCLASS,
    info: &T,
) -> windows::core::Result<()> {
    SetInformationJobObject(
        job,
        class,
        (info as *const T).cast(),
        std::mem::size_of::<T>() as u32,
    )
}

/// Resumes the single thread of a process that was created suspended.
unsafe fn resume_main_thread(pid: u32) -> windows::core::Result<()> {
    let snapshot = Handle(CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0)?);
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut found = Thread32First(snapshot.0, &mut entry);
    while found.is_ok() {
        if entry.th32OwnerProcessID == pid {
            let thread = Handle(OpenThread(
                THREAD_SUSPEND_RESUME,
                false,
                entry.th32ThreadID,
            )?);
            if ResumeThread(thread.0) == u32::MAX {
                return Err(windows::core::Error::from_thread());
            }
            return Ok(());
        }
        found = Thread32Next(snapshot.0, &mut entry);
    }
    Err(windows::core::Error::from_thread())
}

/// Reads a pipe to the end on its own thread. Keeps the first (or, with `keep_tail`, the
/// last) bytes up to the cap and keeps draining after that, so the worker never blocks on a
/// full pipe. Returns the bytes and whether anything was dropped.
fn read_in_background(
    mut pipe: impl Read + Send + 'static,
    keep_tail: bool,
) -> JoinHandle<(Vec<u8>, bool)> {
    let cap = if keep_tail {
        MAX_STDERR_BYTES
    } else {
        MAX_RESPONSE_BYTES
    };
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        let mut dropped = false;
        let mut buf = [0u8; 8192];
        loop {
            let n = match pipe.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            kept.extend_from_slice(&buf[..n]);
            if kept.len() > cap {
                dropped = true;
                if keep_tail {
                    kept.drain(..kept.len() - cap);
                } else {
                    kept.truncate(cap);
                }
            }
        }
        (kept, dropped)
    })
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
