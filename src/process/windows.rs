//! Windows process containment (plan R23) and the Low integrity verify boundary (R36).
//!
//! * Every child is created **suspended**, assigned to its own Job Object with
//!   `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` (no breakaway), and only then resumed.
//!   If assignment fails the child is terminated while still suspended.
//! * `Job::terminate` = `TerminateJobObject`; `wait_empty` polls
//!   `JobObjectBasicAccountingInformation.ActiveProcesses == 0`.
//! * If toryo.exe dies, its Job handles close and Windows kills every descendant.
//! * `Boundary::LowIntegrity`: a duplicate of the caller's token with the
//!   integrity level lowered to Low (S-1-16-4096), started via
//!   `CreateProcessAsUserW`. Windows Mandatory Integrity Control's default
//!   no-write-up policy then denies writes to every Medium object.

#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_void, OsStr};
use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, SetHandleInformation, BOOL, ERROR_ACCESS_DENIED,
    FILETIME, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, STILL_ACTIVE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, ConvertStringSidToSidW,
    SetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    DuplicateTokenEx, GetLengthSid, GetSecurityDescriptorSacl, SecurityImpersonation,
    SetTokenInformation, TokenIntegrityLevel, TokenPrimary, ACL, LABEL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, SID_AND_ATTRIBUTES, TOKEN_ADJUST_DEFAULT,
    TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicAccountingInformation,
    JobObjectBasicProcessIdList, JobObjectExtendedLimitInformation, QueryInformationJobObject,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::SystemServices::SE_GROUP_INTEGRITY;
use windows_sys::Win32::System::Threading::{
    CreateProcessAsUserW, CreateProcessW, GetCurrentProcess, GetCurrentProcessId,
    GetExitCodeProcess, GetProcessTimes, OpenProcess, OpenProcessToken, OpenThread, ResumeThread,
    TerminateProcess, WaitForSingleObject, CREATE_NO_WINDOW, CREATE_SUSPENDED,
    CREATE_UNICODE_ENVIRONMENT, PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, STARTF_USESTDHANDLES, STARTUPINFOW,
    THREAD_SUSPEND_RESUME,
};

use super::{quote_windows_arg, Boundary, ProcId, SpawnSpec};

const LOW_INTEGRITY_SID: &str = "S-1-16-4096";
/// Mandatory label ACE: Low, object+container inherit, no-write-up.
const LOW_LABEL_SDDL: &str = "S:(ML;OICI;NW;;;LW)";

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

fn last_error(what: &str) -> anyhow::Error {
    // SAFETY: no preconditions.
    let code = unsafe { GetLastError() };
    anyhow::anyhow!(
        "{what} failed (Win32 error {code}: {})",
        std::io::Error::from_raw_os_error(code as i32)
    )
}

/// Owned HANDLE closed on drop.
pub struct Owned(pub HANDLE);
impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            // SAFETY: we own the handle.
            unsafe { CloseHandle(self.0) };
        }
    }
}
unsafe impl Send for Owned {}
unsafe impl Sync for Owned {}

pub struct Job {
    h: std::sync::Arc<Owned>,
}

