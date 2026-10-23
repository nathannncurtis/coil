//! Virtual-memory primitives: 4 MiB segments reserved up front and 64 KiB
//! pages committed on demand — `VirtualAlloc` on Windows, `mmap`/`mprotect`
//! on Unix (spec section 9.1).

/// Segments are reserved and sized in multiples of 4 MiB.
pub const SEGMENT_SIZE: usize = 4 * 1024 * 1024;

/// Pages are committed in 64 KiB units.
pub const COMMIT_PAGE: usize = 64 * 1024;

/// Reserve `size` bytes (rounded up to a segment multiple) with no access,
/// and return a base aligned to `SEGMENT_SIZE`. The reservation is padded
/// by one segment so an aligned interior exists; the padding stays reserved
/// for the process lifetime. Returns null on failure.
pub(crate) fn reserve(size: usize) -> *mut u8 {
    let size = (size + SEGMENT_SIZE - 1) & !(SEGMENT_SIZE - 1);
    imp::reserve(size)
}

/// Give `[base, base + len)` read/write access. `len` must be a `COMMIT_PAGE`
/// multiple and the range must lie within one reservation. Freshly committed
/// pages read as zero on both families.
pub(crate) fn commit(base: *mut u8, len: usize) -> bool {
    debug_assert!(len.is_multiple_of(COMMIT_PAGE));
    imp::commit(base, len)
}

#[cfg(windows)]
mod imp {
    use core::ffi::c_void;

    use super::SEGMENT_SIZE;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn VirtualAlloc(
            lpAddress: *mut c_void,
            dwSize: usize,
            flAllocationType: u32,
            flProtect: u32,
        ) -> *mut c_void;
    }

    const MEM_RESERVE: u32 = 0x0000_2000;
    const MEM_COMMIT: u32 = 0x0000_1000;
    const PAGE_NOACCESS: u32 = 0x01;
    const PAGE_READWRITE: u32 = 0x04;

    pub(super) fn reserve(size: usize) -> *mut u8 {
        let total = size + SEGMENT_SIZE;
        // SAFETY: VirtualAlloc with MEM_RESERVE only reserves address space;
        // a null return means failure and touching the mapping is impossible.
        let raw = unsafe { VirtualAlloc(core::ptr::null_mut(), total, MEM_RESERVE, PAGE_NOACCESS) };
        if raw.is_null() {
            return core::ptr::null_mut();
        }
        let base = (raw as usize + SEGMENT_SIZE - 1) & !(SEGMENT_SIZE - 1);
        base as *mut u8
    }

    pub(super) fn commit(base: *mut u8, len: usize) -> bool {
        // SAFETY: the range lies inside our reservation; MEM_COMMIT on a
        // reserved range hands back zeroed pages or null.
        !unsafe { VirtualAlloc(base.cast::<c_void>(), len, MEM_COMMIT, PAGE_READWRITE) }.is_null()
    }
}

#[cfg(unix)]
mod imp {
    use core::ffi::c_void;

    #[cfg(target_os = "macos")]
    const MAP_ANON: i32 = 0x1000;
    #[cfg(not(target_os = "macos"))]
    const MAP_ANON: i32 = 0x0020;
    #[cfg(target_os = "macos")]
    const MAP_NORESERVE: i32 = 0x0040;
    #[cfg(not(target_os = "macos"))]
    const MAP_NORESERVE: i32 = 0x4000;
    const MAP_PRIVATE: i32 = 0x0002;
    const PROT_NONE: i32 = 0;
    const PROT_READ: i32 = 1;
    const PROT_WRITE: i32 = 2;

    use super::SEGMENT_SIZE;

    unsafe extern "C" {
        fn mmap(
            addr: *mut c_void,
            len: usize,
            prot: i32,
            flags: i32,
            fd: i32,
            offset: i64,
        ) -> *mut c_void;
        fn mprotect(addr: *mut c_void, len: usize, prot: i32) -> i32;
    }

    pub(super) fn reserve(size: usize) -> *mut u8 {
        let total = size + SEGMENT_SIZE;
        // SAFETY: anonymous mmap cannot fault; MAP_FAILED ((void*)-1) is the
        // only failure report and the mapping is never touched while PROT_NONE.
        let raw = unsafe {
            mmap(
                core::ptr::null_mut(),
                total,
                PROT_NONE,
                MAP_PRIVATE | MAP_ANON | MAP_NORESERVE,
                -1,
                0,
            )
        };
        if raw as usize == usize::MAX {
            return core::ptr::null_mut();
        }
        let base = (raw as usize + SEGMENT_SIZE - 1) & !(SEGMENT_SIZE - 1);
        base as *mut u8
    }

    pub(super) fn commit(base: *mut u8, len: usize) -> bool {
        // SAFETY: the range lies inside our reservation; mprotect on
        // untouched anonymous pages yields zero-filled memory.
        unsafe { mprotect(base.cast::<c_void>(), len, PROT_READ | PROT_WRITE) == 0 }
    }
}

#[cfg(not(any(windows, unix)))]
compile_error!("coil-rt alloc supports Windows and Unix only");
