//! Compiled-code registration and the frame walk of section 9.4's runtime
//! side: every compiled image registers its code units with their
//! stack-map tables (`coil_rt_register_code`, the format of abi.toml's
//! `[formats.stackmap]`), and the collector walks the rbp chain of every
//! thread's recorded top frame, marking the reference slots each return
//! address's bitmap names. Compiled code does not exist yet; the API is
//! the contract and synthetic frames exercise it.

use core::ffi::c_void;
use std::sync::{Mutex, OnceLock};

use coil_capi_sys::PyObject;

use crate::gc::mark::MarkState;
use crate::tstate;

/// The compiled-frame layout of abi.toml's `[offsets.compiled_frame]`:
/// saved rbp and return address above rbp, the function descriptor and the
/// reference slots stepping down from it.
pub const SAVED_RBP: isize = 0;
pub const RETURN_ADDRESS: isize = 8;
pub const FUNCTION_DESCRIPTOR: isize = -8;
pub const SLOT_BASE: isize = -16;
pub const SLOT_STRIDE: isize = 8;

/// `0x504D5343`, the `CSMP` magic of the stack-map format.
const MAGIC: u32 = 0x504D_5343;
/// The only stack-map format version this runtime reads.
const FORMAT_VERSION: u32 = 1;
/// The largest stack-map table a unit may carry; a malformed header that
/// would drive the parse past this is rejected instead of walked off the
/// mapping.
const MAX_TABLE_BYTES: usize = 32 * 1024 * 1024;

/// One registered code unit: its address range and the parsed stack-map
/// entries, sorted by return address as the format requires.
struct CodeUnit {
    start: usize,
    end: usize,
    entries: Vec<MapEntry>,
}

#[derive(Debug, PartialEq)]
struct MapEntry {
    ret_addr: usize,
    nslots: u32,
    bitmap: Vec<u64>,
}

fn units() -> &'static Mutex<Vec<CodeUnit>> {
    static UNITS: OnceLock<Mutex<Vec<CodeUnit>>> = OnceLock::new();
    UNITS.get_or_init(|| Mutex::new(Vec::new()))
}

/// A little-endian, packed reader over a stack-map table, bounded by
/// `MAX_TABLE_BYTES`; `None` on any truncation.
///
/// # Safety
///
/// `base` must address a stack-map table the compiler emitted; the bound
/// keeps the cursor inside the mapping that holds the table.
struct Reader {
    base: usize,
    pos: usize,
}

impl Reader {
    /// SAFETY: see the struct.
    unsafe fn new(base: *const c_void) -> Reader {
        Reader {
            base: base as usize,
            pos: 0,
        }
    }

    fn u32(&mut self) -> Option<u32> {
        if self.pos + 4 > MAX_TABLE_BYTES {
            return None;
        }
        // SAFETY: the cursor is inside the table's mapping.
        let b = unsafe { core::ptr::read_unaligned((self.base + self.pos) as *const [u8; 4]) };
        self.pos += 4;
        Some(u32::from_le_bytes(b))
    }

    fn u64(&mut self) -> Option<u64> {
        if self.pos + 8 > MAX_TABLE_BYTES {
            return None;
        }
        // SAFETY: the cursor is inside the table's mapping.
        let b = unsafe { core::ptr::read_unaligned((self.base + self.pos) as *const [u8; 8]) };
        self.pos += 8;
        Some(u64::from_le_bytes(b))
    }
}

/// Parse and validate a stack-map table (`abi.toml` `[formats.stackmap]`).
/// `Err` carries the field that failed. The entries' return addresses must
/// lie in `[code_start, code_end)` and ascend.
///
/// # Safety
///
/// `base` must address the table; see `Reader`.
unsafe fn parse_table(
    base: *const c_void,
    code_start: usize,
    code_end: usize,
) -> Result<Vec<MapEntry>, &'static str> {
    let mut reader = unsafe { Reader::new(base) };
    if reader.u32() != Some(MAGIC) {
        return Err("magic");
    }
    if reader.u32() != Some(FORMAT_VERSION) {
        return Err("version");
    }
    let count = reader.u64().ok_or("entry_count")? as usize;
    let mut entries = Vec::new();
    let mut previous = 0usize;
    for i in 0..count {
        let ret_addr = reader.u64().ok_or("entry.ret_addr")? as usize;
        let nslots = reader.u32().ok_or("entry.nslots")?;
        let words = reader.u32().ok_or("entry.bitmap_words")? as usize;
        if words != nslots.div_ceil(64) as usize {
            return Err("entry.bitmap_words");
        }
        let mut bitmap = Vec::with_capacity(words);
        for _ in 0..words {
            bitmap.push(reader.u64().ok_or("entry.bitmap")?);
        }
        if ret_addr < code_start || ret_addr >= code_end {
            return Err("entry.ret_addr range");
        }
        if i > 0 && ret_addr <= previous {
            return Err("entry.ret_addr order");
        }
        previous = ret_addr;
        entries.push(MapEntry {
            ret_addr,
            nslots,
            bitmap,
        });
    }
    Ok(entries)
}

