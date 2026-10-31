//! The `_sre` module (spec section 10.6) over RustPython's `sre-engine`
//! 0.4.3: the compiled-code objects `Lib/re` drives — `Pattern`, `Match`,
//! `Scanner` and the replacement `Template` — with CPython 3.12's method
//! cores, the module functions and the module constants `Lib/re`
//! asserts on import. The engine consumes the same u32 code array
//! `_compiler.py` emits, so `Lib/re` compiles unchanged against this
//! surface once method dispatch exists; today the cores are the C API
//! layer the tests drive, and the paths that must call back into Python
//! (replacement templates with escapes, replacement callables, the
//! finditer call-iterator) raise typed errors naming what they need.

use core::ffi::c_int;

use coil_capi_sys::{
    Py_TPFLAGS_DEFAULT, Py_TPFLAGS_DISALLOW_INSTANTIATION, Py_TPFLAGS_IMMUTABLETYPE, Py_buffer,
    PyObject, PyTypeObject, PyVarObject, c_long,
};
use sre_engine::engine::{Request, State, StrDrive};

use crate::object::header;
use crate::object::typeobject::{PyType_GenericAlloc, zero_type};
use crate::tstate;
use crate::types::exc as exceptions;
use crate::types::unicode;
use crate::types::{bytes as rt_bytes, dict, list, long, tuple};
use crate::util;

// ------------------------------------------------------------ constants

/// `_sre.MAGIC`: the value `Lib/re/_constants.py` asserts on import.
pub const MAGIC: c_int = 20221023;

/// `_sre.CODESIZE`: `sizeof(SRE_CODE)`, and `Py_UCS4` in 3.12.
pub const CODESIZE: c_int = 4;

/// `_sre.MAXREPEAT`: `~(Py_UCS4)0` on every build with size_t > 4.
pub const MAXREPEAT: u32 = u32::MAX;

/// `_sre.MAXGROUPS`: `INT32_MAX / 2`.
pub const MAXGROUPS: u32 = c_int::MAX as u32 / 2;

/// `_sre.copyright`.
pub const COPYRIGHT: &[u8] = b" SRE 2.2.2 Copyright (c) 1997-2002 by Secret Labs AB ";

// The opcode numbers of `Lib/re/_constants.py`, the ABI `_compiler.py`
// emits; the code walk validates against them.
const OP_FAILURE: u32 = 0;
const OP_SUCCESS: u32 = 1;
const OP_ANY: u32 = 2;
const OP_ANY_ALL: u32 = 3;
const OP_ASSERT: u32 = 4;
const OP_ASSERT_NOT: u32 = 5;
const OP_AT: u32 = 6;
const OP_BRANCH: u32 = 7;
const OP_CATEGORY: u32 = 8;
const OP_CHARSET: u32 = 9;
const OP_BIGCHARSET: u32 = 10;
const OP_GROUPREF: u32 = 11;
const OP_GROUPREF_EXISTS: u32 = 12;
const OP_IN: u32 = 13;
const OP_INFO: u32 = 14;
const OP_JUMP: u32 = 15;
const OP_LITERAL: u32 = 16;
const OP_MARK: u32 = 17;
const OP_MAX_UNTIL: u32 = 18;
const OP_MIN_UNTIL: u32 = 19;
const OP_NOT_LITERAL: u32 = 20;
const OP_NEGATE: u32 = 21;
const OP_RANGE: u32 = 22;
const OP_REPEAT: u32 = 23;
const OP_REPEAT_ONE: u32 = 24;
const OP_MIN_REPEAT_ONE: u32 = 26;
const OP_ATOMIC_GROUP: u32 = 27;
const OP_POSSESSIVE_REPEAT: u32 = 28;
const OP_POSSESSIVE_REPEAT_ONE: u32 = 29;

const OP_LITERAL_IGNORE: u32 = 32;
const OP_LITERAL_UNI_IGNORE: u32 = 40;
const OP_LITERAL_LOC_IGNORE: u32 = 36;
const OP_NOT_LITERAL_IGNORE: u32 = 33;
const OP_NOT_LITERAL_UNI_IGNORE: u32 = 41;
const OP_NOT_LITERAL_LOC_IGNORE: u32 = 37;
const OP_GROUPREF_IGNORE: u32 = 30;
const OP_GROUPREF_UNI_IGNORE: u32 = 38;
const OP_GROUPREF_LOC_IGNORE: u32 = 34;
const OP_IN_IGNORE: u32 = 31;
const OP_IN_UNI_IGNORE: u32 = 39;
const OP_IN_LOC_IGNORE: u32 = 35;
const OP_RANGE_UNI_IGNORE: u32 = 42;

/// `Lib/re/_constants.py` opcode numbers into sre-engine 0.4.3's: the
/// engine predates 3.12's renumbering (it still carries the `CALL`
/// opcode 3.12 dropped, and lacks the possessive repeats). Operand
/// words keep their values; only opcode positions translate. `None`
/// marks the repeats the engine does not implement.
const ENGINE_OPS: [Option<u32>; 43] = [
    Some(0),  // FAILURE
    Some(1),  // SUCCESS
    Some(2),  // ANY
    Some(3),  // ANY_ALL
    Some(4),  // ASSERT
    Some(5),  // ASSERT_NOT
    Some(6),  // AT
    Some(7),  // BRANCH
    Some(9),  // CATEGORY
    Some(10), // CHARSET
    Some(11), // BIGCHARSET
    Some(12), // GROUPREF
    Some(13), // GROUPREF_EXISTS
    Some(14), // IN
    Some(15), // INFO
    Some(16), // JUMP
    Some(17), // LITERAL
    Some(18), // MARK
    Some(19), // MAX_UNTIL
    Some(20), // MIN_UNTIL
    Some(21), // NOT_LITERAL
    Some(22), // NEGATE
    Some(23), // RANGE
    Some(24), // REPEAT
    Some(25), // REPEAT_ONE
    Some(26), // SUBPATTERN (the 3.12 compiler never emits it)
    Some(27), // MIN_REPEAT_ONE
    None,     // ATOMIC_GROUP
    None,     // POSSESSIVE_REPEAT
    None,     // POSSESSIVE_REPEAT_ONE
    Some(28), // GROUPREF_IGNORE
    Some(29), // IN_IGNORE
    Some(30), // LITERAL_IGNORE
    Some(31), // NOT_LITERAL_IGNORE
    Some(32), // GROUPREF_LOC_IGNORE
    Some(33), // IN_LOC_IGNORE
    Some(34), // LITERAL_LOC_IGNORE
    Some(35), // NOT_LITERAL_LOC_IGNORE
    Some(36), // GROUPREF_UNI_IGNORE
    Some(37), // IN_UNI_IGNORE
    Some(38), // LITERAL_UNI_IGNORE
    Some(39), // NOT_LITERAL_UNI_IGNORE
    Some(40), // RANGE_UNI_IGNORE
];

/// The engine's number for a 3.12 opcode, if it has one.
fn engine_op(op: u32) -> Option<u32> {
    ENGINE_OPS.get(op as usize).copied().flatten()
}

const INFO_PREFIX: u32 = 1;
const INFO_LITERAL: u32 = 2;
const INFO_CHARSET: u32 = 4;

const AT_BEGINNING: u32 = 0;
const AT_BEGINNING_STRING: u32 = 2;
const AT_BEGINNING_LINE: u32 = 1;
const AT_END: u32 = 5;
const AT_END_LINE: u32 = 6;
const AT_END_STRING: u32 = 7;
const AT_BOUNDARY: u32 = 3;
const AT_NON_BOUNDARY: u32 = 4;
const AT_LOC_BOUNDARY: u32 = 8;
const AT_LOC_NON_BOUNDARY: u32 = 9;
const AT_UNI_BOUNDARY: u32 = 10;
const AT_UNI_NON_BOUNDARY: u32 = 11;

const CATEGORY_DIGIT: u32 = 0;
const CATEGORY_NOT_DIGIT: u32 = 1;
const CATEGORY_SPACE: u32 = 2;
const CATEGORY_NOT_SPACE: u32 = 3;
const CATEGORY_WORD: u32 = 4;
const CATEGORY_NOT_WORD: u32 = 5;
const CATEGORY_LINEBREAK: u32 = 6;
const CATEGORY_NOT_LINEBREAK: u32 = 7;
const CATEGORY_LOC_WORD: u32 = 8;
const CATEGORY_LOC_NOT_WORD: u32 = 9;
const CATEGORY_UNI_DIGIT: u32 = 10;
const CATEGORY_UNI_NOT_DIGIT: u32 = 11;
const CATEGORY_UNI_SPACE: u32 = 12;
const CATEGORY_UNI_NOT_SPACE: u32 = 13;
const CATEGORY_UNI_WORD: u32 = 14;
const CATEGORY_UNI_NOT_WORD: u32 = 15;
const CATEGORY_UNI_LINEBREAK: u32 = 16;
const CATEGORY_UNI_NOT_LINEBREAK: u32 = 17;

// ---------------------------------------------------------------- layouts

/// CPython 3.12's `PatternObject` (`Modules/_sre/sre.h`): the var-object
/// header's `ob_size` is the code-word count. The code tail holds the
/// words in sre-engine's opcode numbering; see the module docs.
#[repr(C)]
pub struct PatternObject {
    pub ob_base: PyVarObject,
    pub groups: isize,
    pub groupindex: *mut PyObject,
    pub indexgroup: *mut PyObject,
    pub pattern: *mut PyObject,
    pub flags: c_int,
    pub weakreflist: *mut PyObject,
    pub isbytes: c_int,
    pub codesize: isize,
    pub code: [u32; 1],
}

/// CPython 3.12's `MatchObject`; `mark` holds `2*(groups+1)` positions.
#[repr(C)]
pub struct MatchObject {
    pub ob_base: PyVarObject,
    pub string: *mut PyObject,
    pub regs: *mut PyObject,
    pub pattern: *mut PyObject,
    pub pos: isize,
    pub endpos: isize,
    pub lastindex: isize,
    pub groups: isize,
    pub mark: [isize; 1],
}

impl MatchObject {
    /// The mark slot `index` of the variable tail, CPython's
    /// `match->mark[index]`.
    ///
    /// # Safety
    ///
    /// `index` must be below `2 * groups`, the allocated tail.
    unsafe fn mark_at(&self, index: usize) -> isize {
        // SAFETY: the caller keeps the index within the tail.
        unsafe { *self.mark.as_ptr().add(index) }
    }
}

/// One replacement chunk: a group index and the literal that follows it
/// (null when the literal is empty).
#[repr(C)]
pub struct TemplateItem {
    index: isize,
    literal: *mut PyObject,
}