impl Job {
    pub fn new() -> Result<Self> {
        // SAFETY: FFI with valid pointers; the handle is owned by `Owned`.
        unsafe {
            let h = CreateJobObjectW(null(), null());
            if h.is_null() {
                return Err(last_error("CreateJobObjectW"));
            }
            let owned = Owned(h);
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                h,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                return Err(last_error("SetInformationJobObject(KILL_ON_JOB_CLOSE)"));
            }
            Ok(Self {
                h: std::sync::Arc::new(owned),
            })
        }
    }

    fn raw(&self) -> HANDLE {
        self.h.0
    }

    fn assign(&self, process: HANDLE) -> Result<()> {
        if std::env::var_os("TORYO_FAULT_ASSIGN").is_some() {
            // Keep the (suspended) child alive long enough that an UNsuspended child would
            // certainly have written its marker — otherwise the test could not tell the difference.
            std::thread::sleep(std::time::Duration::from_secs(2));
            bail!("fault injection: AssignProcessToJobObject failed (TORYO_FAULT_ASSIGN)");
        }
        // SAFETY: both handles valid for the duration of the call.
        if unsafe { AssignProcessToJobObject(self.raw(), process) } == 0 {
            return Err(last_error("AssignProcessToJobObject"));
        }
        Ok(())
    }

    pub fn terminate(&self, code: u32) -> Result<()> {
        // SAFETY: valid job handle.
        if unsafe { TerminateJobObject(self.raw(), code) } == 0 {
            return Err(last_error("TerminateJobObject"));
        }
        Ok(())
    }

    pub fn active_processes(&self) -> Result<u32> {
        // SAFETY: out-struct sized correctly.
        unsafe {
            let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = std::mem::zeroed();
            let ok = QueryInformationJobObject(
                self.raw(),
                JobObjectBasicAccountingInformation,
                &mut info as *mut _ as *mut c_void,
                std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                null_mut(),
            );
            if ok == 0 {
                return Err(last_error("QueryInformationJobObject(accounting)"));
            }
            Ok(info.ActiveProcesses)
        }
    }

    /// (pid, creation time) of every process currently in the Job.
    pub fn process_ids(&self) -> Result<Vec<ProcId>> {
        // JOBOBJECT_BASIC_PROCESS_ID_LIST = { u32 assigned, u32 in_list, usize ids[] }
        let mut buf = vec![0usize; 2 + 1024];
        // SAFETY: buffer large enough for 1024 ids plus the header.
        unsafe {
            let ok = QueryInformationJobObject(
                self.raw(),
                JobObjectBasicProcessIdList,
                buf.as_mut_ptr() as *mut c_void,
                (buf.len() * std::mem::size_of::<usize>()) as u32,
                null_mut(),
            );
            if ok == 0 {
                return Err(last_error("QueryInformationJobObject(pid list)"));
            }
            let header = buf.as_ptr() as *const u32;
            let n = *header.add(1) as usize;
            let ids = (buf.as_ptr() as *const u8).add(8) as *const usize;
            let mut out = Vec::with_capacity(n);
            for i in 0..n {
                let pid = *ids.add(i) as u32;
                if let Some(created) = creation_time(pid) {
                    out.push(ProcId { pid, created });
                }
            }
            Ok(out)
        }
    }

    pub fn wait_empty(&self, max: Duration) -> Result<bool> {
        let deadline = Instant::now() + max;
        loop {
            if self.active_processes()? == 0 {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn clone_handle(&self) -> Job {
        Job { h: self.h.clone() }
    }
}

fn filetime_u64(f: FILETIME) -> u64 {
    ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64
}

fn creation_time_of(h: HANDLE) -> Option<u64> {
    // SAFETY: out params are valid FILETIMEs.
    unsafe {
        let (mut c, mut e, mut k, mut u): (FILETIME, FILETIME, FILETIME, FILETIME) = (
            std::mem::zeroed(),
            std::mem::zeroed(),
            std::mem::zeroed(),
            std::mem::zeroed(),
        );
        if GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u) == 0 {
            return None;
        }
        Some(filetime_u64(c))
    }
}

fn creation_time(pid: u32) -> Option<u64> {
    // SAFETY: handle closed by Owned.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let _o = Owned(h);
        creation_time_of(h)
    }
}

pub fn is_alive(p: ProcId) -> bool {
    // SAFETY: handle closed by Owned.
    unsafe {
        let h = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            p.pid,
        );
        if h.is_null() {
            // M10-R7: an elevated toryo of the same user refuses SYNCHRONIZE but grants the
            // limited query right: check its creation time with that right alone
            if GetLastError() != ERROR_ACCESS_DENIED {
                return false;
            }
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, p.pid);
            if h.is_null() {
                // M10-R9: refused even that — not a toryo of this user (a same-user process,
                // elevated or not, grants it), e.g. a SYSTEM process that got a dead owner's
                // reused PID: not alive, or `watch` and `status` would see it `ACTIVE` forever
                return false;
            }
            let _o = Owned(h);
            if creation_time_of(h) != Some(p.created) {
                return false;
            }
            let mut code = 0u32;
            return GetExitCodeProcess(h, &mut code) != 0 && code == STILL_ACTIVE as u32;
        }
        let _o = Owned(h);
        if creation_time_of(h) != Some(p.created) {
            return false; // PID reused by another process
        }
        WaitForSingleObject(h, 0) != WAIT_OBJECT_0
    }
}