/// `coil_rt_register_code` (abi.toml `[functions]`): register one code
/// unit with its stack maps. Zero on success; -1 with `ValueError` set on
/// a malformed table.
///
/// # Safety
///
/// `code_start..code_end` must address the unit's emitted code and
/// `stack_maps` the unit's stack-map table in abi.toml's
/// `[formats.stackmap]`, both kept for the process lifetime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_rt_register_code(
    code_start: *const c_void,
    code_end: *const c_void,
    stack_maps: *const c_void,
) -> core::ffi::c_int {
    let start = code_start as usize;
    let end = code_end as usize;
    if start >= end || stack_maps.is_null() {
        crate::tstate::set_string(
            crate::types::exc::value_error(),
            b"coil_rt_register_code: empty code range",
        );
        return -1;
    }
    // SAFETY: the caller — the compiled image's startup — hands over the
    // table the compiler emitted; the parse validates every field against
    // the format and its size cap.
    match unsafe { parse_table(stack_maps, start, end) } {
        Ok(entries) => {
            units()
                .lock()
                .expect("code registry poisoned")
                .push(CodeUnit {
                    start,
                    end,
                    entries,
                });
            0
        }
        Err(field) => {
            let message = format!("coil_rt_register_code: bad stack-map table field {field}");
            crate::tstate::set_string(crate::types::exc::value_error(), message.as_bytes());
            -1
        }
    }
}

/// `coil_rt_push_frame` (abi.toml `[functions]`): record the current
/// thread's top compiled frame — the anchor a stack walk starts from.
/// Compiled code's entry trampolines call this on every runtime entry that
/// may allocate; the value stays recorded for the detached-thread roots of
/// section 9.2 when the thread drops into C. Re-entry through C (a
/// `tp_call` into more compiled code, a comparison callback) nests, so
/// each entry pushes rather than overwrites; the anchors chain exactly
/// like CPython's `tstate->frame`.
///
/// # Safety
///
/// `frame` must be null or the calling compiled frame's rbp value, a
/// frame that is live on this thread's stack, and the matching
/// `coil_rt_pop_frame` must run while it still is.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_rt_push_frame(frame: *mut c_void) {
    // SAFETY: `current` returns this thread's own state.
    let state = tstate::current();
    // SAFETY: the state is this thread's own; the frame pointer is read
    // only at a collection, and with the GIL the collecting thread is the
    // only attached thread.
    unsafe {
        if !(*state).frame.is_null() {
            (*state).frame_parents.push((*state).frame as usize);
        }
        (*state).frame = frame.cast();
    }
}

/// `coil_rt_pop_frame` (abi.toml `[functions]`): drop the anchor the
/// matching `coil_rt_push_frame` recorded, restoring the one below it.
///
/// # Safety
///
/// Only the thread that pushed the anchor may pop it, and only once per
/// push.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_rt_pop_frame() {
    // SAFETY: `current` returns this thread's own state.
    let state = tstate::current();
    // SAFETY: the state is this thread's own, and the matching push put
    // the parent anchor there.
    unsafe {
        (*state).frame = (*state)
            .frame_parents
            .pop()
            .map_or(core::ptr::null_mut(), |parent| parent as *mut PyObject);
    }
}

/// Visit the reference slots the stack map for `ret_addr` names in the
/// frame at `rbp`. False when no registered unit covers `ret_addr` — the
/// walk's stop condition.
fn visit_slots(ret_addr: usize, rbp: usize, visit: &mut dyn FnMut(*mut PyObject)) -> bool {
    let units = units().lock().expect("code registry poisoned");
    let Some(unit) = units
        .iter()
        .find(|unit| ret_addr >= unit.start && ret_addr < unit.end)
    else {
        return false;
    };
    // The entries ascend by ret_addr, so the partition point is the entry.
    let index = unit.entries.partition_point(|e| e.ret_addr < ret_addr);
    let Some(map) = unit.entries.get(index).filter(|e| e.ret_addr == ret_addr) else {
        return false;
    };
    for slot in 0..map.nslots as usize {
        if map.bitmap[slot / 64] >> (slot % 64) & 1 == 0 {
            continue;
        }
        // Slot i sits at `rbp + slot_base - i * slot_stride`: the slots
        // descend from `rbp - 16` (section 9.4's frame layout).
        let addr = (rbp as isize + SLOT_BASE - SLOT_STRIDE * slot as isize) as usize;
        // SAFETY: the stack map names this slot as holding a live
        // PyObject* at this safepoint, and the frame is live on a stack.
        let value = unsafe { core::ptr::read_volatile(addr as *const *mut PyObject) };
        if !value.is_null() {
            visit(value);
        }
    }
    true
}