/// CPython 3.12's `TemplateObject`; `ob_size` is the item count.
#[repr(C)]
pub struct TemplateObject {
    pub ob_base: PyVarObject,
    pub chunks: isize,
    pub literal: *mut PyObject,
    pub items: [TemplateItem; 1],
}

/// The match state one entry point runs or, for a scanner, holds across
/// calls: the subject view, the search cursor and the engine's state.
struct MatchState {
    string: *mut PyObject,
    subject: Subject,
    /// The search cursor; CPython's `state->start`.
    start: usize,
    /// The clamped end; CPython's `state->end`.
    end: usize,
    pos: isize,
    endpos: isize,
    must_advance: bool,
    /// The scanner's `state->start == NULL`: no further runs.
    finished: bool,
    engine: State<Subject>,
}

/// CPython 3.12's `ScannerObject`: the pattern plus a persistent match
/// state. The bytes subject is copied (CPython pins the exporter's
/// buffer; see the module docs for the deviation), so the scanner owns
/// the copy's allocation.
#[repr(C)]
pub struct ScannerObject {
    pub ob_base: PyObject,
    pub pattern: *mut PyObject,
    state: MatchState,
    /// `NULL` for str subjects; otherwise `state.subject`'s buffer.
    bytes_copy: *mut u8,
}

unsafe extern "C" fn pattern_dealloc(object: *mut PyObject) {
    // SAFETY: the object came from `PyType_GenericAlloc`; every field
    // read is within it.
    unsafe {
        let pattern = &mut *object.cast::<PatternObject>();
        unicode::decref(pattern.groupindex);
        unicode::decref(pattern.indexgroup);
        unicode::decref(pattern.pattern);
        ((*header::type_of(object)).tp_free.unwrap())(object.cast());
    }
}

unsafe extern "C" fn match_dealloc(object: *mut PyObject) {
    // SAFETY: the object came from `PyType_GenericAlloc`; every field
    // read is within it.
    unsafe {
        let m = &mut *object.cast::<MatchObject>();
        unicode::decref(m.string);
        unicode::decref(m.regs);
        unicode::decref(m.pattern);
        ((*header::type_of(object)).tp_free.unwrap())(object.cast());
    }
}

unsafe extern "C" fn template_dealloc(object: *mut PyObject) {
    // SAFETY: the object came from `PyType_GenericAlloc`; every field
    // read is within it.
    unsafe {
        let template = &mut *object.cast::<TemplateObject>();
        unicode::decref(template.literal);
        let items = template.ob_base.ob_size.max(0) as usize;
        for i in 0..items {
            xdecref(template.items.as_mut_ptr().add(i).read().literal);
        }
        ((*header::type_of(object)).tp_free.unwrap())(object.cast());
    }
}

unsafe extern "C" fn scanner_dealloc(object: *mut PyObject) {
    // SAFETY: the object came from `PyType_GenericAlloc`; every field
    // read is within it.
    unsafe {
        let scanner = &mut *object.cast::<ScannerObject>();
        if !scanner.bytes_copy.is_null() {
            crate::alloc::pymem::plain_free(scanner.bytes_copy.cast());
        }
        xdecref(scanner.state.string);
        xdecref(scanner.pattern);
        ((*header::type_of(object)).tp_free.unwrap())(object.cast());
    }
}

const fn pattern_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"re.Pattern".as_ptr();
    t.tp_basicsize =
        (core::mem::offset_of!(PatternObject, code) + core::mem::size_of::<u32>()) as isize;
    t.tp_itemsize = core::mem::size_of::<u32>() as isize;
    t.tp_dealloc = Some(pattern_dealloc);
    t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_IMMUTABLETYPE | Py_TPFLAGS_DISALLOW_INSTANTIATION;
    t.tp_alloc = Some(PyType_GenericAlloc);
    t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
    t
}

const fn match_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"re.Match".as_ptr();
    t.tp_basicsize =
        (core::mem::offset_of!(MatchObject, mark) + core::mem::size_of::<isize>()) as isize;
    t.tp_itemsize = core::mem::size_of::<isize>() as isize;
    t.tp_dealloc = Some(match_dealloc);
    t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_IMMUTABLETYPE | Py_TPFLAGS_DISALLOW_INSTANTIATION;
    t.tp_alloc = Some(PyType_GenericAlloc);
    t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
    t
}

const fn template_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"_sre.SRE_Template".as_ptr();
    t.tp_basicsize = (core::mem::offset_of!(TemplateObject, items)
        + core::mem::size_of::<TemplateItem>()) as isize;
    t.tp_itemsize = core::mem::size_of::<TemplateItem>() as isize;
    t.tp_dealloc = Some(template_dealloc);
    t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_IMMUTABLETYPE | Py_TPFLAGS_DISALLOW_INSTANTIATION;
    t.tp_alloc = Some(PyType_GenericAlloc);
    t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
    t
}

const fn scanner_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"_sre.SRE_Scanner".as_ptr();
    t.tp_basicsize = core::mem::size_of::<ScannerObject>() as isize;
    t.tp_dealloc = Some(scanner_dealloc);
    t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_IMMUTABLETYPE | Py_TPFLAGS_DISALLOW_INSTANTIATION;
    t.tp_alloc = Some(PyType_GenericAlloc);
    t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
    t
}

/// The `re.Pattern` type object (CPython's module-state `Pattern_Type`);
/// unlike the core types these are module state, not C API symbols, so
/// they stay crate-internal addresses.
pub static mut Pattern_Type: PyTypeObject = pattern_type();

/// The `re.Match` type object (CPython's module-state `Match_Type`).
pub static mut Match_Type: PyTypeObject = match_type();

/// The `_sre.SRE_Template` type object.
pub static mut Template_Type: PyTypeObject = template_type();

/// The `_sre.SRE_Scanner` type object.
pub static mut Scanner_Type: PyTypeObject = scanner_type();

/// The four static types, for `init_static_types`.
pub fn static_types() -> [*mut PyTypeObject; 4] {
    [
        &raw mut Pattern_Type,
        &raw mut Match_Type,
        &raw mut Template_Type,
        &raw mut Scanner_Type,
    ]
}

// ------------------------------------------------------ the subject view

/// The string a pattern runs over: a character view of a `str` (kind
/// aware, lone surrogates included) or a byte buffer. `StrDrive`'s
/// offsets are character indices for `str` and byte indices for bytes,
/// the positions CPython's marks hold.
#[derive(Clone, Copy)]
enum Subject {
    Str(unicode::Chars),
    Bytes { data: *const u8, len: usize },
}

impl StrDrive for Subject {
    fn offset(&self, offset: usize, skip: usize) -> usize {
        offset + skip
    }

    fn count(&self) -> usize {
        match *self {
            Subject::Str(chars) => chars.len() as usize,
            Subject::Bytes { len, .. } => len,
        }
    }

    fn peek(&self, offset: usize) -> u32 {
        match *self {
            // SAFETY: the engine keeps the offset below `count()`.
            Subject::Str(chars) => chars.get(offset as isize),
            // SAFETY: the engine keeps the offset below `count()`.
            Subject::Bytes { data, .. } => unsafe { *data.add(offset) as u32 },
        }
    }

    fn back_peek(&self, offset: usize) -> u32 {
        match *self {
            // SAFETY: the engine calls this past position zero only.
            Subject::Str(chars) => chars.get(offset as isize - 1),
            // SAFETY: the engine calls this past position zero only.
            Subject::Bytes { data, .. } => unsafe { *data.add(offset - 1) as u32 },
        }
    }

    fn back_offset(&self, offset: usize, skip: usize) -> usize {
        offset - skip
    }
}

impl Subject {
    fn isbytes(self) -> bool {
        matches!(self, Subject::Bytes { .. })
    }
}

/// `getstring` (sre.c): the subject view of a string or buffer object.
/// `None` with the error set; `PyBUF_SIMPLE` views land in `hold` and
/// stay pinned until the caller releases them.
///
/// # Safety
///
/// `string` must be a valid object; `hold` must be droppable.
unsafe fn get_subject(
    pattern_isbytes: c_int,
    string: *mut PyObject,
    hold: &mut Option<Py_buffer>,
) -> Option<(Subject, usize)> {
    // SAFETY: the checks read the object's header and fixed fields.
    unsafe {
        if unicode::check(string) {
            if pattern_isbytes > 0 {
                tstate::set_string(
                    exceptions::type_error(),
                    b"cannot use a bytes pattern on a string-like object",
                );
                return None;
            }
            let chars = unicode::as_chars(string)?;
            return Some((Subject::Str(chars), chars.len() as usize));
        }
        let mut view: Py_buffer = core::mem::zeroed();
        if rt_bytes::PyObject_GetBuffer(string, &mut view, coil_capi_sys::PyBUF_SIMPLE) < 0 {
            tstate::set_string(
                exceptions::type_error(),
                format!(
                    "expected string or bytes-like object, got '{}'",
                    util::cstr((*header::type_of(string)).tp_name)
                )
                .as_bytes(),
            );
            return None;
        }
        if view.buf.is_null() {
            rt_bytes::PyBuffer_Release(&mut view);
            tstate::set_string(exceptions::value_error(), b"Buffer is NULL");
            return None;
        }
        let len = view.len as usize;
        if pattern_isbytes == 0 {
            rt_bytes::PyBuffer_Release(&mut view);
            tstate::set_string(
                exceptions::type_error(),
                b"cannot use a string pattern on a bytes-like object",
            );
            return None;
        }
        let subject = Subject::Bytes {
            data: view.buf as *const u8,
            len,
        };
        *hold = Some(view);
        Some((subject, len))
    }
}

/// Build the clamped window for a subject `get_subject` resolved.
fn clamp_window(length: usize, pos: isize, endpos: isize) -> (usize, usize) {
    let mut start = pos;
    let mut end = endpos;
    if start < 0 {
        start = 0;
    } else if start > length as isize {
        start = length as isize;
    }
    if end < 0 {
        end = 0;
    } else if end > length as isize {
        end = length as isize;
    }
    (start as usize, end as usize)
}

/// A fresh match state for a resolved subject, owning the string
/// reference `state_init` takes; the caller drops it (`state_fini`) and
/// keeps the buffer view alive for the state's lifetime.
fn new_state(
    string: *mut PyObject,
    subject: Subject,
    length: usize,
    pos: isize,
    endpos: isize,
) -> MatchState {
    unicode::incref(string);
    let (start, end) = clamp_window(length, pos, endpos);
    MatchState {
        string,
        subject,
        start,
        end,
        pos: start as isize,
        endpos: end as isize,
        must_advance: false,
        finished: false,
        engine: State::default(),
    }
}