pub fn kill_pid(pid: u32) -> Result<()> {
    // SAFETY: handle closed by Owned.
    unsafe {
        let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if h.is_null() {
            return Err(last_error("OpenProcess(PROCESS_TERMINATE)"));
        }
        let _o = Owned(h);
        if TerminateProcess(h, 1) == 0 {
            return Err(last_error("TerminateProcess"));
        }
    }
    Ok(())
}

pub fn current_proc_id() -> ProcId {
    // SAFETY: pseudo-handle, no close needed.
    unsafe {
        let pid = GetCurrentProcessId();
        ProcId {
            pid,
            created: creation_time_of(GetCurrentProcess()).unwrap_or(0),
        }
    }
}

/// Resume the (single) main thread of a process created with CREATE_SUSPENDED.
fn resume_main_thread(pid: u32) -> Result<()> {
    // SAFETY: snapshot and thread handles are closed by Owned.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snap == INVALID_HANDLE_VALUE {
            return Err(last_error("CreateToolhelp32Snapshot"));
        }
        let _s = Owned(snap);
        let mut te: THREADENTRY32 = std::mem::zeroed();
        te.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        let mut more: BOOL = Thread32First(snap, &mut te);
        while more != 0 {
            if te.th32OwnerProcessID == pid {
                let th = OpenThread(THREAD_SUSPEND_RESUME, 0, te.th32ThreadID);
                if !th.is_null() {
                    let _t = Owned(th);
                    let prev = ResumeThread(th);
                    if prev == u32::MAX {
                        return Err(last_error("ResumeThread"));
                    }
                    if prev > 0 {
                        return Ok(()); // this was the suspended thread
                    }
                    // prev == 0: not suspended, keep searching
                }
            }
            more = Thread32Next(snap, &mut te);
        }
    }
    bail!("no suspended thread found for pid {pid}")
}

pub enum Child {
    Std(std::process::Child),
    Raw { process: Owned },
}

impl Child {
    pub fn try_exit_code(&mut self) -> Result<Option<i32>> {
        match self {
            Child::Std(c) => Ok(c.try_wait()?.map(|s| s.code().unwrap_or(-1))),
            Child::Raw { process } => {
                // SAFETY: valid process handle.
                unsafe {
                    if WaitForSingleObject(process.0, 0) != WAIT_OBJECT_0 {
                        return Ok(None);
                    }
                    let mut code = 0u32;
                    if GetExitCodeProcess(process.0, &mut code) == 0 {
                        return Err(last_error("GetExitCodeProcess"));
                    }
                    Ok(Some(code as i32))
                }
            }
        }
    }

    /// M11-R17: the signal that ended the process — a Unix notion; always `None` here.
    pub fn signal(&self) -> Option<i32> {
        None
    }
}

pub struct Spawned {
    pub job: Job,
    pub root: ProcId,
    pub child: Child,
    pub stdin: Option<Box<dyn Write + Send>>,
    pub stdout: Option<Box<dyn Read + Send>>,
    pub stderr: Option<Box<dyn Read + Send>>,
}

pub fn spawn_in_job(spec: &SpawnSpec) -> Result<Spawned> {
    match spec.boundary {
        Boundary::None => spawn_std(spec),
        Boundary::LowIntegrity => spawn_low_integrity(spec),
    }
}

