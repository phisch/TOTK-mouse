use std::mem::{size_of, zeroed};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Memory::{VirtualQueryEx, MEMORY_BASIC_INFORMATION, MEM_COMMIT, PAGE_READWRITE};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ, PROCESS_VM_WRITE,
};

#[link(name = "ntdll")]
extern "system" {
    fn NtWriteVirtualMemory(
        process: HANDLE,
        base: *mut core::ffi::c_void,
        buffer: *const core::ffi::c_void,
        size: usize,
        written: *mut usize,
    ) -> i32;
}

pub struct Process {
    handle: HANDLE,
    /// Set once NtWriteVirtualMemory has failed, to stay on WriteProcessMemory.
    use_fallback: std::sync::atomic::AtomicBool,
}

// SAFETY: a process handle is just a kernel handle; any thread may use it.
unsafe impl Send for Process {}

impl Drop for Process {
    fn drop(&mut self) {
        // SAFETY: we own the handle.
        unsafe { CloseHandle(self.handle) };
    }
}

impl Process {
    pub fn open(pid: u32) -> Option<Process> {
        let access = PROCESS_VM_READ | PROCESS_VM_WRITE | PROCESS_VM_OPERATION | PROCESS_QUERY_INFORMATION;
        // SAFETY: plain call; a null handle means failure.
        let handle = unsafe { OpenProcess(access, 0, pid) };
        (!handle.is_null()).then_some(Process { handle, use_fallback: Default::default() })
    }

    /// Committed read-write regions large enough to be guest RAM, largest first.
    /// Reserved-only ranges (fastmem arenas) are skipped, so nothing huge and
    /// untouched gets read.
    pub fn guest_ram_regions(&self) -> Vec<(u64, u64)> {
        let mut out = Vec::new();
        let mut addr: usize = 0;
        loop {
            // SAFETY: MEMORY_BASIC_INFORMATION is plain data.
            let mut info: MEMORY_BASIC_INFORMATION = unsafe { zeroed() };
            // SAFETY: valid handle and output buffer of the right size.
            let n = unsafe {
                VirtualQueryEx(self.handle, addr as *const _, &mut info, size_of::<MEMORY_BASIC_INFORMATION>())
            };
            if n == 0 {
                break;
            }
            let (base, size) = (info.BaseAddress as u64, info.RegionSize as u64);
            if info.State == MEM_COMMIT && info.Protect == PAGE_READWRITE && size >= super::MIN_REGION {
                out.push((base, base + size));
            }
            let next = base + size;
            if next <= addr as u64 {
                break;
            }
            addr = next as usize;
        }
        out.sort_by(|a, b| (b.1 - b.0).cmp(&(a.1 - a.0)));
        out
    }

    /// Bytes read, 0 on failure.
    pub fn read_at(&self, buf: &mut [u8], addr: u64) -> usize {
        let mut got = 0usize;
        // SAFETY: valid handle, buffer and length; failure leaves `got` at what was read.
        let ok = unsafe {
            ReadProcessMemory(self.handle, addr as *const _, buf.as_mut_ptr().cast(), buf.len(), &mut got)
        };
        if ok == 0 && got == 0 { 0 } else { got }
    }

    /// Writes with NtWriteVirtualMemory. WriteProcessMemory changes the target page's
    /// protection around every write (to learn the old one) and may flush the
    /// instruction cache. Each protection change makes every thread of the emulator
    /// drop its address translations, and at one write per frame that took a tester
    /// from 60 to about 10 fps. Our block lives in writable memory, so the plain write
    /// suffices; WriteProcessMemory stays as the fallback.
    pub fn write_at(&self, bytes: &[u8], addr: u64) -> std::io::Result<()> {
        use std::sync::atomic::Ordering;
        if !self.use_fallback.load(Ordering::Relaxed) {
            let mut put = 0usize;
            // SAFETY: valid handle, buffer and length.
            let status = unsafe {
                NtWriteVirtualMemory(self.handle, addr as *mut _, bytes.as_ptr().cast(), bytes.len(), &mut put)
            };
            if status >= 0 && put == bytes.len() {
                return Ok(());
            }
            eprintln!("NtWriteVirtualMemory failed (status 0x{status:08x}), using WriteProcessMemory");
            self.use_fallback.store(true, Ordering::Relaxed);
        }
        let mut put = 0usize;
        // SAFETY: valid handle, buffer and length.
        let ok = unsafe {
            WriteProcessMemory(self.handle, addr as *const _, bytes.as_ptr().cast(), bytes.len(), &mut put)
        };
        if ok == 0 || put != bytes.len() {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

/// `root` and all its descendants.
pub fn descendants(root: u32) -> Vec<u32> {
    // SAFETY: plain call; INVALID_HANDLE_VALUE means failure.
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snap == INVALID_HANDLE_VALUE {
        return vec![root];
    }
    let mut pairs = Vec::new();
    // SAFETY: PROCESSENTRY32W is plain data; dwSize must be set before the first call.
    let mut e: PROCESSENTRY32W = unsafe { zeroed() };
    e.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    // SAFETY: valid snapshot handle and entry.
    let mut ok = unsafe { Process32FirstW(snap, &mut e) };
    while ok != 0 {
        pairs.push((e.th32ProcessID, e.th32ParentProcessID));
        // SAFETY: as above.
        ok = unsafe { Process32NextW(snap, &mut e) };
    }
    // SAFETY: we own the snapshot handle.
    unsafe { CloseHandle(snap) };
    let mut out = vec![root];
    let mut i = 0;
    while i < out.len() {
        let parent = out[i];
        out.extend(pairs.iter().filter(|(p, pp)| *pp == parent && *p != parent).map(|(p, _)| *p));
        i += 1;
    }
    out
}
