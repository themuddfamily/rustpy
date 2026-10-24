//! Process memory housekeeping for the IDE (glibc only; no-ops elsewhere).
//!
//! glibc's allocator keeps freed memory in per-thread arenas, and its dynamic
//! mmap threshold grows to 32 MB once large blocks are freed, after which the
//! tables of index builds and other background jobs are carved from those
//! arenas too. Their pages stay resident after the job ends: indexing a
//! 100k-file project left the IDE at about four times its live heap.
//! [`tune_allocator`] keeps large blocks in their own mappings (returned to
//! the system when freed) and [`release_free_memory`] hands the free pages of
//! the arenas back after bursts of temporary allocations.

/// Blocks at least this large get their own mapping. A fixed value also turns
/// off glibc's dynamic threshold, which would otherwise grow up to 32 MB.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
const MMAP_THRESHOLD: libc::c_int = 128 * 1024;

/// Sets the allocator options of a long-running IDE process. Call it early,
/// before threads allocate.
pub fn tune_allocator() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: mallopt only changes allocator parameters.
    unsafe {
        libc::mallopt(libc::M_MMAP_THRESHOLD, MMAP_THRESHOLD);
    }
}

/// Returns free allocator pages to the system (after a large job freed its
/// temporary data). Takes milliseconds on a big heap: call it off the UI thread.
pub fn release_free_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: malloc_trim only releases memory the allocator holds unused.
    unsafe {
        libc::malloc_trim(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn housekeeping_keeps_live_allocations_intact() {
        tune_allocator();
        let big: Vec<u64> = (0..1_000_000).collect();
        let small: Vec<Box<[u8; 64]>> = (0..10_000).map(|i| Box::new([i as u8; 64])).collect();
        drop((0..10_000).map(|_| vec![0u8; 512]).collect::<Vec<_>>());
        release_free_memory();
        assert_eq!(big.iter().sum::<u64>(), 999_999 * 1_000_000 / 2);
        assert!(small.iter().enumerate().all(|(i, b)| b[63] == i as u8));
    }
}