/// Normal token: std::process with CREATE_SUSPENDED → assign → resume (zeroshot pattern).
fn spawn_std(spec: &SpawnSpec) -> Result<Spawned> {
    let job = Job::new()?;
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args)
        .current_dir(&spec.cwd)
        .env_clear()
        .envs(&spec.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning {}", spec.program.display()))?;
    let handle = child.as_raw_handle() as HANDLE;
    let pid = child.id();
    if let Err(e) = job.assign(handle).and_then(|_| resume_main_thread(pid)) {
        // Nothing runs outside a Job: kill it while it is still suspended.
        let _ = job.terminate(1);
        let _ = child.kill();
        let _ = child.wait();
        return Err(e.context("process was terminated before running (not contained)"));
    }
    let created = creation_time_of(handle).unwrap_or(0);
    let stdin = child
        .stdin
        .take()
        .map(|s| Box::new(s) as Box<dyn Write + Send>);
    let stdout = child
        .stdout
        .take()
        .map(|s| Box::new(s) as Box<dyn Read + Send>);
    let stderr = child
        .stderr
        .take()
        .map(|s| Box::new(s) as Box<dyn Read + Send>);
    Ok(Spawned {
        job,
        root: ProcId { pid, created },
        child: Child::Std(child),
        stdin,
        stdout,
        stderr,
    })
}

/// Duplicate our own primary token and lower it to Low integrity.
fn low_integrity_token() -> Result<Owned> {
    // SAFETY: FFI; all handles/SIDs released on every path.
    unsafe {
        let mut tok: HANDLE = null_mut();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_DUPLICATE | TOKEN_ADJUST_DEFAULT | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY,
            &mut tok,
        ) == 0
        {
            return Err(last_error("OpenProcessToken"));
        }
        let tok = Owned(tok);
        let mut dup: HANDLE = null_mut();
        if DuplicateTokenEx(
            tok.0,
            0,
            null(),
            SecurityImpersonation,
            TokenPrimary,
            &mut dup,
        ) == 0
        {
            return Err(last_error("DuplicateTokenEx"));
        }
        let dup = Owned(dup);
        let mut sid: PSID = null_mut();
        let sid_str = wide(OsStr::new(LOW_INTEGRITY_SID));
        if ConvertStringSidToSidW(sid_str.as_ptr(), &mut sid) == 0 {
            return Err(last_error("ConvertStringSidToSidW"));
        }
        let label = TOKEN_MANDATORY_LABEL {
            Label: SID_AND_ATTRIBUTES {
                Sid: sid,
                Attributes: SE_GROUP_INTEGRITY as u32,
            },
        };
        let size = std::mem::size_of::<TOKEN_MANDATORY_LABEL>() as u32 + GetLengthSid(sid);
        let ok = SetTokenInformation(
            dup.0,
            TokenIntegrityLevel,
            &label as *const _ as *const c_void,
            size,
        );
        // read the error before LocalFree, which may overwrite GetLastError (CODE-10)
        let err = (ok == 0).then(|| last_error("SetTokenInformation(TokenIntegrityLevel=Low)"));
        LocalFree(sid as _);
        if let Some(e) = err {
            return Err(e);
        }
        Ok(dup)
    }
}

/// Inheritable anonymous pipe; returns (parent_end, child_end). `child_reads` selects direction.
fn pipe(child_reads: bool) -> Result<(Owned, Owned)> {
    // SAFETY: FFI with valid out pointers.
    unsafe {
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: null_mut(),
            bInheritHandle: 1,
        };
        let (mut r, mut w): (HANDLE, HANDLE) = (null_mut(), null_mut());
        if CreatePipe(&mut r, &mut w, &sa, 0) == 0 {
            return Err(last_error("CreatePipe"));
        }
        let (r, w) = (Owned(r), Owned(w));
        let (parent, child) = if child_reads { (w, r) } else { (r, w) };
        if SetHandleInformation(parent.0, HANDLE_FLAG_INHERIT, 0) == 0 {
            return Err(last_error("SetHandleInformation"));
        }
        Ok((parent, child))
    }
}

