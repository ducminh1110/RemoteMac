//! Crash records that need no --logs-enabled. A panic appends its message and a backtrace to
//! %LOCALAPPDATA%\Taro\crash.log; a crash Rust never sees (an illegal instruction, an access
//! violation in a driver) leaves a minidump beside it and one line in crash.log.

use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Dumps kept: the newest five.
const KEEP_DUMPS: usize = 5;
/// The log is meant to stay small: past this size it is cut to its newer half at start.
const LOG_LIMIT: u64 = 512 * 1024;

/// Where crash records go: %LOCALAPPDATA%\Taro.
pub fn dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("Taro")
}

/// The always-on crash log.
pub fn log_path() -> PathBuf {
    dir().join("crash.log")
}

/// Start keeping crash records (first thing in main). The panic hook writes its record, then
/// hands the panic on to the hook that was there before it.
pub fn install() {
    let dir = dir();
    let log = dir.join("crash.log");
    trim(&log, LOG_LIMIT);
    // one place left for this run's dump, so there are never more than KEEP_DUMPS
    prune(&dir, KEEP_DUMPS - 1);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let at = info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
        let record = panic_record(now(), thread.name().unwrap_or("<unnamed>"), info.payload_as_str().unwrap_or("(no message)"), at.as_deref(), &std::backtrace::Backtrace::force_capture());
        append(&log, record.as_bytes());
        previous(info);
    }));
    #[cfg(windows)]
    seh::install(&dir);
}

/// A panic as crash.log keeps it.
fn panic_record(secs: u64, thread: &str, message: &str, at: Option<&str>, backtrace: &dyn fmt::Display) -> String {
    format!("---- {} Taro {}: panic on thread '{thread}' at {}\n{message}\n{backtrace}\n", Utc(secs), env!("CARGO_PKG_VERSION"), at.unwrap_or("an unknown place"))
}

fn append(path: &Path, bytes: &[u8]) {
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(bytes);
    }
}

/// Seconds since 1970 (UTC).
fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Year, month, day, hour, minute, second of a UNIX time (UTC; days to civil, H. Hinnant).
fn civil(secs: u64) -> (u64, u64, u64, u64, u64, u64) {
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + u64::from(m <= 2);
    (y, m, d, rem / 3600, rem / 60 % 60, rem % 60)
}

/// "2026-10-16 08:05:09 UTC"
struct Utc(u64);
impl fmt::Display for Utc {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let (y, mo, d, h, mi, s) = civil(self.0);
        write!(f, "{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02} UTC")
    }
}

/// "20261016-080509", for file names (sorts by time)
#[cfg_attr(not(windows), allow(dead_code))] // only the Windows filter uses it; tested everywhere
struct Stamp(u64);
impl fmt::Display for Stamp {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let (y, mo, d, h, mi, s) = civil(self.0);
        write!(f, "{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}")
    }
}

/// Text formatted into a fixed buffer: the exception filter must not lean on the heap of a
/// process that is going down. What does not fit is dropped.
#[cfg_attr(not(windows), allow(dead_code))]
struct Buf<const N: usize> {
    b: [u8; N],
    n: usize,
}
#[cfg_attr(not(windows), allow(dead_code))]
impl<const N: usize> Buf<N> {
    fn new() -> Self {
        Buf { b: [0; N], n: 0 }
    }
    fn bytes(&self) -> &[u8] {
        &self.b[..self.n]
    }
}
impl<const N: usize> fmt::Write for Buf<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let take = s.len().min(N - self.n);
        self.b[self.n..self.n + take].copy_from_slice(&s.as_bytes()[..take]);
        self.n += take;
        Ok(())
    }
}

/// Remove all but the newest `keep` crash-*.dmp in `dir` (their names sort by time).
fn prune(dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut dumps: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("crash-") && n.ends_with(".dmp"))).collect();
    dumps.sort();
    for old in &dumps[..dumps.len().saturating_sub(keep)] {
        let _ = std::fs::remove_file(old);
    }
}

/// Past `limit` bytes, keep the newer half of the log (from a line start).
fn trim(path: &Path, limit: u64) {
    if std::fs::metadata(path).map_or(true, |m| m.len() <= limit) {
        return;
    }
    let Ok(all) = std::fs::read(path) else { return };
    let from = all.len() - (limit / 2) as usize;
    let from = all[from..].iter().position(|&b| b == b'\n').map_or(all.len(), |i| from + i + 1);
    let _ = std::fs::write(path, &all[from..]);
}

