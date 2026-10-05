//! Platform reads only: procfs, sysctl/Mach and statvfs through fs2.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use super::{DiskSample, HostResourceProbe, HostResourceSample};

#[derive(Default)]
pub(super) struct NativeProbe {
    cache: Mutex<Option<NativeCache>>,
    #[cfg(target_os = "macos")]
    cpu_ticks: Mutex<Option<(Instant, [u32; 4])>>,
}

struct NativeCache {
    at: Instant,
    sampled_at: chrono::DateTime<chrono::Utc>,
    cpu_percent: Option<f64>,
    memory_percent: Option<f64>,
    disks: BTreeMap<PathBuf, Option<f64>>,
}

impl HostResourceProbe for NativeProbe {
    fn sample(&self, paths: &[PathBuf]) -> HostResourceSample {
        // CPU/memory are shared even when many workspaces request different
        // disk paths. Bound disk memo size as well as its lifetime.
        let mut guard = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        if guard
            .as_ref()
            .is_none_or(|cache| cache.at.elapsed() >= super::RESOURCE_CACHE_TTL)
        {
            let (cpu_percent, memory_percent) = self.cpu_memory();
            *guard = Some(NativeCache {
                at: Instant::now(),
                sampled_at: chrono::Utc::now(),
                cpu_percent,
                memory_percent,
                disks: BTreeMap::new(),
            });
        }
        let Some(cache) = guard.as_mut() else {
            return HostResourceSample {
                sampled_at: chrono::Utc::now(),
                cpu_percent: None,
                memory_percent: None,
                disks: paths
                    .iter()
                    .map(|path| DiskSample {
                        path: path.clone(),
                        used_percent: None,
                    })
                    .collect(),
            };
        };
        let disks = paths
            .iter()
            .map(|path| {
                if !cache.disks.contains_key(path) {
                    if cache.disks.len() >= 256 {
                        cache.disks.pop_first();
                    }
                    cache.disks.insert(path.clone(), disk_percent(path));
                }
                DiskSample {
                    path: path.clone(),
                    used_percent: cache.disks.get(path).copied().flatten(),
                }
            })
            .collect();
        HostResourceSample {
            sampled_at: cache.sampled_at,
            cpu_percent: cache.cpu_percent,
            memory_percent: cache.memory_percent,
            disks,
        }
    }
}

fn disk_percent(path: &Path) -> Option<f64> {
    // The worktrees directory may not exist yet; use its nearest existing
    // ancestor's filesystem without creating anything. Permission/I/O failures
    // are unknown rather than being disguised by a fallback.
    let mut existing = path;
    while let Err(error) = existing.metadata() {
        if error.kind() != std::io::ErrorKind::NotFound {
            return None;
        }
        existing = existing.parent()?;
    }
    let stat = fs2::statvfs(existing).ok()?;
    let total = stat.total_space();
    (total > 0).then(|| 100.0 * total.saturating_sub(stat.available_space()) as f64 / total as f64)
}

#[cfg(target_os = "linux")]
impl NativeProbe {
    fn cpu_memory(&self) -> (Option<f64>, Option<f64>) {
        fn read(path: &str) -> Option<String> {
            use std::io::Read;
            let mut data = String::new();
            std::fs::File::open(path)
                .ok()?
                .take(65536)
                .read_to_string(&mut data)
                .ok()?;
            Some(data)
        }
        let cpu = read("/proc/loadavg").and_then(|data| {
            let load: f64 = data.split_whitespace().next()?.parse().ok()?;
            // SAFETY: sysconf with this constant has no pointer arguments. Use
            // all online host CPUs, not the caller's affinity/cgroup budget.
            let cores = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
            (cores > 0 && load.is_finite() && load >= 0.0).then(|| 100.0 * load / cores as f64)
        });
        let memory = read("/proc/meminfo").and_then(|data| {
            let mut total = None;
            let mut available = None;
            for line in data.lines() {
                let mut parts = line.split_whitespace();
                let key = parts.next()?;
                if key == "MemTotal:" || key == "MemAvailable:" {
                    let value = parts.next()?.parse::<u64>().ok()?;
                    if parts.next() != Some("kB") {
                        return None;
                    }
                    if key == "MemTotal:" {
                        total = Some(value);
                    } else {
                        available = Some(value);
                    }
                }
            }
            let (total, available) = (total?, available?);
            (total > 0 && available <= total)
                .then(|| 100.0 * (total - available) as f64 / total as f64)
        });
        (cpu, memory)
    }
}