/// Windows environment block: sorted (case-insensitively), UTF-16, double-NUL terminated.
fn env_block(env: &std::collections::BTreeMap<String, String>) -> Vec<u16> {
    let mut pairs: Vec<(&String, &String)> = env.iter().collect();
    pairs.sort_by_key(|(k, _)| k.to_uppercase());
    let mut out = Vec::new();
    for (k, v) in pairs {
        out.extend(OsStr::new(&format!("{k}={v}")).encode_wide());
        out.push(0);
    }
    if out.is_empty() {
        out.push(0);
    }
    out.push(0);
    out
}

/// R36: verify commands at Low integrity, still suspended-create → Job → resume.
/// Note (spike): inheritable handles are process-wide; M6 should switch to
/// PROC_THREAD_ATTRIBUTE_HANDLE_LIST so concurrent spawns cannot leak handles.
fn spawn_low_integrity(spec: &SpawnSpec) -> Result<Spawned> {
    let job = Job::new()?;
    let token = low_integrity_token()?;
    let (stdin_parent, stdin_child) = pipe(true)?;
    let (stdout_parent, stdout_child) = pipe(false)?;
    let (stderr_parent, stderr_child) = pipe(false)?;
    let mut cmdline = quote_windows_arg(&spec.program.to_string_lossy());
    for a in &spec.args {
        cmdline.push(' ');
        cmdline.push_str(&quote_windows_arg(a));
    }
    let mut cmdline_w = wide(OsStr::new(&cmdline));
    let env = env_block(&spec.env);
    let cwd = wide(spec.cwd.as_os_str());
    // SAFETY: all pointers live across the call; handles closed via Owned.
    unsafe {
        let mut si: STARTUPINFOW = std::mem::zeroed();
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        si.dwFlags = STARTF_USESTDHANDLES;
        si.hStdInput = stdin_child.0;
        si.hStdOutput = stdout_child.0;
        si.hStdError = stderr_child.0;
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let ok = CreateProcessAsUserW(
            token.0,
            null(),
            cmdline_w.as_mut_ptr(),
            null(),
            null(),
            1,
            CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
            env.as_ptr() as *const c_void,
            cwd.as_ptr(),
            &si,
            &mut pi,
        );
        if ok == 0 {
            return Err(last_error("CreateProcessAsUserW(Low integrity)")
                .context("verify_boundary_unavailable"));
        }
        let process = Owned(pi.hProcess);
        let thread = Owned(pi.hThread);
        drop((stdin_child, stdout_child, stderr_child));
        if let Err(e) = job.assign(process.0) {
            TerminateProcess(process.0, 1);
            return Err(e.context("process was terminated before running (not contained)"));
        }
        if ResumeThread(thread.0) == u32::MAX {
            let _ = job.terminate(1);
            return Err(last_error("ResumeThread"));
        }
        let created = creation_time_of(process.0).unwrap_or(0);
        let into_file = |o: Owned| {
            let h = o.0;
            std::mem::forget(o);
            File::from_raw_handle(h as _)
        };
        Ok(Spawned {
            job,
            root: ProcId {
                pid: pi.dwProcessId,
                created,
            },
            child: Child::Raw { process },
            stdin: Some(Box::new(into_file(stdin_parent))),
            stdout: Some(Box::new(into_file(stdout_parent))),
            stderr: Some(Box::new(into_file(stderr_parent))),
        })
    }
}