impl MatchState {
    /// One `sre_match` run (`pymatch` in the engine): `state.ptr =
    /// state.start`, then the pattern anchored there.
    fn run_match(&mut self, code: &[u32], match_all: bool) -> bool {
        let mut req = Request::new(self.subject, self.start, self.end, code, match_all);
        req.must_advance = self.must_advance;
        self.engine.pymatch(req);
        self.engine.has_matched
    }

    /// One `sre_search` run, with CPython's `ptr > end` rejection: a
    /// window whose start passed its end never matches.
    fn run_search(&mut self, code: &[u32]) -> bool {
        if self.start > self.end {
            return false;
        }
        let mut req = Request::new(self.subject, self.start, self.end, code, false);
        req.must_advance = self.must_advance;
        self.engine.search(req);
        self.engine.has_matched
    }

    /// The match start (CPython's `state->start` after a search).
    fn match_start(&self) -> usize {
        self.engine.start
    }

    /// The match end (CPython's `state->ptr` after a match).
    fn match_end(&self) -> usize {
        self.engine.string_position
    }

    /// Group `index`'s span, or `None` when the group has no value;
    /// CPython's `mark` pair over the marks array. `index` is the
    /// engine's mark index: user group `g` is `g - 1`, matching the
    /// `MARK (group-1)*2` operands `_compiler.py` emits.
    fn group_span(&self, index: usize) -> Option<(usize, usize)> {
        let (start, end) = self.engine.marks.get(index);
        if start.is_none() || end.is_none() {
            return None;
        }
        Some((start.unwrap(), end.unwrap()))
    }
}

// ------------------------------------------------------- match building

/// `getslice` (sre.c): the substring `[start, end)` of the subject, as
/// the subject's own type. The caller owns the result.
///
/// # Safety
///
/// `string` must be the subject object of `subject`, and the window
/// within it.
unsafe fn getslice(
    subject: Subject,
    string: *mut PyObject,
    start: usize,
    end: usize,
) -> *mut PyObject {
    // SAFETY: the slice constructors read within the subject window.
    unsafe {
        match subject {
            Subject::Str(_) => unicode::PyUnicode_Substring(string, start as isize, end as isize),
            Subject::Bytes { data, .. } => {
                if header::type_of(string) == &raw mut rt_bytes::PyBytes_Type
                    && start == 0
                    && end == rt_bytes::size(string) as usize
                {
                    unicode::incref(string);
                    return string;
                }
                rt_bytes::PyBytes_FromStringAndSize(data.add(start).cast(), (end - start) as isize)
            }
        }
    }
}

/// `state_getslice` (sre.c): group `index`'s slice, the empty string for
/// unset groups when `empty`, the default otherwise. `index` is the
/// 1-based group number, as in CPython's `(index - 1) * 2`.
///
/// # Safety
///
/// `string` must be `state`'s subject object.
unsafe fn state_getslice(
    state: &MatchState,
    string: *mut PyObject,
    index: usize,
    empty: bool,
) -> *mut PyObject {
    // SAFETY: the span reads are within the state's marks.
    unsafe {
        let Some((i, j)) = state.group_span(index - 1) else {
            if empty {
                return getslice(state.subject, string, 0, 0);
            }
            return crate::object::singletons::none();
        };
        if i > j {
            tstate::set_string(
                exceptions::system_error(),
                b"The span of capturing group is wrong, please report a bug for the re module.",
            );
            return core::ptr::null_mut();
        }
        getslice(state.subject, string, i, j)
    }
}

/// `pattern_new_match` (sre.c): the Match object for a finished run, or
/// `None` when the run did not match. The result owns its references.
///
/// # Safety
///
/// `pattern` must be a pattern object; `state`'s subject must be live.
unsafe fn new_match(
    pattern: *mut PatternObject,
    state: &MatchState,
    matched: bool,
) -> *mut PyObject {
    // SAFETY: the allocation and field writes stay within the object.
    unsafe {
        if !matched {
            return crate::object::singletons::none();
        }
        let groups = (*pattern).groups as usize;
        let object = PyType_GenericAlloc(&raw mut Match_Type, (2 * (groups + 1)) as isize);
        if object.is_null() {
            return core::ptr::null_mut();
        }
        let m = &mut *object.cast::<MatchObject>();
        m.ob_base.ob_size = (2 * (groups + 1)) as isize;
        unicode::incref(pattern as *mut PyObject);
        m.pattern = pattern as *mut PyObject;
        unicode::incref(state.string);
        m.string = state.string;
        m.regs = core::ptr::null_mut();
        m.groups = (groups + 1) as isize;
        let marks = m.mark.as_mut_ptr();
        *marks.add(0) = state.match_start() as isize;
        *marks.add(1) = state.match_end() as isize;
        for group in 0..groups {
            match state.group_span(group) {
                Some((i, j)) if i <= j => {
                    *marks.add(2 * group + 2) = i as isize;
                    *marks.add(2 * group + 3) = j as isize;
                }
                Some((_, _)) => {
                    tstate::set_string(
                        exceptions::system_error(),
                        b"The span of capturing group is wrong, please report a bug for the re module.",
                    );
                    unicode::decref(object);
                    return core::ptr::null_mut();
                }
                None => {
                    *marks.add(2 * group + 2) = -1;
                    *marks.add(2 * group + 3) = -1;
                }
            }
        }
        m.pos = state.pos;
        m.endpos = state.endpos;
        m.lastindex = state.engine.marks.last_index();
        object
    }
}

/// The code array of a pattern object.
///
/// # Safety
///
/// `pattern` must be a pattern object.
unsafe fn code_of<'a>(pattern: *mut PatternObject) -> &'a [u32] {
    // SAFETY: the code words are the pattern's variable tail.
    unsafe {
        core::slice::from_raw_parts(
            (*pattern).code.as_ptr(),
            (*pattern).ob_base.ob_size.max(0) as usize,
        )
    }
}

/// A null-tolerant `Py_DECREF` for error paths.
fn xdecref(object: *mut PyObject) {
    if !object.is_null() {
        unicode::decref(object);
    }
}

// --------------------------------------------------------- pattern cores

/// The one-shot run `match`, `fullmatch` and `search` share:
/// `state_init`, the engine call, `pattern_new_match`, `state_fini`.
///
/// # Safety
///
/// `pattern` must be a pattern object and `string` a valid object.
unsafe fn pattern_run(
    pattern: *mut PatternObject,
    string: *mut PyObject,
    pos: isize,
    endpos: isize,
    scanning: bool,
    match_all: bool,
) -> *mut PyObject {
    let mut hold = None;
    // SAFETY: the subject resolution reads `string` only.
    let Some((subject, length)) = (unsafe { get_subject((*pattern).isbytes, string, &mut hold) })
    else {
        return core::ptr::null_mut();
    };
    let mut state = new_state(string, subject, length, pos, endpos);
    // SAFETY: the code words belong to `pattern`.
    let code = unsafe { code_of(pattern) };
    let matched = if scanning {
        state.run_search(code)
    } else {
        state.run_match(code, match_all)
    };
    // SAFETY: the state's subject is live through here.
    let result = unsafe { new_match(pattern, &state, matched) };
    finish_state(&mut state, &mut hold);
    result
}

/// `state_fini`: drop the state's references and release its view.
fn finish_state(state: &mut MatchState, hold: &mut Option<Py_buffer>) {
    release_hold(hold);
    unicode::decref(state.string);
    state.string = core::ptr::null_mut();
}

/// `Pattern.match`: anchored at `pos`, `endpos` clamped.
///
/// # Safety
///
/// `pattern` must be a pattern object and `string` a valid object.
pub unsafe fn pattern_match(
    pattern: *mut PyObject,
    string: *mut PyObject,
    pos: isize,
    endpos: isize,
) -> *mut PyObject {
    // SAFETY: the run reads the pattern's code and the subject only.
    unsafe { pattern_run(pattern.cast(), string, pos, endpos, false, false) }
}

/// `Pattern.fullmatch`: anchored and required to reach `endpos`.
///
/// # Safety
///
/// `pattern` must be a pattern object and `string` a valid object.
pub unsafe fn pattern_fullmatch(
    pattern: *mut PyObject,
    string: *mut PyObject,
    pos: isize,
    endpos: isize,
) -> *mut PyObject {
    // SAFETY: the run reads the pattern's code and the subject only.
    unsafe { pattern_run(pattern.cast(), string, pos, endpos, false, true) }
}

/// `Pattern.search`.
///
/// # Safety
///
/// `pattern` must be a pattern object and `string` a valid object.
pub unsafe fn pattern_search(
    pattern: *mut PyObject,
    string: *mut PyObject,
    pos: isize,
    endpos: isize,
) -> *mut PyObject {
    // SAFETY: the run reads the pattern's code and the subject only.
    unsafe { pattern_run(pattern.cast(), string, pos, endpos, true, false) }
}

/// `Pattern.findall`.
///
/// # Safety
///
/// `pattern` must be a pattern object and `string` a valid object.
pub unsafe fn pattern_findall(
    pattern: *mut PyObject,
    string: *mut PyObject,
    pos: isize,
    endpos: isize,
) -> *mut PyObject {
    // SAFETY: the loop reads the pattern's code and the subject only.
    unsafe {
        let pattern = pattern.cast::<PatternObject>();
        let mut hold = None;
        let Some((subject, length)) = get_subject((*pattern).isbytes, string, &mut hold) else {
            return core::ptr::null_mut();
        };
        let mut state = new_state(string, subject, length, pos, endpos);
        let list = list::PyList_New(0);
        if list.is_null() {
            finish_state(&mut state, &mut hold);
            return core::ptr::null_mut();
        }
        let code = code_of(pattern);
        while state.start <= state.end {
            state.engine.reset(state.start);
            if !state.run_search(code) {
                break;
            }
            let item = match (*pattern).groups {
                0 => getslice(
                    state.subject,
                    string,
                    state.match_start(),
                    state.match_end(),
                ),
                1 => state_getslice(&state, string, 1, true),
                _ => {
                    let groups = (*pattern).groups as usize;
                    let item = tuple::PyTuple_New(groups as isize);
                    if !item.is_null() {
                        let mut broken = false;
                        for group in 0..groups {
                            let slice = state_getslice(&state, string, group + 1, true);
                            if slice.is_null() {
                                broken = true;
                                break;
                            }
                            tuple::PyTuple_SetItem(item, group as isize, slice);
                        }
                        if !broken {
                            item
                        } else {
                            unicode::decref(item);
                            core::ptr::null_mut()
                        }
                    } else {
                        item
                    }
                }
            };
            if item.is_null() {
                unicode::decref(list);
                finish_state(&mut state, &mut hold);
                return core::ptr::null_mut();
            }
            if list::PyList_Append(list, item) < 0 {
                unicode::decref(item);
                unicode::decref(list);
                finish_state(&mut state, &mut hold);
                return core::ptr::null_mut();
            }
            unicode::decref(item);
            state.must_advance = state.match_end() == state.match_start();
            state.start = state.match_end();
        }
        finish_state(&mut state, &mut hold);
        list
    }
}

