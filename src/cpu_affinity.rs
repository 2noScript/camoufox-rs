//! Pinning the browser process to `navigator.hardwareConcurrency` cores, matching `cpu_affinity.ts` / `cpu_affinity.py`.

use std::collections::BTreeSet;
use std::fs;
use std::future::Future;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use tokio::sync::Mutex as AsyncMutex;

static TASKSET_AVAILABLE: OnceLock<bool> = OnceLock::new();
static HOST_CORE_COUNT: OnceLock<usize> = OnceLock::new();
static EVER_PINNED: AtomicBool = AtomicBool::new(false);

fn pin_mutex() -> &'static AsyncMutex<()> {
    static LOCK: OnceLock<AsyncMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| AsyncMutex::new(()))
}

fn run_cmd(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

pub fn supported() -> bool {
    if cfg!(target_os = "linux") {
        *TASKSET_AVAILABLE.get_or_init(|| run_cmd("taskset", &["-V"]).is_some())
    } else {
        cfg!(target_os = "windows")
    }
}

pub fn parse_cpu_list(list: &str) -> Vec<usize> {
    let mut cores = BTreeSet::new();
    for part in list.trim().split(',') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        let mut bounds = p.split('-');
        let Ok(lo) = bounds.next().unwrap_or("").trim().parse::<usize>() else {
            continue;
        };
        let hi = bounds
            .next()
            .and_then(|s| s.trim().parse::<usize>().ok())
            .unwrap_or(lo);
        for c in lo..=hi {
            cores.insert(c);
        }
    }
    cores.into_iter().collect()
}

fn linux_get_affinity(pid: u32) -> Option<Vec<usize>> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("Cpus_allowed_list:") {
            let cores = parse_cpu_list(rest);
            if !cores.is_empty() {
                return Some(cores);
            }
        }
    }
    None
}

fn linux_set_affinity(pid: u32, cores: &[usize]) -> bool {
    let mut sorted = cores.to_vec();
    sorted.sort_unstable();
    let list = sorted
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let pid_str = pid.to_string();
    run_cmd("taskset", &["-p", "-c", &list, &pid_str]).is_some()
}

pub fn mask_to_cores(mask: u128) -> Vec<usize> {
    let mut cores = Vec::new();
    let mut i = 0u32;
    while (mask >> i) > 0 && i < 128 {
        if ((mask >> i) & 1) == 1 {
            cores.push(i as usize);
        }
        i += 1;
    }
    cores
}

pub fn cores_to_mask(cores: &[usize]) -> u128 {
    let mut mask = 0u128;
    for &c in cores {
        if c < 128 {
            mask |= 1u128 << c;
        }
    }
    mask
}

fn win_get_mask(pid: u32) -> u128 {
    let script = format!("[int64](Get-Process -Id {pid}).ProcessorAffinity");
    run_cmd("powershell", &["-NoProfile", "-Command", &script])
        .and_then(|s| s.trim().parse::<u128>().ok())
        .unwrap_or(0)
}

fn win_set_mask(pid: u32, mask: u128) -> bool {
    let script = format!("(Get-Process -Id {pid}).ProcessorAffinity = [IntPtr][int64]{mask}");
    run_cmd("powershell", &["-NoProfile", "-Command", &script]).is_some()
}

pub fn host_cores() -> Option<Vec<usize>> {
    let pid = std::process::id();
    if cfg!(target_os = "linux") {
        if let Some(cores) = linux_get_affinity(pid) {
            return Some(cores);
        }
    } else if cfg!(target_os = "windows") {
        let mask = win_get_mask(pid);
        if mask > 0 {
            return Some(mask_to_cores(mask));
        }
    }
    let n = std::thread::available_parallelism().ok()?.get();
    Some((0..n).collect())
}

pub fn host_core_count() -> usize {
    *HOST_CORE_COUNT.get_or_init(|| {
        host_cores()
            .map(|c| c.len())
            .filter(|&n| n > 0)
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(1)
            })
    })
}

pub fn pick(cores: &[usize], count: usize) -> Vec<usize> {
    if cores.is_empty() || count == 0 {
        return Vec::new();
    }
    let mut rng = crate::pycompat::PyRandom::new(None);
    let start = (rng.random() * cores.len() as f64) as usize % cores.len();
    let mut out: Vec<usize> = cores[start..]
        .iter()
        .chain(cores[..start].iter())
        .take(count)
        .copied()
        .collect();
    out.sort_unstable();
    out
}

pub fn pin(pid: u32, count: usize) -> Option<Vec<usize>> {
    if count < 1 || !supported() {
        return None;
    }
    if pid == std::process::id() {
        let _ = host_core_count();
    }
    if cfg!(target_os = "linux") {
        let before = linux_get_affinity(pid)?;
        if count >= before.len() {
            return None;
        }
        let chosen = pick(&before, count);
        if linux_set_affinity(pid, &chosen) {
            Some(before)
        } else {
            None
        }
    } else if cfg!(target_os = "windows") {
        let mask = win_get_mask(pid);
        if mask == 0 {
            return None;
        }
        let before = mask_to_cores(mask);
        if count >= before.len() {
            return None;
        }
        let chosen = pick(&before, count);
        if win_set_mask(pid, cores_to_mask(&chosen)) {
            Some(before)
        } else {
            None
        }
    } else {
        None
    }
}

pub fn restore(pid: u32, previous: Option<&[usize]>) {
    let Some(prev) = previous.filter(|s| !s.is_empty()) else {
        return;
    };
    if cfg!(target_os = "linux") {
        let _ = linux_set_affinity(pid, prev);
    } else if cfg!(target_os = "windows") {
        let _ = win_set_mask(pid, cores_to_mask(prev));
    }
}

pub async fn with_pin_lock<F, Fut, T>(f: F) -> T
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    EVER_PINNED.store(true, Ordering::SeqCst);
    let _guard = pin_mutex().lock().await;
    f().await
}

pub async fn with_unpinned_launch<F, Fut, T>(f: F) -> T
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    if EVER_PINNED.load(Ordering::SeqCst) {
        let _guard = pin_mutex().lock().await;
        f().await
    } else {
        f().await
    }
}