/// Give `dir` (and, by inheritance, everything created under it) a Low mandatory label.
pub fn set_low_label(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    // SAFETY: SD allocated by the API and freed with LocalFree.
    unsafe {
        let sddl = wide(OsStr::new(LOW_LABEL_SDDL));
        let mut sd: PSECURITY_DESCRIPTOR = null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            null_mut(),
        ) == 0
        {
            return Err(last_error(
                "ConvertStringSecurityDescriptorToSecurityDescriptorW",
            ));
        }
        let mut present: BOOL = 0;
        let mut defaulted: BOOL = 0;
        let mut sacl: *mut ACL = null_mut();
        if GetSecurityDescriptorSacl(sd, &mut present, &mut sacl, &mut defaulted) == 0 {
            // read the error before LocalFree, which may overwrite GetLastError (CODE-10)
            let e = last_error("GetSecurityDescriptorSacl");
            LocalFree(sd as _);
            return Err(e);
        }
        let path = wide(dir.as_os_str());
        let rc = SetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            LABEL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            null(),
            sacl,
        );
        LocalFree(sd as _);
        if rc == 5 {
            bail!(
                "cannot give {} a Low integrity label: access denied (the account lacks WRITE_OWNER there). \
                 Folders created directly under C:\\ give a standard / UAC-filtered user only Modify. \
                 Put verify.verify_root under %LOCALAPPDATA% (the default), or grant yourself Full Control on that folder",
                dir.display()
            );
        }
        if rc != 0 {
            bail!(
                "SetNamedSecurityInfoW(Low label) on {} failed: Win32 error {rc}",
                dir.display()
            );
        }
    }
    Ok(())
}

/// A process started with no inherited handle and no console window (M10-R4: the toast), so it
/// never holds a pipe of toryo's open (a harness reading toryo's stdout would wait for it).
pub struct Detached {
    process: Owned,
    pub pid: u32,
}

// SAFETY: a process handle may be waited on and terminated from any thread.
unsafe impl Send for Detached {}

impl Detached {
    /// Wait up to `limit`: the exit code, `None` while it still runs.
    pub fn wait(&self, limit: Duration) -> Option<u32> {
        let ms = limit.as_millis().min(u32::MAX as u128 - 1) as u32;
        // SAFETY: we own the process handle.
        unsafe {
            if WaitForSingleObject(self.process.0, ms) != WAIT_OBJECT_0 {
                return None;
            }
            let mut code = 0u32;
            (GetExitCodeProcess(self.process.0, &mut code) != 0).then_some(code)
        }
    }

    pub fn kill(&self) {
        // SAFETY: we own the process handle.
        unsafe { TerminateProcess(self.process.0, 1) };
    }
}