/// `Pattern.split`.
///
/// # Safety
///
/// `pattern` must be a pattern object and `string` a valid object.
pub unsafe fn pattern_split(
    pattern: *mut PyObject,
    string: *mut PyObject,
    maxsplit: isize,
) -> *mut PyObject {
    // SAFETY: the loop reads the pattern's code and the subject only.
    unsafe {
        let pattern = pattern.cast::<PatternObject>();
        let mut hold = None;
        let Some((subject, length)) = get_subject((*pattern).isbytes, string, &mut hold) else {
            return core::ptr::null_mut();
        };
        let mut state = new_state(string, subject, length, 0, isize::MAX);
        let list = list::PyList_New(0);
        if list.is_null() {
            finish_state(&mut state, &mut hold);
            return core::ptr::null_mut();
        }
        let code = code_of(pattern);
        let mut n: isize = 0;
        let mut last = state.start;
        while maxsplit == 0 || n < maxsplit {
            state.engine.reset(state.start);
            if !state.run_search(code) {
                break;
            }
            let b = state.match_start();
            let e = state.match_end();
            let item = getslice(state.subject, string, last, b);
            if item.is_null() || list::PyList_Append(list, item) < 0 {
                xdecref(item);
                unicode::decref(list);
                finish_state(&mut state, &mut hold);
                return core::ptr::null_mut();
            }
            unicode::decref(item);
            let mut failed = false;
            for group in 0..(*pattern).groups as usize {
                let slice = state_getslice(&state, string, group + 1, false);
                if slice.is_null() || list::PyList_Append(list, slice) < 0 {
                    xdecref(slice);
                    failed = true;
                    break;
                }
                unicode::decref(slice);
            }
            if failed {
                unicode::decref(list);
                finish_state(&mut state, &mut hold);
                return core::ptr::null_mut();
            }
            n += 1;
            state.must_advance = e == b;
            last = e;
            state.start = e;
        }
        let item = getslice(state.subject, string, last, state.end);
        if item.is_null() || list::PyList_Append(list, item) < 0 {
            xdecref(item);
            unicode::decref(list);
            finish_state(&mut state, &mut hold);
            return core::ptr::null_mut();
        }
        unicode::decref(item);
        finish_state(&mut state, &mut hold);
        list
    }
}

/// `pattern_subx` (sre.c) with CPython's filter trichotomy. The literal
/// replacement runs natively; a callable replacement and a template with
/// escapes need calls into Python, which raise typed errors naming what
/// is missing — a callable only once a match actually needs it, as in
/// CPython.
///
/// # Safety
///
/// `pattern` must be a pattern object; `repl` and `string` valid objects.
unsafe fn pattern_subx(
    pattern: *mut PatternObject,
    repl: *mut PyObject,
    string: *mut PyObject,
    count: isize,
    subn: bool,
) -> *mut PyObject {
    // SAFETY: header and type reads stay within the objects.
    unsafe {
        #[derive(PartialEq, Clone, Copy)]
        enum Filter {
            Literal,
            Callable,
        }
        let filter;
        let literal;
        if (*header::type_of(repl)).tp_call.is_some() {
            filter = Filter::Callable;
            literal = core::ptr::null_mut();
        } else {
            let mut hold = None;
            let mut is_literal = false;
            if let Some((subject, length)) = get_subject(-1, repl, &mut hold) {
                match subject {
                    Subject::Bytes { data, len } => {
                        is_literal = !core::slice::from_raw_parts(data, len).contains(&b'\\');
                    }
                    Subject::Str(_) => {
                        is_literal =
                            unicode::PyUnicode_FindChar(repl, b'\\' as u32, 0, length as isize, 1)
                                == -1;
                    }
                }
                release_hold(&mut hold);
            } else {
                tstate::clear();
            }
            if is_literal {
                filter = Filter::Literal;
                literal = repl;
                unicode::incref(literal);
            } else {
                // CPython hands the template to `re._compile_template`;
                // compiling one needs Python-level calls.
                tstate::set_string(
                    exceptions::system_error(),
                    b"coil: compiling replacement templates needs the container runtime",
                );
                return core::ptr::null_mut();
            }
        }

        let mut hold = None;
        let Some((subject, length)) = get_subject((*pattern).isbytes, string, &mut hold) else {
            xdecref(literal);
            return core::ptr::null_mut();
        };
        let mut state = new_state(string, subject, length, 0, isize::MAX);
        let list = list::PyList_New(0);
        if list.is_null() {
            finish_state(&mut state, &mut hold);
            xdecref(literal);
            return core::ptr::null_mut();
        }
        let code = code_of(pattern);
        let mut i = 0usize;
        let mut n: isize = 0;
        while count == 0 || n < count {
            state.engine.reset(state.start);
            if !state.run_search(code) {
                break;
            }
            let b = state.match_start();
            let e = state.match_end();
            if i < b {
                let item = getslice(state.subject, string, i, b);
                if item.is_null() || list::PyList_Append(list, item) < 0 {
                    xdecref(item);
                    sub_error(list, &mut state, &mut hold, literal);
                    return core::ptr::null_mut();
                }
                unicode::decref(item);
            }
            if filter != Filter::Literal {
                // CPython hands the match to the callable here; the
                // match object's construction still runs first.
                let m = new_match(pattern, &state, true);
                unicode::decref(m);
                tstate::set_string(
                    exceptions::system_error(),
                    b"coil: calling replacement callables needs the container runtime",
                );
                sub_error(list, &mut state, &mut hold, literal);
                return core::ptr::null_mut();
            } else {
                if list::PyList_Append(list, literal) < 0 {
                    sub_error(list, &mut state, &mut hold, literal);
                    return core::ptr::null_mut();
                }
            }
            i = e;
            n += 1;
            state.must_advance = e == b;
            state.start = e;
        }
        if i < state.end {
            let item = getslice(state.subject, string, i, state.end);
            if item.is_null() || list::PyList_Append(list, item) < 0 {
                xdecref(item);
                sub_error(list, &mut state, &mut hold, literal);
                return core::ptr::null_mut();
            }
            unicode::decref(item);
        }
        finish_state(&mut state, &mut hold);
        xdecref(literal);
        let joiner = getslice(subject, string, 0, 0);
        if joiner.is_null() {
            unicode::decref(list);
            return core::ptr::null_mut();
        }
        let size = list::PyList_Size(list);
        let item = if size == 0 {
            unicode::decref(list);
            joiner
        } else {
            let mut items = Vec::with_capacity(size as usize);
            for index in 0..size {
                items.push(list::PyList_GetItem(list, index));
            }
            let joined = if subject.isbytes() {
                rt_bytes::join_bytes(joiner, &items)
            } else {
                unicode::join_items(joiner, &items)
            };
            unicode::decref(joiner);
            unicode::decref(list);
            joined
        };
        if item.is_null() {
            return core::ptr::null_mut();
        }
        if subn {
            let count_object = long::PyLong_FromSsize_t(n);
            if count_object.is_null() {
                unicode::decref(item);
                return core::ptr::null_mut();
            }
            let pair = tuple::PyTuple_New(2);
            if pair.is_null() {
                unicode::decref(item);
                unicode::decref(count_object);
                return core::ptr::null_mut();
            }
            tuple::PyTuple_SetItem(pair, 0, item);
            tuple::PyTuple_SetItem(pair, 1, count_object);
            return pair;
        }
        item
    }
}

/// The error exit of `pattern_subx`.
fn sub_error(
    list: *mut PyObject,
    state: &mut MatchState,
    hold: &mut Option<Py_buffer>,
    literal: *mut PyObject,
) {
    unicode::decref(list);
    finish_state(state, hold);
    xdecref(literal);
}

/// `finish_state` without a state reference to drop: releasing the
/// template probe's view.
fn release_hold(hold: &mut Option<Py_buffer>) {
    if let Some(view) = hold.take() {
        // SAFETY: the view was filled by `PyObject_GetBuffer`.
        unsafe { rt_bytes::PyBuffer_Release(&view as *const Py_buffer as *mut Py_buffer) };
    }
}

/// `Pattern.sub`.
///
/// # Safety
///
/// `pattern` must be a pattern object; `repl` and `string` valid objects.
pub unsafe fn pattern_sub(
    pattern: *mut PyObject,
    repl: *mut PyObject,
    string: *mut PyObject,
    count: isize,
) -> *mut PyObject {
    // SAFETY: the run reads the pattern's code and the subject only.
    unsafe { pattern_subx(pattern.cast(), repl, string, count, false) }
}

/// `Pattern.subn`.
///
/// # Safety
///
/// `pattern` must be a pattern object; `repl` and `string` valid objects.
pub unsafe fn pattern_subn(
    pattern: *mut PyObject,
    repl: *mut PyObject,
    string: *mut PyObject,
    count: isize,
) -> *mut PyObject {
    // SAFETY: the run reads the pattern's code and the subject only.
    unsafe { pattern_subx(pattern.cast(), repl, string, count, true) }
}

// ---------------------------------------------------------- scanner core

/// `Pattern.scanner`: the persistent search state.
///
/// # Safety
///
/// `pattern` must be a pattern object and `string` a valid object.
pub unsafe fn pattern_scanner(
    pattern: *mut PyObject,
    string: *mut PyObject,
    pos: isize,
    endpos: isize,
) -> *mut PyObject {
    // SAFETY: the scanner allocation and field writes stay within it.
    unsafe {
        let pattern = pattern.cast::<PatternObject>();
        let mut hold = None;
        let Some((mut subject, length)) = get_subject((*pattern).isbytes, string, &mut hold) else {
            return core::ptr::null_mut();
        };
        let object = PyType_GenericAlloc(&raw mut Scanner_Type, 0);
        if object.is_null() {
            release_hold(&mut hold);
            return core::ptr::null_mut();
        }
        // A bytes subject's buffer must outlive the view CPython pins
        // until `state_fini`; the scanner copies it and owns the copy.
        let mut bytes_copy = core::ptr::null_mut();
        if let Subject::Bytes { data, len } = subject {
            let copy = crate::alloc::pymem::plain_malloc(len);
            if copy.is_null() {
                tstate::set_string(exceptions::memory_error(), b"");
                release_hold(&mut hold);
                unicode::decref(object);
                return core::ptr::null_mut();
            }
            core::ptr::copy_nonoverlapping(data, copy, len);
            subject = Subject::Bytes { data: copy, len };
            bytes_copy = copy;
        }
        let scanner = &mut *object.cast::<ScannerObject>();
        unicode::incref(pattern as *mut PyObject);
        scanner.pattern = pattern as *mut PyObject;
        scanner.bytes_copy = bytes_copy;
        scanner.state = new_state(string, subject, length, pos, endpos);
        release_hold(&mut hold);
        object
    }
}

