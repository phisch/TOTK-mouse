use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::FileExt;

/// Writing another process's memory needs ptrace rights. With
/// `kernel.yama.ptrace_scope = 1` that means being its ancestor, which is why we
/// launch the emulator ourselves instead of attaching to a running one.
pub struct Process {
    pid: u32,
    mem: File,
}

impl Process {
    pub fn open(pid: u32) -> Option<Process> {
        let mem = OpenOptions::new().read(true).write(true).open(format!("/proc/{pid}/mem")).ok()?;
        Some(Process { pid, mem })
    }

    /// Candidate guest RAM regions, most resident first. Emulators reserve huge
    /// address ranges they never touch; those have almost nothing resident and must
    /// not be read (reading hundreds of GB of untouched reservation takes minutes),
    /// so this goes by resident size, not mapping size.
    pub fn guest_ram_regions(&self) -> Vec<(u64, u64)> {
        let Ok(smaps) = fs::read_to_string(format!("/proc/{}/smaps", self.pid)) else { return Vec::new() };
        let mut out: Vec<(u64, u64, u64)> = Vec::new();
        let mut cur: Option<(u64, u64)> = None;
        for line in smaps.lines() {
            let mut parts = line.split_whitespace();
            let first = parts.next().unwrap_or("");
            if let Some((a, b)) = first.split_once('-') {
                let perms = parts.next().unwrap_or("");
                cur = None;
                if perms.len() == 4 && perms.starts_with("rw") && !perms.contains('x') {
                    if let (Ok(a), Ok(b)) = (u64::from_str_radix(a, 16), u64::from_str_radix(b, 16)) {
                        cur = Some((a, b));
                    }
                }
            } else if first == "Rss:" {
                if let (Some((a, b)), Some(kb)) = (cur, parts.next().and_then(|v| v.parse::<u64>().ok())) {
                    if kb * 1024 >= super::MIN_REGION {
                        out.push((a, b, kb));
                    }
                }
            }
        }
        out.sort_by(|x, y| y.2.cmp(&x.2));
        out.into_iter().map(|(a, b, _)| (a, b)).collect()
    }

    /// Bytes read, 0 on failure.
    pub fn read_at(&self, buf: &mut [u8], addr: u64) -> usize {
        self.mem.read_at(buf, addr).unwrap_or(0)
    }

    pub fn write_at(&self, bytes: &[u8], addr: u64) -> std::io::Result<()> {
        self.mem.write_all_at(bytes, addr)
    }
}

fn children_of(pid: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let Ok(dir) = fs::read_dir("/proc") else { return out };
    for e in dir.flatten() {
        let Ok(p) = e.file_name().to_string_lossy().parse::<u32>() else { continue };
        let Ok(stat) = fs::read_to_string(format!("/proc/{p}/stat")) else { continue };
        let Some(rest) = stat.rsplit_once(')') else { continue };
        if rest.1.split_whitespace().nth(1).and_then(|s| s.parse::<u32>().ok()) == Some(pid) {
            out.push(p);
        }
    }
    out
}

/// `root` and all its descendants. The launched process may be a wrapper (an
/// AppImage launcher, for example).
pub fn descendants(root: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(p) = stack.pop() {
        out.push(p);
        stack.extend(children_of(p));
    }
    out
}
