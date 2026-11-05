//! Peak resident set size of a finished child process, measured the same way for the Rust
//! and the Java side: Windows `PeakWorkingSetSize` (`K32GetProcessMemoryInfo` on the child's
//! handle) and Linux `ru_maxrss` from `getrusage(RUSAGE_CHILDREN)`.

use std::process::Child;

/// Peak RSS in KiB of `child`, which must have been waited for. `None` when unsupported.
#[cfg(windows)]
pub fn peak_rss_kb(child: &Child) -> Option<u64> {
    use std::os::windows::io::AsRawHandle;

    #[repr(C)]
    #[derive(Default)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn K32GetProcessMemoryInfo(process: *mut std::ffi::c_void, counters: *mut ProcessMemoryCounters, cb: u32) -> i32;
    }
    let mut c = ProcessMemoryCounters { cb: std::mem::size_of::<ProcessMemoryCounters>() as u32, ..Default::default() };
    // SAFETY: the handle belongs to `child` and stays open until it is dropped; `c` is a
    // correctly sized PROCESS_MEMORY_COUNTERS.
    let ok = unsafe { K32GetProcessMemoryInfo(child.as_raw_handle() as *mut _, &mut c, c.cb) };
    (ok != 0).then(|| c.peak_working_set_size as u64 / 1024)
}

/// Peak RSS in KiB of the largest waited-for child (one child per `measure` run).
#[cfg(target_os = "linux")]
pub fn peak_rss_kb(_child: &Child) -> Option<u64> {
    extern "C" {
        fn getrusage(who: i32, usage: *mut i64) -> i32;
    }
    const RUSAGE_CHILDREN: i32 = -1;
    // struct rusage on 64-bit Linux: two timevals (4 x i64) then 14 longs; ru_maxrss is the
    // first long (KiB).
    let mut buf = [0i64; 18];
    // SAFETY: `buf` is at least as large as struct rusage.
    let ok = unsafe { getrusage(RUSAGE_CHILDREN, buf.as_mut_ptr()) };
    (ok == 0).then(|| buf[4] as u64)
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn peak_rss_kb(_child: &Child) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(any(windows, target_os = "linux"))]
    fn measures_a_child() {
        let exe = std::env::current_exe().unwrap();
        // run the test binary itself with a filter that matches nothing
        let mut child = std::process::Command::new(exe)
            .arg("--exact")
            .arg("no-such-test")
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        child.wait().unwrap();
        let kb = super::peak_rss_kb(&child).expect("peak rss");
        assert!(kb > 100 && kb < 4 * 1024 * 1024, "{kb}");
    }
}