/// The unhandled-exception filter: a minidump and a line in crash.log, then Windows Error
/// Reporting goes on as usual.
#[cfg(windows)]
mod seh {
    use super::{Buf, Stamp, Utc};
    use std::fmt::Write as _;
    use std::os::windows::ffi::OsStrExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::OnceLock;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, EXCEPTION_STACK_OVERFLOW, GENERIC_WRITE, HMODULE};
    use windows::Win32::Storage::FileSystem::{CreateDirectoryW, CreateFileW, WriteFile, CREATE_ALWAYS, FILE_APPEND_DATA, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_NONE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_ALWAYS};
    use windows::Win32::System::Diagnostics::Debug::{MiniDumpNormal, MiniDumpWithIndirectlyReferencedMemory, MiniDumpWithThreadInfo, MiniDumpWriteDump, SetUnhandledExceptionFilter, EXCEPTION_CONTINUE_SEARCH, EXCEPTION_POINTERS, MINIDUMP_EXCEPTION_INFORMATION, MINIDUMP_TYPE};
    use windows::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT};
    use windows::Win32::System::Threading::{CreateThread, GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId, WaitForSingleObject, THREAD_CREATION_FLAGS};

    /// The crash folder in UTF-16 (no NUL), made ready at start so the filter allocates nothing.
    static DIR: OnceLock<Vec<u16>> = OnceLock::new();
    /// Only the first crash is recorded (two threads can fault at once).
    static TAKEN: AtomicBool = AtomicBool::new(false);
    const MAX_PATH: usize = 1024;

    pub fn install(dir: &std::path::Path) {
        let _ = DIR.set(dir.as_os_str().encode_wide().collect());
        unsafe {
            SetUnhandledExceptionFilter(Some(filter));
        }
    }

    /// What a helper thread needs to record a crash for the faulting one.
    struct Fault {
        ptrs: *const EXCEPTION_POINTERS,
        thread: u32,
    }

    unsafe extern "system" fn filter(ptrs: *const EXCEPTION_POINTERS) -> i32 {
        if ptrs.is_null() || (*ptrs).ExceptionRecord.is_null() || TAKEN.swap(true, Ordering::SeqCst) {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        let fault = Fault { ptrs, thread: GetCurrentThreadId() };
        // an overflowed stack has no room left for dbghelp: write from a fresh thread then
        if (*(*ptrs).ExceptionRecord).ExceptionCode == EXCEPTION_STACK_OVERFLOW {
            if let Ok(t) = CreateThread(None, 0, Some(helper), Some(&fault as *const Fault as *const _), THREAD_CREATION_FLAGS(0), None) {
                WaitForSingleObject(t, 30_000);
                let _ = CloseHandle(t);
                return EXCEPTION_CONTINUE_SEARCH;
            }
        }
        record(&fault);
        EXCEPTION_CONTINUE_SEARCH
    }

    unsafe extern "system" fn helper(fault: *mut core::ffi::c_void) -> u32 {
        record(&*(fault as *const Fault));
        0
    }

    /// `DIR` + `name` + NUL, or None when it does not fit.
    fn path(name: &[u8], out: &mut [u16; MAX_PATH]) -> Option<PCWSTR> {
        let dir = DIR.get()?;
        if dir.len() + 1 + name.len() + 1 > MAX_PATH {
            return None;
        }
        out[..dir.len()].copy_from_slice(dir);
        out[dir.len()] = b'\\' as u16;
        for (o, &c) in out[dir.len() + 1..].iter_mut().zip(name) {
            *o = c as u16;
        }
        out[dir.len() + 1 + name.len()] = 0;
        Some(PCWSTR(out.as_ptr()))
    }

    unsafe fn record(fault: &Fault) {
        let rec = &*(*fault.ptrs).ExceptionRecord;
        let addr = rec.ExceptionAddress as usize;
        let secs = super::now();
        let mut wide = [0u16; MAX_PATH];
        if let Some(dir) = DIR.get() {
            if dir.len() < MAX_PATH {
                wide[..dir.len()].copy_from_slice(dir);
                wide[dir.len()] = 0;
                let _ = CreateDirectoryW(PCWSTR(wide.as_ptr()), None);
            }
        }
        let mut name = Buf::<40>::new();
        let _ = write!(name, "crash-{}.dmp", Stamp(secs));
        let mut dumped = false;
        if let Some(p) = path(name.bytes(), &mut wide) {
            if let Ok(file) = CreateFileW(p, GENERIC_WRITE.0, FILE_SHARE_NONE, None, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, None) {
                let info = MINIDUMP_EXCEPTION_INFORMATION { ThreadId: fault.thread, ExceptionPointers: fault.ptrs as *mut _, ClientPointers: false.into() };
                let kind = MINIDUMP_TYPE(MiniDumpNormal.0 | MiniDumpWithThreadInfo.0 | MiniDumpWithIndirectlyReferencedMemory.0);
                dumped = MiniDumpWriteDump(GetCurrentProcess(), GetCurrentProcessId(), file, kind, Some(&info), None, None).is_ok();
                let _ = CloseHandle(file);
            }
        }
        let mut line = Buf::<640>::new();
        let _ = write!(line, "---- {} Taro {}: exception 0x{:08X} at 0x{addr:X}", Utc(secs), env!("CARGO_PKG_VERSION"), rec.ExceptionCode.0 as u32);
        // which module the address is in (the driver, a system DLL, Taro itself) and where
        let mut module = HMODULE::default();
        if GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, PCWSTR(addr as *const u16), &mut module).is_ok() {
            let n = GetModuleFileNameW(Some(module), &mut wide) as usize;
            let file = &wide[..n.min(MAX_PATH)];
            let base = file.iter().rposition(|&c| c == b'\\' as u16).map_or(file, |i| &file[i + 1..]);
            let _ = line.write_str(" (");
            for c in char::decode_utf16(base.iter().copied()) {
                let _ = line.write_char(c.unwrap_or(char::REPLACEMENT_CHARACTER));
            }
            let _ = write!(line, "+0x{:X})", addr.wrapping_sub(module.0 as usize));
        }
        let _ = write!(line, " on thread {}; ", fault.thread);
        if dumped {
            let _ = line.write_str("dump ");
            let _ = line.write_str(core::str::from_utf8(name.bytes()).unwrap_or("?"));
        } else {
            let _ = line.write_str("no dump written");
        }
        let _ = line.write_str("\n");
        if let Some(p) = path(b"crash.log", &mut wide) {
            if let Ok(log) = CreateFileW(p, FILE_APPEND_DATA.0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_ALWAYS, FILE_ATTRIBUTE_NORMAL, None) {
                let _ = WriteFile(log, Some(line.bytes()), None, None);
                let _ = CloseHandle(log);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(civil(951_782_400), (2000, 2, 29, 0, 0, 0));
        assert_eq!(civil(1_792_137_909), (2026, 10, 16, 8, 5, 9));
        assert_eq!(Utc(1_792_137_909).to_string(), "2026-10-16 08:05:09 UTC");
        assert_eq!(Stamp(1_792_137_909).to_string(), "20261016-080509");
    }

    #[test]
    fn panic_record_has_what_a_tester_needs() {
        let r = panic_record(1_792_137_909, "main", "index out of bounds", Some("src/ui.rs:12:5"), &"   0: rm_viewer::ui::run");
        assert!(r.starts_with(&format!("---- 2026-10-16 08:05:09 UTC Taro {}: panic on thread 'main' at src/ui.rs:12:5\n", env!("CARGO_PKG_VERSION"))), "{r}");
        assert!(r.contains("\nindex out of bounds\n") && r.ends_with("rm_viewer::ui::run\n"), "{r}");
    }

    #[test]
    fn buf_drops_what_does_not_fit() {
        let mut b = Buf::<8>::new();
        let _ = write!(b, "0x{:08X}", 0xC000_001Du32);
        assert_eq!(b.bytes(), b"0xC00000");
    }

    #[test]
    fn prune_keeps_the_newest_dumps_and_trim_keeps_the_newest_lines() {
        let d = std::env::temp_dir().join(format!("rm-crash-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for i in 0..7 {
            std::fs::write(d.join(format!("crash-2026101{i}-000000.dmp")), b"x").unwrap();
        }
        std::fs::write(d.join("crash.log"), b"").unwrap();
        prune(&d, 4);
        let mut left: Vec<String> = std::fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        left.sort();
        assert_eq!(left, ["crash-20261013-000000.dmp", "crash-20261014-000000.dmp", "crash-20261015-000000.dmp", "crash-20261016-000000.dmp", "crash.log"]);

        let log = d.join("crash.log");
        let lines: String = (0..100).map(|i| format!("line {i:03}\n")).collect();
        std::fs::write(&log, &lines).unwrap();
        trim(&log, 400);
        let kept = std::fs::read_to_string(&log).unwrap();
        assert!(kept.starts_with("line ") && kept.ends_with("line 099\n") && kept.len() <= 200, "{kept}");
        trim(&log, 400); // small enough: left alone
        assert_eq!(std::fs::read_to_string(&log).unwrap(), kept);
        let _ = std::fs::remove_dir_all(&d);
    }
}