/// `Scanner.match`: anchored at the cursor, then the cursor moves past
/// the match or the scanner retires.
///
/// # Safety
///
/// `scanner` must be a scanner object.
pub unsafe fn scanner_match(scanner: *mut PyObject) -> *mut PyObject {
    // SAFETY: the run reads the pattern's code and the subject only.
    unsafe {
        let scanner = scanner.cast::<ScannerObject>();
        if (*scanner).state.finished {
            return crate::object::singletons::none();
        }
        (*scanner).state.engine.reset((*scanner).state.start);
        let code = code_of((*scanner).pattern.cast::<PatternObject>());
        let matched = (*scanner).state.run_match(code, false);
        let result = new_match(
            (*scanner).pattern.cast::<PatternObject>(),
            &(*scanner).state,
            matched,
        );
        advance_scanner(&mut *scanner, matched);
        result
    }
}

/// `Scanner.search`.
///
/// # Safety
///
/// `scanner` must be a scanner object.
pub unsafe fn scanner_search(scanner: *mut PyObject) -> *mut PyObject {
    // SAFETY: the run reads the pattern's code and the subject only.
    unsafe {
        let scanner = scanner.cast::<ScannerObject>();
        if (*scanner).state.finished {
            return crate::object::singletons::none();
        }
        (*scanner).state.engine.reset((*scanner).state.start);
        let code = code_of((*scanner).pattern.cast::<PatternObject>());
        let matched = (*scanner).state.run_search(code);
        let result = new_match(
            (*scanner).pattern.cast::<PatternObject>(),
            &(*scanner).state,
            matched,
        );
        advance_scanner(&mut *scanner, matched);
        result
    }
}

/// The cursor update every scanner method ends with: CPython's
/// `state->start = NULL` once a run fails, else the must-advance rule.
fn advance_scanner(scanner: &mut ScannerObject, matched: bool) {
    if !matched {
        scanner.state.finished = true;
        return;
    }
    scanner.state.must_advance = scanner.state.match_end() == scanner.state.match_start();
    scanner.state.start = scanner.state.match_end();
}

// ------------------------------------------------------------ match core

/// `match_getindex` (sre.c): an int group number, or a name looked up
/// in the pattern's `groupindex`.
///
/// # Safety
///
/// `m` must be a match object; `index` a valid object or null.
unsafe fn match_getindex(m: *mut MatchObject, index: *mut PyObject) -> Option<usize> {
    // SAFETY: the reads stay within the match, its pattern and `index`.
    unsafe {
        let mut i: isize = -1;
        if index.is_null() {
            return Some(0);
        }
        if long::is_long(index) {
            i = long::PyLong_AsSsize_t(index);
        } else if !(*(*m).pattern.cast::<PatternObject>()).groupindex.is_null() {
            let groupindex = (*(*m).pattern.cast::<PatternObject>()).groupindex;
            let found = dict::PyDict_GetItemWithError(groupindex, index);
            if !found.is_null() && long::is_long(found) {
                i = long::PyLong_AsSsize_t(found);
            }
        }
        if i < 0 || i >= (*m).groups {
            if tstate::occurred().is_null() {
                tstate::set_string(exceptions::index_error(), b"no such group");
            }
            return None;
        }
        Some(i as usize)
    }
}

/// `match_getslice_by_index` (sre.c): the group's slice, or `def` when
/// the group has no value.
///
/// # Safety
///
/// `m` must be a match object and `def` a valid object.
unsafe fn match_getslice_by_index(
    m: *mut MatchObject,
    index: usize,
    def: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the reads stay within the match and its string.
    unsafe {
        if (*m).string == crate::object::singletons::none() || (*m).mark_at(index * 2) < 0 {
            unicode::incref(def);
            return def;
        }
        let pattern = (*m).pattern.cast::<PatternObject>();
        let mut hold = None;
        let Some((subject, length)) = get_subject((*pattern).isbytes, (*m).string, &mut hold)
        else {
            return core::ptr::null_mut();
        };
        let i = ((*m).mark_at(index * 2) as usize).min(length);
        let j = ((*m).mark_at(index * 2 + 1) as usize).min(length);
        let result = getslice(subject, (*m).string, i, j);
        release_hold(&mut hold);
        result
    }
}

/// `match_getslice`: `match_getslice_by_index` through `match_getindex`.
///
/// # Safety
///
/// `m` must be a match object; `index` and `def` valid objects.
unsafe fn match_getslice(
    m: *mut MatchObject,
    index: *mut PyObject,
    def: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the index conversion reads the match's pattern.
    unsafe {
        let Some(i) = match_getindex(m, index) else {
            return core::ptr::null_mut();
        };
        match_getslice_by_index(m, i, def)
    }
}

/// `Match.group`.
///
/// # Safety
///
/// `m` must be a match object; every entry of `args` a valid object.
pub unsafe fn match_group(m: *mut PyObject, args: &[*mut PyObject]) -> *mut PyObject {
    // SAFETY: the reads stay within the match and its pattern.
    unsafe {
        let m = m.cast::<MatchObject>();
        match args.len() {
            0 => match_getslice(m, core::ptr::null_mut(), crate::object::singletons::none()),
            1 => match_getslice(m, args[0], crate::object::singletons::none()),
            _ => {
                let result = tuple::PyTuple_New(args.len() as isize);
                if result.is_null() {
                    return core::ptr::null_mut();
                }
                for (i, index) in args.iter().enumerate() {
                    let item = match_getslice(m, *index, crate::object::singletons::none());
                    if item.is_null() {
                        unicode::decref(result);
                        return core::ptr::null_mut();
                    }
                    tuple::PyTuple_SetItem(result, i as isize, item);
                }
                result
            }
        }
    }
}

/// `Match.__getitem__`.
///
/// # Safety
///
/// `m` must be a match object and `name` a valid object.
pub unsafe fn match_getitem(m: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    // SAFETY: the slice conversion reads the match's pattern.
    unsafe {
        match_getslice(
            m.cast::<MatchObject>(),
            name,
            crate::object::singletons::none(),
        )
    }
}

/// `Match.start`.
///
/// # Safety
///
/// `m` must be a match object; `group` a valid object or null.
pub unsafe fn match_start(m: *mut PyObject, group: *mut PyObject) -> isize {
    // SAFETY: the reads stay within the match.
    unsafe {
        let m = m.cast::<MatchObject>();
        match match_getindex(m, group) {
            Some(index) => (*m).mark_at(index * 2),
            None => -1,
        }
    }
}

/// `Match.end`.
///
/// # Safety
///
/// `m` must be a match object; `group` a valid object or null.
pub unsafe fn match_end(m: *mut PyObject, group: *mut PyObject) -> isize {
    // SAFETY: the reads stay within the match.
    unsafe {
        let m = m.cast::<MatchObject>();
        match match_getindex(m, group) {
            Some(index) => (*m).mark_at(index * 2 + 1),
            None => -1,
        }
    }
}

/// `Match.span`.
///
/// # Safety
///
/// `m` must be a match object; `group` a valid object or null.
pub unsafe fn match_span(m: *mut PyObject, group: *mut PyObject) -> *mut PyObject {
    // SAFETY: the reads stay within the match.
    unsafe {
        let m = m.cast::<MatchObject>();
        let Some(index) = match_getindex(m, group) else {
            return core::ptr::null_mut();
        };
        pair((*m).mark_at(index * 2), (*m).mark_at(index * 2 + 1))
    }
}

/// `_pair` (sre.c).
fn pair(i1: isize, i2: isize) -> *mut PyObject {
    let pair = tuple::PyTuple_New(2);
    if pair.is_null() {
        return core::ptr::null_mut();
    }
    let first = long::PyLong_FromSsize_t(i1);
    let second = long::PyLong_FromSsize_t(i2);
    if first.is_null() || second.is_null() {
        xdecref(first);
        xdecref(second);
        unicode::decref(pair);
        return core::ptr::null_mut();
    }
    // SAFETY: the steals target a fresh tuple.
    unsafe {
        tuple::PyTuple_SetItem(pair, 0, first);
        tuple::PyTuple_SetItem(pair, 1, second);
    }
    pair
}

/// `Match.groups`.
///
/// # Safety
///
/// `m` must be a match object and `default` a valid object.
pub unsafe fn match_groups(m: *mut PyObject, default: *mut PyObject) -> *mut PyObject {
    // SAFETY: the reads stay within the match.
    unsafe {
        let m = m.cast::<MatchObject>();
        let result = tuple::PyTuple_New((*m).groups - 1);
        if result.is_null() {
            return core::ptr::null_mut();
        }
        for index in 1..(*m).groups {
            let item = match_getslice_by_index(m, index as usize, default);
            if item.is_null() {
                unicode::decref(result);
                return core::ptr::null_mut();
            }
            tuple::PyTuple_SetItem(result, index - 1, item);
        }
        result
    }
}

/// `Match.groupdict`.
///
/// # Safety
///
/// `m` must be a match object and `default` a valid object.
pub unsafe fn match_groupdict(m: *mut PyObject, default: *mut PyObject) -> *mut PyObject {
    // SAFETY: the dict iteration reads the pattern's groupindex.
    unsafe {
        let m = m.cast::<MatchObject>();
        let result = dict::PyDict_New();
        if result.is_null() {
            return core::ptr::null_mut();
        }
        let pattern = (*m).pattern.cast::<PatternObject>();
        if (*pattern).groupindex.is_null() {
            return result;
        }
        let mut pos: isize = 0;
        let mut key: *mut PyObject = core::ptr::null_mut();
        let mut value: *mut PyObject = core::ptr::null_mut();
        loop {
            if dict::PyDict_Next((*pattern).groupindex, &mut pos, &mut key, &mut value) == 0 {
                break;
            }
            let slice = match_getslice(m, key, default);
            if slice.is_null() || dict::PyDict_SetItem(result, key, slice) < 0 {
                xdecref(slice);
                unicode::decref(result);
                return core::ptr::null_mut();
            }
            unicode::decref(slice);
        }
        result
    }
}