#[cfg(target_os = "macos")]
impl NativeProbe {
    // libc recommends mach2 for these bindings; the underlying Mach APIs remain
    // supported. Keep the existing dependency for this small native collector.
    #[allow(deprecated)]
    fn cpu_memory(&self) -> (Option<f64>, Option<f64>) {
        use std::mem::{MaybeUninit, size_of};
        // libc exposes the statistics calls but omits this libSystem symbol.
        // The signature matches the macOS SDK's mach/mach_port.h declaration.
        unsafe extern "C" {
            fn mach_port_deallocate(
                task: libc::mach_port_t,
                name: libc::mach_port_t,
            ) -> libc::kern_return_t;
        }
        // SAFETY: mach_host_self returns a send right owned by this call. Both
        // statistics buffers use the libc ABI with counts in integer_t units.
        let host = unsafe { libc::mach_host_self() };
        let mut cpu = MaybeUninit::<libc::host_cpu_load_info>::zeroed();
        let mut cpu_count = libc::HOST_CPU_LOAD_INFO_COUNT;
        let cpu_result = unsafe {
            libc::host_statistics(
                host,
                libc::HOST_CPU_LOAD_INFO,
                cpu.as_mut_ptr().cast(),
                &mut cpu_count,
            )
        };
        let cpu_percent = if cpu_result == libc::KERN_SUCCESS
            && cpu_count == libc::HOST_CPU_LOAD_INFO_COUNT
        {
            // SAFETY: host_statistics initialized the full CPU buffer on success.
            let ticks = unsafe { cpu.assume_init() }.cpu_ticks;
            let mut previous = self
                .cpu_ticks
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let value = previous.and_then(|(at, old)| {
                // A long idle/error gap cannot supply a short CPU window.
                if at.elapsed() > super::RESOURCE_MAX_AGE {
                    return None;
                }
                let delta =
                    std::array::from_fn::<_, 4, _>(|i| u64::from(ticks[i].wrapping_sub(old[i])));
                let total: u64 = delta.iter().sum();
                (total > 0).then(|| {
                    100.0 * (total - delta[libc::CPU_STATE_IDLE as usize]) as f64 / total as f64
                })
            });
            *previous = Some((Instant::now(), ticks));
            value
        } else {
            *self
                .cpu_ticks
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = None;
            None
        };
        let mut vm = MaybeUninit::<libc::vm_statistics64>::zeroed();
        let mut vm_count = libc::HOST_VM_INFO64_COUNT;
        // SAFETY: same ABI/count guarantee as CPU stats above.
        let vm_result = unsafe {
            libc::host_statistics64(
                host,
                libc::HOST_VM_INFO64,
                vm.as_mut_ptr().cast(),
                &mut vm_count,
            )
        };
        let mut total: u64 = 0;
        let mut length = size_of::<u64>();
        // SAFETY: NUL-terminated name, correctly sized output, no new value.
        let total_result = unsafe {
            libc::sysctlbyname(
                c"hw.memsize".as_ptr(),
                (&mut total as *mut u64).cast(),
                &mut length,
                std::ptr::null_mut(),
                0,
            )
        };
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        let memory_percent = if vm_result == libc::KERN_SUCCESS
            && vm_count == libc::HOST_VM_INFO64_COUNT
            && total_result == 0
            && length == size_of::<u64>()
            && total > 0
            && page_size > 0
        {
            // SAFETY: host_statistics64 returned all requested fields.
            let vm = unsafe { vm.assume_init() };
            // Free includes speculative pages; inactive is reclaimable. Do not
            // add purgeable separately (it overlaps active/inactive pages).
            let available = (u64::from(vm.free_count) + u64::from(vm.inactive_count))
                .saturating_mul(page_size as u64)
                .min(total);
            Some(100.0 * (total - available) as f64 / total as f64)
        } else {
            None
        };
        // SAFETY: release the send right acquired by mach_host_self each sample.
        unsafe {
            mach_port_deallocate(libc::mach_task_self(), host);
        }
        (cpu_percent, memory_percent)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl NativeProbe {
    fn cpu_memory(&self) -> (Option<f64>, Option<f64>) {
        (None, None)
    }
}