/// Walk the frame chain anchored at `rbp`, handing every reference slot
/// the stack maps name to `visit` (section 9.4: every live reference is
/// in a frame slot at a safepoint, and `rbp` is always kept). The walk
/// stops at the first frame whose return address no unit covers, at a
/// null or unaligned `rbp`, a chain that does not climb, or after
/// `MAX_FRAMES` frames — a runaway chain must not hang the collector on
/// a corrupted stack.
///
/// # Safety
///
/// `rbp` must be null or a saved-rbp value of a compiled frame that is
/// live on some thread's stack, whose chain is well-formed.
pub(crate) unsafe fn walk_frames(rbp: usize, visit: &mut dyn FnMut(*mut PyObject)) {
    const MAX_FRAMES: usize = 64 * 1024;
    let mut rbp = rbp;
    for _ in 0..MAX_FRAMES {
        if rbp == 0 || rbp & 7 != 0 {
            return;
        }
        // SAFETY: the frame is live on a stack; the return address and the
        // caller's rbp are the two words above it.
        let ret_addr = unsafe { core::ptr::read_volatile((rbp as *const usize).offset(1)) };
        let caller = unsafe { core::ptr::read_volatile(rbp as *const usize) };
        if !visit_slots(ret_addr, rbp, visit) {
            return;
        }
        if caller <= rbp {
            // The chain must climb the stack; anything else is corruption.
            return;
        }
        rbp = caller;
    }
}

/// `walk_frames` as the collector's marking pass.
///
/// # Safety
///
/// As `walk_frames`.
pub(crate) unsafe fn mark_frames(rbp: usize, marks: &mut MarkState) {
    // SAFETY: as `walk_frames`; the visitor only marks.
    unsafe {
        walk_frames(rbp, &mut |value| marks.mark(value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal valid table for one entry.
    fn table(ret_addr: u64, nslots: u32, bitmap: &[u64]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC.to_le_bytes());
        bytes.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        bytes.extend_from_slice(&1u64.to_le_bytes());
        bytes.extend_from_slice(&ret_addr.to_le_bytes());
        bytes.extend_from_slice(&nslots.to_le_bytes());
        bytes.extend_from_slice(&(bitmap.len() as u32).to_le_bytes());
        for word in bitmap {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes
    }

    // The reader walks the caller's buffer; these tests only hand it
    // buffers they own.
    #[test]
    fn parses_a_well_formed_table() {
        let code = [0u8; 64];
        let start = code.as_ptr() as usize;
        let table = table(start as u64 + 16, 70, &[0xFFFF_FFFF_FFFF_FFFF, 0b111]);
        // SAFETY: the buffer is a valid table of exactly its bytes.
        let entries = unsafe { parse_table(table.as_ptr() as *const c_void, start, start + 64) };
        assert!(entries.is_ok());
        let entries = entries.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].ret_addr, start + 16);
        assert_eq!(entries[0].nslots, 70);
        assert_eq!(entries[0].bitmap, vec![u64::MAX, 0b111]);
    }

    #[test]
    fn rejects_bad_magic_and_range() {
        let code = [0u8; 64];
        let start = code.as_ptr() as usize;
        let mut bad = table(start as u64, 1, &[1]);
        bad[0] ^= 0xFF;
        // SAFETY: as above, the buffer backs the table.
        assert_eq!(
            unsafe { parse_table(bad.as_ptr() as *const c_void, start, start + 64) }.unwrap_err(),
            "magic"
        );
        // A return address outside the unit's code range is rejected.
        let outside = table(start as u64 + 1000, 1, &[1]);
        // SAFETY: as above.
        assert_eq!(
            unsafe { parse_table(outside.as_ptr() as *const c_void, start, start + 64) }
                .unwrap_err(),
            "entry.ret_addr range"
        );
    }
}