/// `Match.expand`: CPython delegates to `re._compile_template`, a
/// Python-level call.
///
/// # Safety
///
/// `m` must be a match object and `template` a valid object.
pub unsafe fn match_expand(m: *mut PyObject, template: *mut PyObject) -> *mut PyObject {
    let _ = (m, template);
    tstate::set_string(
        exceptions::system_error(),
        b"coil: compiling replacement templates needs the container runtime",
    );
    core::ptr::null_mut()
}

/// `Match.__copy__` and `Match.__deepcopy__`.
///
/// # Safety
///
/// `m` must be a match object.
pub unsafe fn match_copy(m: *mut PyObject) -> *mut PyObject {
    unicode::incref(m);
    m
}

// ------------------------------------------------------- match attributes

/// `Match.string`.
///
/// # Safety
///
/// `m` must be a match object.
pub unsafe fn match_string(m: *mut PyObject) -> *mut PyObject {
    // SAFETY: m is a match object whose fields outlive the call.
    unsafe {
        let m = m.cast::<MatchObject>();
        unicode::incref((*m).string);
        (*m).string
    }
}

/// `Match.re`.
///
/// # Safety
///
/// `m` must be a match object.
pub unsafe fn match_re(m: *mut PyObject) -> *mut PyObject {
    // SAFETY: m is a match object whose fields outlive the call.
    unsafe {
        let m = m.cast::<MatchObject>();
        unicode::incref((*m).pattern);
        (*m).pattern
    }
}

/// `Match.pos`.
///
/// # Safety
///
/// `m` must be a match object.
pub unsafe fn match_pos(m: *mut PyObject) -> isize {
    // SAFETY: m is a match object.
    unsafe { (*m.cast::<MatchObject>()).pos }
}

/// `Match.endpos`.
///
/// # Safety
///
/// `m` must be a match object.
pub unsafe fn match_endpos(m: *mut PyObject) -> isize {
    // SAFETY: m is a match object.
    unsafe { (*m.cast::<MatchObject>()).endpos }
}

/// `Match.lastindex`.
///
/// # Safety
///
/// `m` must be a match object.
pub unsafe fn match_lastindex(m: *mut PyObject) -> *mut PyObject {
    // SAFETY: m is a match object.
    let lastindex = unsafe { (*m.cast::<MatchObject>()).lastindex };
    if lastindex >= 0 {
        long::PyLong_FromSsize_t(lastindex)
    } else {
        crate::object::singletons::none()
    }
}

/// `Match.lastgroup`.
///
/// # Safety
///
/// `m` must be a match object.
pub unsafe fn match_lastgroup(m: *mut PyObject) -> *mut PyObject {
    // SAFETY: the indexgroup read stays within the pattern.
    unsafe {
        let m = m.cast::<MatchObject>();
        let pattern = (*m).pattern.cast::<PatternObject>();
        let indexgroup = (*pattern).indexgroup;
        if !indexgroup.is_null() && (*m).lastindex >= 0 {
            let size = tuple::PyTuple_Size(indexgroup);
            if (*m).lastindex < size {
                let name = tuple::PyTuple_GetItem(indexgroup, (*m).lastindex);
                unicode::incref(name);
                return name;
            }
        }
        crate::object::singletons::none()
    }
}

/// `Match.regs`: the `((start, end), ...)` tuple, cached.
///
/// # Safety
///
/// `m` must be a match object.
pub unsafe fn match_regs(m: *mut PyObject) -> *mut PyObject {
    // SAFETY: the reads stay within the match.
    unsafe {
        let mo = m.cast::<MatchObject>();
        if !(*mo).regs.is_null() {
            unicode::incref((*mo).regs);
            return (*mo).regs;
        }
        let regs = tuple::PyTuple_New((*mo).groups);
        if regs.is_null() {
            return core::ptr::null_mut();
        }
        for index in 0..(*mo).groups {
            let item = pair(
                (*mo).mark_at(index as usize * 2),
                (*mo).mark_at(index as usize * 2 + 1),
            );
            if item.is_null() {
                unicode::decref(regs);
                return core::ptr::null_mut();
            }
            tuple::PyTuple_SetItem(regs, index, item);
        }
        unicode::incref(regs);
        (*mo).regs = regs;
        regs
    }
}

// ---------------------------------------------------- pattern attributes

/// `Pattern.pattern`.
///
/// # Safety
///
/// `pattern` must be a pattern object.
pub unsafe fn pattern_pattern(pattern: *mut PyObject) -> *mut PyObject {
    // SAFETY: pattern is a pattern object whose fields outlive the call.
    unsafe {
        let pattern = pattern.cast::<PatternObject>();
        unicode::incref((*pattern).pattern);
        (*pattern).pattern
    }
}

/// `Pattern.flags`.
///
/// # Safety
///
/// `pattern` must be a pattern object.
pub unsafe fn pattern_flags(pattern: *mut PyObject) -> c_int {
    // SAFETY: pattern is a pattern object.
    unsafe { (*pattern.cast::<PatternObject>()).flags }
}

/// `Pattern.groups`.
///
/// # Safety
///
/// `pattern` must be a pattern object.
pub unsafe fn pattern_groups(pattern: *mut PyObject) -> isize {
    // SAFETY: pattern is a pattern object.
    unsafe { (*pattern.cast::<PatternObject>()).groups }
}

/// `Pattern.groupindex`.
///
/// # Safety
///
/// `pattern` must be a pattern object.
pub unsafe fn pattern_groupindex(pattern: *mut PyObject) -> *mut PyObject {
    // SAFETY: pattern is a pattern object whose fields outlive the call.
    unsafe {
        let pattern = pattern.cast::<PatternObject>();
        if (*pattern).groupindex.is_null() {
            return dict::PyDict_New();
        }
        unicode::incref((*pattern).groupindex);
        (*pattern).groupindex
    }
}

/// `Pattern.__copy__` and `Pattern.__deepcopy__`.
///
/// # Safety
///
/// `pattern` must be a pattern object.
pub unsafe fn pattern_copy(pattern: *mut PyObject) -> *mut PyObject {
    unicode::incref(pattern);
    pattern
}

// -------------------------------------------------------- template object

/// `_sre.template`: the replacement object `sub` and `expand` run.
///
/// # Safety
///
/// `pattern` must be a valid object and `template` a list of
/// interleaved literals and group indices.
pub unsafe fn template(pattern: *mut PyObject, template: *mut PyObject) -> *mut PyObject {
    // SAFETY: the list reads stay within the template argument.
    unsafe {
        let _ = pattern;
        let n = list::PyList_Size(template);
        if (n & 1) == 0 || n < 1 {
            tstate::set_string(exceptions::type_error(), b"invalid template");
            return core::ptr::null_mut();
        }
        let items = n / 2;
        let object = PyType_GenericAlloc(&raw mut Template_Type, items);
        if object.is_null() {
            return core::ptr::null_mut();
        }
        let t = &mut *object.cast::<TemplateObject>();
        t.ob_base.ob_size = items;
        t.chunks = 1 + 2 * items;
        let first = list::PyList_GetItem(template, 0);
        unicode::incref(first);
        t.literal = first;
        for item in 0..items {
            let index = list::PyList_GetItem(template, 2 * item + 1);
            let index = long::PyLong_AsSsize_t(index);
            if index == -1 && !tstate::occurred().is_null() {
                t.ob_base.ob_size = item;
                unicode::decref(object);
                return core::ptr::null_mut();
            }
            if index < 0 {
                t.ob_base.ob_size = item;
                tstate::set_string(exceptions::type_error(), b"invalid template");
                unicode::decref(object);
                return core::ptr::null_mut();
            }
            let mut literal = list::PyList_GetItem(template, 2 * item + 2);
            let empty_literal = (unicode::check(literal) && unicode::get_length(literal) == 0)
                || (rt_bytes::check(literal) && rt_bytes::size(literal) == 0);
            if empty_literal {
                literal = core::ptr::null_mut();
                t.chunks -= 1;
            }
            if !literal.is_null() {
                unicode::incref(literal);
            }
            let entry = t.items.as_mut_ptr().add(item as usize);
            (*entry).index = index;
            (*entry).literal = literal;
        }
        object
    }
}

// ---------------------------------------------------------- the validator

/// `_validate_charset` (sre.c): the body of an `IN` operator. The
/// word counts mirror C's; a range outside the array is a structure the
/// skip word could not have described, which fails. Opcode words are
/// rewritten into `out` as they are checked.
fn validate_charset(code: &[u32], out: &mut [u32], range: core::ops::Range<i64>) -> bool {
    if range.start < 0 || range.end > code.len() as i64 || range.start > range.end {
        return false;
    }
    let end = range.end;
    let mut i = range.start;
    while i < end {
        let op = code[i as usize];
        translate_op(out, i, op);
        i += 1;
        match op {
            OP_NEGATE => {}
            OP_LITERAL => i += 1,
            OP_RANGE | OP_RANGE_UNI_IGNORE => i += 2,
            OP_CHARSET => {
                let offset = 256 / 32;
                if i + offset > end {
                    return false;
                }
                i += offset;
            }
            OP_BIGCHARSET => {
                if i >= end {
                    return false;
                }
                let blocks = code[i as usize];
                i += 1;
                let offset = 256 / 4;
                if i + offset > end {
                    return false;
                }
                // Each byte of the 256-byte block table names a block
                // below `blocks`; the words are read as bytes.
                // SAFETY: the words are within the code slice.
                let bytes = unsafe {
                    core::slice::from_raw_parts(code.as_ptr().cast::<u8>(), code.len() * 4)
                };
                for b in &bytes[(i * 4) as usize..(i * 4 + 256) as usize] {
                    if *b as u32 >= blocks {
                        return false;
                    }
                }
                i += offset;
                let offset = blocks as i64 * (256 / 32);
                if i + offset > end {
                    return false;
                }
                i += offset;
            }
            OP_CATEGORY => {
                if i >= end {
                    return false;
                }
                let arg = code[i as usize];
                i += 1;
                if !matches!(
                    arg,
                    CATEGORY_DIGIT
                        | CATEGORY_NOT_DIGIT
                        | CATEGORY_SPACE
                        | CATEGORY_NOT_SPACE
                        | CATEGORY_WORD
                        | CATEGORY_NOT_WORD
                        | CATEGORY_LINEBREAK
                        | CATEGORY_NOT_LINEBREAK
                        | CATEGORY_LOC_WORD
                        | CATEGORY_LOC_NOT_WORD
                        | CATEGORY_UNI_DIGIT
                        | CATEGORY_UNI_NOT_DIGIT
                        | CATEGORY_UNI_SPACE
                        | CATEGORY_UNI_NOT_SPACE
                        | CATEGORY_UNI_WORD
                        | CATEGORY_UNI_NOT_WORD
                        | CATEGORY_UNI_LINEBREAK
                        | CATEGORY_UNI_NOT_LINEBREAK
                ) {
                    return false;
                }
            }
            _ => return false,
        }
    }
    i == end
}