/// R8: what `std::process::Command` refuses too — a NUL in an argument, an environment name or
/// value, an empty name, or `=` in a name after its first character (Windows keeps
/// `=C:`-style names).
pub fn check_spawn_input(
    program: &Path,
    args: &[String],
    env: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Result<()> {
    const NUL: char = '\0';
    // R9: the program path too
    if program.as_os_str().encode_wide().any(|c| c == 0) {
        bail!("the program path holds a NUL character: {program:?}");
    }
    if let Some(a) = args.iter().find(|a| a.contains(NUL)) {
        bail!("an argument holds a NUL character: {a:?}");
    }
    for (k, v) in env {
        let name: Vec<u16> = k.encode_wide().collect();
        if name.is_empty() {
            bail!("an empty environment variable name");
        }
        if name.contains(&0) || v.encode_wide().any(|c| c == 0) {
            bail!("the environment variable {k:?} holds a NUL character");
        }
        if name[1..].contains(&(b'=' as u16)) {
            bail!("the environment variable name {k:?} holds `=`");
        }
    }
    Ok(())
}

/// The order of two environment variable names in a Windows environment block (R9): an ordinal,
/// case-insensitive comparison of the UTF-16 names — `CompareStringOrdinal(a, b, TRUE)`, what
/// std's `EnvKey` uses — not a Rust uppercase of a lossy string (`ß` → `SS`, `ﬀ` → `FF`).
pub fn env_name_order(a: &OsStr, b: &OsStr) -> std::cmp::Ordering {
    use windows_sys::Win32::Globalization::CompareStringOrdinal;
    let (a, b): (Vec<u16>, Vec<u16>) = (a.encode_wide().collect(), b.encode_wide().collect());
    // SAFETY: both slices live across the call; lengths are given explicitly
    let r =
        unsafe { CompareStringOrdinal(a.as_ptr(), a.len() as i32, b.as_ptr(), b.len() as i32, 1) };
    match r {
        1 => std::cmp::Ordering::Less,
        3 => std::cmp::Ordering::Greater,
        _ => std::cmp::Ordering::Equal,
    }
}

/// Windows environment block from `OsString` pairs (non-Unicode values kept): sorted by name,
/// case-insensitively, UTF-16, double-NUL terminated.
fn env_block_os(env: &[(std::ffi::OsString, std::ffi::OsString)]) -> Vec<u16> {
    let mut pairs: Vec<&(std::ffi::OsString, std::ffi::OsString)> = env.iter().collect();
    pairs.sort_by(|a, b| env_name_order(&a.0, &b.0));
    let mut out = Vec::new();
    for (k, v) in pairs {
        out.extend(k.encode_wide());
        out.push(b'=' as u16);
        out.extend(v.encode_wide());
        out.push(0);
    }
    if out.is_empty() {
        out.push(0);
    }
    out.push(0);
    out
}

/// Start `program args` with exactly `env` as its environment, `bInheritHandles = FALSE`,
/// `CREATE_NO_WINDOW`, in toryo's current directory, outside any Job.
pub fn spawn_detached(
    program: &Path,
    args: &[String],
    env: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Result<Detached> {
    check_spawn_input(program, args, env)?;
    let mut cmdline = quote_windows_arg(&program.to_string_lossy());
    for a in args {
        cmdline.push(' ');
        cmdline.push_str(&quote_windows_arg(a));
    }
    let mut cmdline_w = wide(OsStr::new(&cmdline));
    let env = env_block_os(env);
    // SAFETY: all pointers live across the call; the thread handle is closed at once.
    unsafe {
        let mut si: STARTUPINFOW = std::mem::zeroed();
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let ok = CreateProcessW(
            null(),
            cmdline_w.as_mut_ptr(),
            null(),
            null(),
            0,
            CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
            env.as_ptr() as *const c_void,
            null(),
            &si,
            &mut pi,
        );
        if ok == 0 {
            return Err(last_error(&format!(
                "CreateProcessW({})",
                program.display()
            )));
        }
        drop(Owned(pi.hThread));
        Ok(Detached {
            process: Owned(pi.hProcess),
            pid: pi.dwProcessId,
        })
    }
}

#[cfg(test)]
mod detached_tests {
    use super::*;

    fn powershell() -> std::path::PathBuf {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        std::path::PathBuf::from(root)
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe")
    }

    fn sleeper(secs: u32) -> Detached {
        let args: Vec<String> = ["-NoProfile", "-NonInteractive", "-Command"]
            .iter()
            .map(|s| s.to_string())
            .chain([format!("Start-Sleep -Seconds {secs}")])
            .collect();
        spawn_detached(
            &powershell(),
            &args,
            &std::env::vars_os().collect::<Vec<_>>(),
        )
        .unwrap()
    }

    /// M10-R4: a detached process holds none of toryo's inheritable handles — here the write end
    /// of an inheritable pipe: once toryo drops it, the read end is at EOF at once, although the
    /// child still runs (with `bInheritHandles = TRUE` the read would wait for the child).
    #[test]
    fn a_detached_process_inherits_no_handle() {
        let (r, w) = pipe(false).unwrap();
        let p = sleeper(6);
        drop(w);
        let t = Instant::now();
        // SAFETY: the handle is ours; ownership moves to the File
        let mut f = unsafe { File::from_raw_handle(r.0 as _) };
        std::mem::forget(r);
        let mut buf = Vec::new();
        let _ = f.read_to_end(&mut buf);
        assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
        assert_eq!(p.wait(Duration::ZERO), None, "the child still runs");
        p.kill();
        assert!(p.wait(Duration::from_secs(10)).is_some());
    }
}

#[cfg(test)]
mod alive_tests {
    use super::*;

    /// M10-R7: a process that refuses `PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE`
    /// (elevated, another user's, a system process) exists and is alive. The System process
    /// (PID 4) refuses them to a non-elevated test run; an elevated run can open it and the test
    /// has nothing to show (it says so).
    #[test]
    fn access_denied_is_decided_by_the_limited_right() {
        const SYSTEM: u32 = 4;
        // SAFETY: FFI; handles closed by Owned.
        let created = unsafe {
            let h = OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                SYSTEM,
            );
            if !h.is_null() {
                drop(Owned(h));
                eprintln!("the System process can be opened (elevated run): nothing to test");
                return;
            }
            assert_eq!(GetLastError(), ERROR_ACCESS_DENIED);
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, SYSTEM);
            if h.is_null() {
                None
            } else {
                let o = Owned(h);
                creation_time_of(o.0)
            }
        };
        match created {
            // the limited right is granted: the creation time decides (R7)
            Some(created) => {
                assert!(is_alive(ProcId {
                    pid: SYSTEM,
                    created
                }));
                assert!(!is_alive(ProcId {
                    pid: SYSTEM,
                    created: created + 1
                }));
            }
            // M10-R9: refused even the limited right — like a SYSTEM process that reused a dead
            // owner's PID: not alive, whatever the creation time
            None => {
                eprintln!("the System process refuses the limited right too");
                assert!(!is_alive(ProcId {
                    pid: SYSTEM,
                    created: 0
                }));
            }
        }
    }
}

#[cfg(test)]
mod spawn_input_tests {
    use super::*;
    use std::ffi::OsString;

    /// M10-R8: `spawn_detached` refuses what `std::process::Command` refused: a NUL in an
    /// argument, a variable name or value, an empty name, `=` in a name after its first character.
    #[test]
    fn detached_spawns_refuse_what_command_refuses() {
        let ok: Vec<(OsString, OsString)> = vec![
            ("PATH".into(), r"C:\x".into()),
            ("=C:".into(), r"C:\".into()),
        ];
        assert!(
            check_spawn_input(Path::new("p"), &["-Command".into()], &ok).is_ok(),
            "`=C:` names are fine"
        );
        let nul = String::from_utf8(vec![b'a', 0, b'b']).unwrap();
        assert!(check_spawn_input(Path::new("p"), std::slice::from_ref(&nul), &ok).is_err());
        for bad in [
            vec![(OsString::from(nul.as_str()), OsString::from("v"))],
            vec![(OsString::from("K"), OsString::from(nul.as_str()))],
            vec![(OsString::from(""), OsString::from("v"))],
            vec![(OsString::from("A=B"), OsString::from("v"))],
        ] {
            assert!(
                check_spawn_input(Path::new("p"), &[], &bad).is_err(),
                "{bad:?}"
            );
        }
        let p = std::path::PathBuf::from(r"C:\Windows\System32\cmd.exe");
        assert!(
            spawn_detached(&p, std::slice::from_ref(&nul), &ok).is_err(),
            "checked before CreateProcessW"
        );
        // R9: a NUL in the program path too
        assert!(check_spawn_input(Path::new(&nul), &[], &ok).is_err());
    }

    /// M10-R9: environment names are ordered like std's `EnvKey` — ordinal, case-insensitive on
    /// UTF-16 (`CompareStringOrdinal`), not by a Rust uppercase (`ß` → `SS`, `ﬀ` → `FF`).
    #[test]
    fn environment_names_sort_ordinally_ignoring_case() {
        let mut names: Vec<OsString> = ["ﬀX", "😀X", "ßX", "SSA", "path", "PATH2", "Path"]
            .iter()
            .map(OsString::from)
            .collect();
        names.sort_by(|a, b| env_name_order(a, b));
        let got: Vec<String> = names
            .iter()
            .map(|n| n.to_string_lossy().into_owned())
            .collect();
        // `path` and `Path` compare equal (a stable sort keeps their order); U+00DF < the
        // emoji's high surrogate U+D83D < U+FB00
        assert_eq!(got, ["path", "Path", "PATH2", "SSA", "ßX", "😀X", "ﬀX"]);
        assert_eq!(
            env_name_order(OsStr::new("abc"), OsStr::new("ABC")),
            std::cmp::Ordering::Equal
        );
    }
}