/// `_validate_inner`'s result: okay, okay-and-ends-in-JUMP, or failure.
#[derive(PartialEq, Clone, Copy)]
enum Validated {
    Ok,
    Jump,
    Fail,
}

/// `_validate_inner` (sre.c): the opcode structure walk. Positions run
/// as offsets in C's pointer arithmetic (i64), so the degenerate skip
/// words that wrap C's unsigned math fail instead of panicking. Opcode
/// words are rewritten into `out` (the engine's numbering) as they are
/// checked; operand words keep their values. `engine_gap` records the
/// first opcode sre-engine 0.4.3 does not implement.
fn validate_inner(
    code: &[u32],
    out: &mut [u32],
    range: core::ops::Range<i64>,
    groups: usize,
    engine_gap: &mut Option<&'static str>,
) -> Validated {
    let end = range.end.min(code.len() as i64);
    let mut i = range.start;
    if i > end {
        return Validated::Fail;
    }
    while i < end {
        if i < 0 {
            return Validated::Fail;
        }
        let op = code[i as usize];
        translate_op(out, i, op);
        i += 1;
        match op {
            OP_MARK => {
                if i >= end {
                    return Validated::Fail;
                }
                let arg = code[i as usize] as i64;
                i += 1;
                if arg > 2 * groups as i64 + 1 {
                    return Validated::Fail;
                }
            }
            OP_LITERAL
            | OP_NOT_LITERAL
            | OP_LITERAL_IGNORE
            | OP_NOT_LITERAL_IGNORE
            | OP_LITERAL_UNI_IGNORE
            | OP_NOT_LITERAL_UNI_IGNORE
            | OP_LITERAL_LOC_IGNORE
            | OP_NOT_LITERAL_LOC_IGNORE => {
                if i >= end {
                    return Validated::Fail;
                }
                i += 1;
            }
            OP_SUCCESS | OP_FAILURE => {}
            OP_AT => {
                if i >= end {
                    return Validated::Fail;
                }
                let arg = code[i as usize];
                i += 1;
                if !matches!(
                    arg,
                    AT_BEGINNING
                        | AT_BEGINNING_STRING
                        | AT_BEGINNING_LINE
                        | AT_END
                        | AT_END_LINE
                        | AT_END_STRING
                        | AT_BOUNDARY
                        | AT_NON_BOUNDARY
                        | AT_LOC_BOUNDARY
                        | AT_LOC_NON_BOUNDARY
                        | AT_UNI_BOUNDARY
                        | AT_UNI_NON_BOUNDARY
                ) {
                    return Validated::Fail;
                }
            }
            OP_ANY | OP_ANY_ALL => {}
            OP_IN | OP_IN_IGNORE | OP_IN_UNI_IGNORE | OP_IN_LOC_IGNORE => {
                let Some(skip) = skip_arg(code, i, end, 0) else {
                    return Validated::Fail;
                };
                i += 1;
                // Stop 1 before the end; the FAILURE word is checked below.
                if !validate_charset(code, out, i..i + skip as i64 - 2) {
                    return Validated::Fail;
                }
                if code[(i + skip as i64 - 2) as usize] != OP_FAILURE {
                    return Validated::Fail;
                }
                translate_op(out, i + skip as i64 - 2, OP_FAILURE);
                i += skip as i64 - 1;
            }
            OP_INFO => {
                let Some(skip) = skip_arg(code, i, end, 0) else {
                    return Validated::Fail;
                };
                let newcode = i + skip as i64;
                i += 1;
                // <flags> <min> <max>
                if i + 3 > end {
                    return Validated::Fail;
                }
                let flags = code[i as usize];
                i += 3;
                if flags & !(INFO_PREFIX | INFO_LITERAL | INFO_CHARSET) != 0 {
                    return Validated::Fail;
                }
                if flags & (INFO_PREFIX | INFO_CHARSET) == INFO_PREFIX | INFO_CHARSET {
                    return Validated::Fail;
                }
                if flags & INFO_LITERAL != 0 && flags & INFO_PREFIX == 0 {
                    return Validated::Fail;
                }
                if flags & INFO_PREFIX != 0 {
                    // <prefix_len> <skip>, then the prefix and its overlap
                    // table.
                    if i + 2 > end {
                        return Validated::Fail;
                    }
                    let prefix_len = code[i as usize] as i64;
                    i += 2;
                    if newcode < i || prefix_len > newcode - i {
                        return Validated::Fail;
                    }
                    i += prefix_len;
                    if newcode < i || prefix_len > newcode - i {
                        return Validated::Fail;
                    }
                    for k in 0..prefix_len {
                        if code[(i + k) as usize] as i64 >= prefix_len {
                            return Validated::Fail;
                        }
                    }
                    i += prefix_len;
                }
                if flags & INFO_CHARSET != 0 {
                    if newcode < 1 || !validate_charset(code, out, i..newcode - 1) {
                        return Validated::Fail;
                    }
                    if code[(newcode - 1) as usize] != OP_FAILURE {
                        return Validated::Fail;
                    }
                    translate_op(out, newcode - 1, OP_FAILURE);
                    i = newcode;
                } else if i != newcode {
                    return Validated::Fail;
                }
            }
            OP_BRANCH => {
                let mut target: Option<i64> = None;
                loop {
                    let Some(skip) = skip_arg(code, i, end, 0) else {
                        return Validated::Fail;
                    };
                    i += 1;
                    if skip == 0 {
                        break;
                    }
                    // Stop 2 before the end; the JUMP below is checked.
                    if matches!(
                        validate_inner(code, out, i..i + skip as i64 - 3, groups, engine_gap),
                        Validated::Fail
                    ) {
                        return Validated::Fail;
                    }
                    i += skip as i64 - 3;
                    if i >= end {
                        return Validated::Fail;
                    }
                    let branch_op = code[i as usize];
                    if branch_op != OP_JUMP {
                        return Validated::Fail;
                    }
                    translate_op(out, i, branch_op);
                    i += 1;
                    let Some(jump) = skip_arg(code, i, end, 0) else {
                        return Validated::Fail;
                    };
                    i += 1;
                    let here = i + jump as i64 - 1;
                    match target {
                        None => target = Some(here),
                        Some(t) if t == here => {}
                        Some(_) => return Validated::Fail,
                    }
                }
                if target != Some(i) {
                    return Validated::Fail;
                }
            }
            OP_REPEAT_ONE | OP_MIN_REPEAT_ONE | OP_POSSESSIVE_REPEAT_ONE => {
                if op == OP_POSSESSIVE_REPEAT_ONE {
                    *engine_gap = Some("possessive repeats");
                }
                let Some(skip) = skip_arg(code, i, end, 0) else {
                    return Validated::Fail;
                };
                i += 1;
                // <min> <max>
                if i + 2 > end {
                    return Validated::Fail;
                }
                let min = code[i as usize];
                let max = code[(i + 1) as usize];
                i += 2;
                if min > max {
                    return Validated::Fail;
                }
                if matches!(
                    validate_inner(code, out, i..i + skip as i64 - 4, groups, engine_gap),
                    Validated::Fail
                ) {
                    return Validated::Fail;
                }
                i += skip as i64 - 4;
                if i >= end || code[i as usize] != OP_SUCCESS {
                    return Validated::Fail;
                }
                translate_op(out, i, OP_SUCCESS);
                i += 1;
            }
            OP_REPEAT | OP_POSSESSIVE_REPEAT => {
                if op == OP_POSSESSIVE_REPEAT {
                    *engine_gap = Some("possessive repeats");
                }
                let Some(skip) = skip_arg(code, i, end, 0) else {
                    return Validated::Fail;
                };
                i += 1;
                // <min> <max>
                if i + 2 > end {
                    return Validated::Fail;
                }
                let min = code[i as usize];
                let max = code[(i + 1) as usize];
                i += 2;
                if min > max {
                    return Validated::Fail;
                }
                if matches!(
                    validate_inner(code, out, i..i + skip as i64 - 3, groups, engine_gap),
                    Validated::Fail
                ) {
                    return Validated::Fail;
                }
                i += skip as i64 - 3;
                if i >= end {
                    return Validated::Fail;
                }
                let tail = code[i as usize];
                translate_op(out, i, tail);
                i += 1;
                if op == OP_POSSESSIVE_REPEAT {
                    if tail != OP_SUCCESS {
                        return Validated::Fail;
                    }
                } else if tail != OP_MAX_UNTIL && tail != OP_MIN_UNTIL {
                    return Validated::Fail;
                }
            }
            OP_ATOMIC_GROUP => {
                *engine_gap = Some("atomic groups");
                let Some(skip) = skip_arg(code, i, end, 0) else {
                    return Validated::Fail;
                };
                i += 1;
                if matches!(
                    validate_inner(code, out, i..i + skip as i64 - 2, groups, engine_gap),
                    Validated::Fail
                ) {
                    return Validated::Fail;
                }
                i += skip as i64 - 2;
                if i >= end || code[i as usize] != OP_SUCCESS {
                    return Validated::Fail;
                }
                translate_op(out, i, OP_SUCCESS);
                i += 1;
            }
            OP_GROUPREF | OP_GROUPREF_IGNORE | OP_GROUPREF_UNI_IGNORE | OP_GROUPREF_LOC_IGNORE => {
                if i >= end {
                    return Validated::Fail;
                }
                let arg = code[i as usize] as usize;
                i += 1;
                if arg >= groups {
                    return Validated::Fail;
                }
            }
            OP_GROUPREF_EXISTS => {
                // '(?(group)then|else)': the then part's skip counts from
                // the group argument, and a trailing JUMP means an else
                // part follows at the jump's target.
                if i >= end {
                    return Validated::Fail;
                }
                let arg = code[i as usize] as usize;
                i += 1;
                if arg >= groups {
                    return Validated::Fail;
                }
                let mut skip = match skip_arg(code, i, end, 1) {
                    Some(skip) => skip,
                    None => return Validated::Fail,
                };
                let mut rc =
                    validate_inner(code, out, i + 1..i + skip as i64 - 1, groups, engine_gap);
                if rc == Validated::Jump {
                    i += skip as i64 - 2;
                    skip = match skip_arg(code, i, end, 0) {
                        Some(skip) => skip,
                        None => return Validated::Fail,
                    };
                    i += 1;
                    rc = validate_inner(code, out, i..i + skip as i64 - 1, groups, engine_gap);
                }
                if rc != Validated::Ok {
                    return Validated::Fail;
                }
                i += skip as i64 - 1;
            }
            OP_ASSERT | OP_ASSERT_NOT => {
                let Some(skip) = skip_arg(code, i, end, 0) else {
                    return Validated::Fail;
                };
                // <width> for lookbehind; C reads it and backs up, so the
                // ranges below count from the width word.
                if i + 1 >= end {
                    return Validated::Fail;
                }
                i += 1;
                if matches!(
                    validate_inner(code, out, i + 1..i + skip as i64 - 2, groups, engine_gap),
                    Validated::Fail
                ) {
                    return Validated::Fail;
                }
                i += skip as i64 - 2;
                if i >= end || code[i as usize] != OP_SUCCESS {
                    return Validated::Fail;
                }
                translate_op(out, i, OP_SUCCESS);
                i += 1;
            }
            OP_JUMP => {
                if i + 1 != end {
                    return Validated::Fail;
                }
                return Validated::Jump;
            }
            _ => return Validated::Fail,
        }
    }
    Validated::Ok
}

/// Rewrite the opcode word the walk just checked at `at` into the
/// engine's numbering. `None` opcodes never reach here: the walk either
/// matched them against a 3.12 constant or recorded the engine gap.
fn translate_op(out: &mut [u32], at: i64, op: u32) {
    if let Some(engine) = engine_op(op) {
        out[at as usize] = engine;
    }
}

/// `GET_SKIP_ADJ` (sre.c): the skip word and its bounds check, on the
/// word at `i`.
fn skip_arg(code: &[u32], i: i64, end: i64, adj: u64) -> Option<u64> {
    if i < 0 || i >= end || i >= code.len() as i64 {
        return None;
    }
    let skip = code[i as usize] as u64;
    if skip.wrapping_sub(adj) > (end - i) as u64 {
        return None;
    }
    Some(skip)
}

/// `_validate` (sre.c): the outer structure check over the whole code
/// array, with the engine-gap report sre-engine 0.4.3 leaves. `out` is
/// the same length as `code` and receives the engine-numbered words.
fn validate(code: &[u32], out: &mut [u32], groups: isize) -> Result<(), ValidationError> {
    if groups < 0
        || groups as u64 > MAXGROUPS as u64
        || code.is_empty()
        || out.len() != code.len()
        || code[code.len() - 1] != OP_SUCCESS
    {
        return Err(ValidationError::Invalid);
    }
    let mut engine_gap = None;
    if matches!(
        validate_inner(
            code,
            out,
            0..code.len() as i64 - 1,
            groups as usize,
            &mut engine_gap
        ),
        Validated::Fail
    ) {
        return Err(ValidationError::Invalid);
    }
    translate_op(out, code.len() as i64 - 1, OP_SUCCESS);
    match engine_gap {
        None => Ok(()),
        Some(feature) => Err(ValidationError::EngineGap(feature)),
    }
}

enum ValidationError {
    Invalid,
    EngineGap(&'static str),
}

// ------------------------------------------------------ module functions

/// `_sre.compile`: the `PatternObject` for a code array `_compiler.py`
/// produced. The code validates (CPython's `_validate`) before the
/// object is built; opcodes the engine cannot run reject up front with
/// a typed error instead of a mid-match engine panic.
///
/// # Safety
///
/// `pattern` must be a valid object or `None`; `code` a list of ints,
/// `groupindex` a dict and `indexgroup` a tuple.
pub unsafe fn compile(
    pattern: *mut PyObject,
    flags: c_int,
    code: *mut PyObject,
    groups: isize,
    groupindex: *mut PyObject,
    indexgroup: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the list reads stay within the code argument.
    unsafe {
        let n = list::PyList_Size(code);
        if n < 0 {
            return core::ptr::null_mut();
        }
        let object = PyType_GenericAlloc(&raw mut Pattern_Type, n);
        if object.is_null() {
            return core::ptr::null_mut();
        }
        let p = &mut *object.cast::<PatternObject>();
        p.ob_base.ob_size = n;
        p.weakreflist = core::ptr::null_mut();
        p.groupindex = core::ptr::null_mut();
        p.indexgroup = core::ptr::null_mut();
        p.pattern = core::ptr::null_mut();
        p.codesize = n;
        let mut words: Vec<u32> = Vec::with_capacity(n as usize);
        let mut failed = false;
        for i in 0..n {
            let item = list::PyList_GetItem(code, i);
            let value = long::PyLong_AsUnsignedLongLong(item);
            if value == u64::MAX && !tstate::occurred().is_null() {
                failed = true;
                break;
            }
            if value > u32::MAX as u64 {
                tstate::set_string(
                    exceptions::overflow_error(),
                    b"regular expression code size limit exceeded",
                );
                failed = true;
                break;
            }
            words.push(value as u32);
        }
        if !failed && pattern != crate::object::singletons::none() {
            // getstring decides the subject flavour the pattern
            // accepts; get_subject reproduces its errors.
            let mut probe = None;
            // SAFETY: the probe reads the pattern object only.
            let resolved = get_subject(-1, pattern, &mut probe);
            release_hold(&mut probe);
            match resolved {
                Some(_) => p.isbytes = if unicode::check(pattern) { 0 } else { 1 },
                None => failed = true,
            }
        }
        if !failed {
            unicode::incref(pattern);
            p.pattern = pattern;
            p.flags = flags;
            p.groups = groups;
            if dict::PyDict_Size(groupindex) > 0 {
                unicode::incref(groupindex);
                p.groupindex = groupindex;
                if tuple::PyTuple_Size(indexgroup) > 0 {
                    unicode::incref(indexgroup);
                    p.indexgroup = indexgroup;
                }
            }
            // The walk rewrites the opcode words in place; the operand
            // words start as exact copies.
            let mut engine_words: Vec<u32> = words.clone();
            match validate(&words, &mut engine_words, groups) {
                Ok(()) => {
                    // SAFETY: the code words are the object's variable
                    // tail, in the engine's opcode numbering.
                    let tail = core::slice::from_raw_parts_mut(p.code.as_mut_ptr(), n as usize);
                    tail.copy_from_slice(&engine_words);
                }
                Err(ValidationError::Invalid) => {
                    tstate::set_string(exceptions::runtime_error(), b"invalid SRE code");
                    failed = true;
                }
                Err(ValidationError::EngineGap(feature)) => {
                    tstate::set_string(
                        exceptions::system_error(),
                        format!("coil: the sre-engine 0.4.3 backend does not implement {feature}")
                            .as_bytes(),
                    );
                    failed = true;
                }
            }
        }
        if failed {
            unicode::decref(object);
            return core::ptr::null_mut();
        }
        object
    }
}

/// `_sre.getcodesize`.
pub fn getcodesize() -> *mut PyObject {
    long::PyLong_FromLong(CODESIZE)
}

/// The `character: int` conversion the clinic signatures share: an
/// exact int in C's `int` range, else CPython's message.
///
/// # Safety
///
/// `object` must be a valid object.
unsafe fn character_arg(object: *mut PyObject) -> Option<i64> {
    // SAFETY: the conversion reads the int object only.
    unsafe {
        if !long::is_long(object) {
            let name = util::cstr((*header::type_of(object)).tp_name);
            tstate::set_string(
                exceptions::type_error(),
                format!("'{name}' object cannot be interpreted as an integer").as_bytes(),
            );
            return None;
        }
        let value = long::PyLong_AsLongLong(object);
        if !tstate::occurred().is_null() {
            return None;
        }
        if value < c_int::MIN as i64 || value > c_int::MAX as i64 {
            tstate::set_string(
                exceptions::overflow_error(),
                b"int too large to convert to C int",
            );
            return None;
        }
        Some(value)
    }
}

/// `_sre.ascii_iscased`.
///
/// # Safety
///
/// `object` must be a valid object.
pub unsafe fn ascii_iscased(object: *mut PyObject) -> *mut PyObject {
    // SAFETY: the conversion reads the int object only.
    unsafe {
        let Some(ch) = character_arg(object) else {
            return core::ptr::null_mut();
        };
        let ch = ch as u32;
        crate::types::bool_::PyBool_FromLong(
            (ch < 128 && (ch as u8).is_ascii_alphabetic()) as c_int,
        )
    }
}

/// `_sre.unicode_iscased`.
///
/// # Safety
///
/// `object` must be a valid object.
pub unsafe fn unicode_iscased(object: *mut PyObject) -> *mut PyObject {
    // SAFETY: the conversion reads the int object only.
    unsafe {
        let Some(ch) = character_arg(object) else {
            return core::ptr::null_mut();
        };
        let ch = ch as u32;
        crate::types::bool_::PyBool_FromLong(
            (ch != unicode::to_lowercase(ch) || ch != unicode::to_uppercase(ch)) as c_int,
        )
    }
}

/// `_sre.ascii_tolower`.
///
/// # Safety
///
/// `object` must be a valid object.
pub unsafe fn ascii_tolower(object: *mut PyObject) -> *mut PyObject {
    // SAFETY: the conversion reads the int object only.
    unsafe {
        let Some(ch) = character_arg(object) else {
            return core::ptr::null_mut();
        };
        long::PyLong_FromLong((ch as u8).to_ascii_lowercase() as c_long)
    }
}

/// `_sre.unicode_tolower`.
///
/// # Safety
///
/// `object` must be a valid object.
pub unsafe fn unicode_tolower(object: *mut PyObject) -> *mut PyObject {
    // SAFETY: the conversion reads the int object only.
    unsafe {
        let Some(ch) = character_arg(object) else {
            return core::ptr::null_mut();
        };
        long::PyLong_FromLong(unicode::to_lowercase(ch as u32) as c_long)
    }
}

/// `Pattern.finditer`: CPython wraps the scanner's `search` in a call
/// iterator, which needs the callable-object machinery.
///
/// # Safety
///
/// `pattern` must be a pattern object and `string` a valid object.
pub unsafe fn pattern_finditer(
    pattern: *mut PyObject,
    string: *mut PyObject,
    pos: isize,
    endpos: isize,
) -> *mut PyObject {
    // SAFETY: the scanner construction reads the pattern and subject.
    unsafe {
        let _ = pattern_scanner(pattern, string, pos, endpos);
        tstate::set_string(
            exceptions::system_error(),
            b"coil: building the finditer iterator needs the call-iterator machinery",
        );
        core::ptr::null_mut()
    }
}
