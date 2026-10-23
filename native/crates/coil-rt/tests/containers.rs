//! Oracle-driven tests for the container types (spec sections 8.2, 8.5,
//! 8.6): layouts, C API contracts, iteration order and hash values
//! checked against CPython 3.12 through `tools/gen_container_oracle.py`,
//! and the multi-threaded hammer test for the per-object locks.

use std::fs;
use std::path::Path;
use std::sync::Once;
use std::sync::atomic::{AtomicU8, Ordering};

use coil_capi_sys::{PyObject, PyTypeObject};
use python312::object::{exceptions, header, typeobject};
use python312::tstate;
use python312::types::{dict, float_, list, long, set, tuple};

const TUPLE_TYPE: *mut PyTypeObject = &raw mut tuple::PyTuple_Type;
const LIST_TYPE: *mut PyTypeObject = &raw mut list::PyList_Type;
const DICT_TYPE: *mut PyTypeObject = &raw mut dict::PyDict_Type;
const SET_TYPE: *mut PyTypeObject = &raw mut set::PySet_Type;
const FROZENSET_TYPE: *mut PyTypeObject = &raw mut set::PyFrozenSet_Type;

static INIT: Once = Once::new();

fn init() {
    INIT.call_once(|| assert_eq!(typeobject::init_static_types(), 0));
}

/// The data file's rows with their 1-based line numbers.
fn rows(name: &str) -> Vec<(usize, Vec<String>)> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name);
    let text = fs::read_to_string(path).unwrap();
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.is_empty() && !line.starts_with('#'))
        .map(|(i, line)| (i + 1, line.split('\t').map(str::to_owned).collect()))
        .collect()
}

fn int(value: i64) -> *mut PyObject {
    long::PyLong_FromLongLong(value)
}

fn int_value(object: *mut PyObject) -> i64 {
    // SAFETY: every object stored by these tests is an int.
    unsafe { long::PyLong_AsLongLong(object) }
}

fn as_float(token: &str) -> *mut PyObject {
    let bits = u64::from_str_radix(token.strip_prefix("f:").unwrap(), 16).unwrap();
    float_::PyFloat_FromDouble(f64::from_bits(bits))
}

/// One element token of the shared encoding.
fn scalar(token: &str) -> *mut PyObject {
    match token.strip_prefix("f:") {
        Some(_) => as_float(token),
        None => int(token.parse().unwrap()),
    }
}

fn fill_tuple(object: *mut PyObject, items: &[*mut PyObject]) -> *mut PyObject {
    for (i, item) in items.iter().enumerate() {
        // SAFETY: the tuple was built with `items.len()` slots.
        assert_eq!(
            unsafe { tuple::PyTuple_SetItem(object, i as isize, *item) },
            0
        );
    }
    object
}

/// The tuple encoded by `tokens`: ints, `f:`-prefixed floats and `(`/`)`
/// around a nested tuple (the generator's encoding).
fn build_tuple(tokens: &[&str]) -> *mut PyObject {
    fn walk(tokens: &[&str], next: &mut usize) -> *mut PyObject {
        let mut items = Vec::new();
        while *next < tokens.len() {
            match tokens[*next] {
                "(" => {
                    *next += 1;
                    items.push(walk(tokens, next));
                }
                ")" => {
                    *next += 1;
                    return fill_tuple(tuple::PyTuple_New(items.len() as isize), &items);
                }
                token => {
                    *next += 1;
                    items.push(scalar(token));
                }
            }
        }
        fill_tuple(tuple::PyTuple_New(items.len() as isize), &items)
    }
    walk(tokens, &mut 0)
}

/// The dict's keys in iteration order, as the oracle writes them.
fn dict_keys(mp: *mut PyObject) -> Vec<i64> {
    let mut keys = Vec::new();
    let mut pos = 0isize;
    let mut key = std::ptr::null_mut::<PyObject>();
    let mut value = std::ptr::null_mut::<PyObject>();
    // SAFETY: `mp` is a dict object and the cursor starts at 0.
    while unsafe { dict::PyDict_Next(mp, &mut pos, &mut key, &mut value) } != 0 {
        keys.push(int_value(key));
    }
    keys
}

/// The set's elements in iteration order, walked through its iterator
/// slots the way `for` does.
fn set_items(anyset: *mut PyObject) -> Vec<i64> {
    // SAFETY: `anyset` is a set object readied by init.
    let iterator = unsafe { ((*header::type_of(anyset)).tp_iter.unwrap())(anyset) };
    assert!(!iterator.is_null());
    // SAFETY: the iterator's type carries `tp_iternext`.
    let next = unsafe { (*header::type_of(iterator)).tp_iternext.unwrap() };
    let mut items = Vec::new();
    loop {
        // SAFETY: the iterator is a set iterator over `anyset`.
        let item = unsafe { next(iterator) };
        if item.is_null() {
            return items;
        }
        items.push(int_value(item));
    }
}

/// The set's lock byte at the layout's tail offset.
fn lock_byte(object: *mut PyObject, offset: usize) -> u8 {
    // SAFETY: the offset is a fixed field of the container layout.
    unsafe { (*((object as *const u8).add(offset) as *const AtomicU8)).load(Ordering::Acquire) }
}

fn assert_error(expected: *mut PyObject) {
    assert!(
        core::ptr::eq(tstate::occurred(), expected),
        "expected a different exception"
    );
    tstate::clear();
}

#[test]
fn container_layouts_match_the_abi() {
    init();
    use coil_rt_abi::cp312;

    assert_eq!(
        std::mem::offset_of!(tuple::PyTupleObject, ob_item),
        cp312::tuple::OB_ITEM
    );
    assert_eq!(
        std::mem::size_of::<tuple::PyTupleObject>(),
        cp312::tuple::SIZEOF
    );
    // SAFETY: the static type was readied by init.
    unsafe {
        assert_eq!((*TUPLE_TYPE).tp_basicsize, cp312::tuple::OB_ITEM as isize);
        assert_eq!(
            (*TUPLE_TYPE).tp_itemsize,
            std::mem::size_of::<*mut PyObject>() as isize
        );
    }

    assert_eq!(
        std::mem::offset_of!(list::PyListObject, ob_item),
        cp312::list::OB_ITEM
    );
    assert_eq!(
        std::mem::offset_of!(list::PyListObject, ob_allocated),
        cp312::list::ALLOCATED
    );
    assert_eq!(list::LOCK, cp312::list::SIZEOF);
    assert_eq!(
        std::mem::size_of::<list::PyListObject>(),
        cp312::list::SIZEOF + 8
    );
    // SAFETY: the static type was readied by init.
    unsafe {
        assert_eq!(
            (*LIST_TYPE).tp_basicsize,
            (cp312::list::SIZEOF + 8) as isize
        );
    }

    assert_eq!(
        std::mem::offset_of!(dict::PyDictObject, ma_used),
        cp312::dict::MA_USED
    );
    assert_eq!(
        std::mem::offset_of!(dict::PyDictObject, ma_version_tag),
        cp312::dict::MA_VERSION_TAG
    );
    assert_eq!(dict::LOCK, 32);
    assert_eq!(
        std::mem::size_of::<dict::PyDictObject>(),
        cp312::dict::SIZEOF
    );
    // SAFETY: the static type was readied by init.
    unsafe {
        assert_eq!((*DICT_TYPE).tp_basicsize, cp312::dict::SIZEOF as isize);
    }

    assert_eq!(
        std::mem::offset_of!(set::PySetObject, fill),
        cp312::set::FILL
    );
    assert_eq!(
        std::mem::offset_of!(set::PySetObject, used),
        cp312::set::USED
    );
    assert_eq!(set::LOCK, cp312::set::SIZEOF);
    assert_eq!(
        std::mem::size_of::<set::PySetObject>(),
        cp312::set::SIZEOF + 8
    );
    // SAFETY: the static types were readied by init.
    unsafe {
        assert_eq!((*SET_TYPE).tp_basicsize, (cp312::set::SIZEOF + 8) as isize);
        assert_eq!(
            (*FROZENSET_TYPE).tp_basicsize,
            (cp312::set::SIZEOF + 8) as isize
        );
    }
}

#[test]
fn inline_item_access_reads_the_exact_layouts() {
    init();

    let a = int(11);
    let b = int(22);
    let object = build_tuple(&["33", "44"]);
    let tuple_ptr = object.cast::<tuple::PyTupleObject>();
    // SAFETY: the inline item array is within the tuple's allocation.
    unsafe {
        *(*tuple_ptr).ob_item.as_mut_ptr().add(0) = a;
        *(*tuple_ptr).ob_item.as_mut_ptr().add(1) = b;
        assert!(core::ptr::eq(tuple::PyTuple_GetItem(object, 0), a));
        assert!(core::ptr::eq(tuple::PyTuple_GetItem(object, 1), b));
        assert_eq!((*tuple_ptr).ob_base.ob_size, 2);
    }

    let list_object = list::PyList_New(2);
    let list_ptr = list_object.cast::<list::PyListObject>();
    // SAFETY: the item array holds two slots in a fresh list.
    unsafe {
        assert_eq!(list::PyList_SetItem(list_object, 0, a), 0);
        assert_eq!(list::PyList_SetItem(list_object, 1, b), 0);
        assert!(core::ptr::eq(*(*list_ptr).ob_item.add(0), a));
        assert!(core::ptr::eq(*(*list_ptr).ob_item.add(1), b));
        assert!(core::ptr::eq(list::PyList_GetItem(list_object, 1), b));
    }
}

#[test]
fn tuple_hash_matches_cpython() {
    init();
    for (line, fields) in rows("tuple_hash.tsv") {
        let tokens: Vec<&str> = fields[0].split_whitespace().collect();
        let object = build_tuple(&tokens);
        assert!(!object.is_null(), "tuple build failed on line {line}");
        // SAFETY: the static type's slot is `tuplehash`.
        let hash = unsafe { (*TUPLE_TYPE).tp_hash.unwrap()(object) };
        assert_eq!(hash.to_string(), fields[1], "line {line}");
    }
}

#[test]
fn dict_iteration_order_matches_cpython() {
    init();
    for (line, fields) in rows("dict_order.tsv") {
        let mp = dict::PyDict_New();
        assert!(!mp.is_null(), "line {line}");
        for op in fields[0].split(';') {
            let mut parts = op.split(':');
            match parts.next().unwrap() {
                "i" => {
                    let key = int(parts.next().unwrap().parse().unwrap());
                    let value = int(parts.next().unwrap().parse().unwrap());
                    // SAFETY: `mp` is a dict object.
                    assert_eq!(unsafe { dict::PyDict_SetItem(mp, key, value) }, 0);
                }
                "d" => {
                    let key = int(parts.next().unwrap().parse().unwrap());
                    // SAFETY: `mp` is a dict object.
                    if unsafe { dict::PyDict_DelItem(mp, key) } != 0 {
                        assert_error(exceptions::key_error());
                    }
                }
                "c" => unsafe { dict::PyDict_Clear(mp) },
                other => panic!("unknown op {other} on line {line}"),
            }
        }
        let got: Vec<String> = dict_keys(mp)
            .into_iter()
            .map(|key| key.to_string())
            .collect();
        assert_eq!(got.join(","), fields[1], "line {line}");
    }
}

#[test]
fn set_iteration_order_matches_cpython() {
    init();
    for (line, fields) in rows("set_order.tsv") {
        // SAFETY: a null iterable builds the empty set.
        let so = unsafe { set::PySet_New(std::ptr::null_mut()) };
        assert!(!so.is_null(), "line {line}");
        for op in fields[0].split(';') {
            match op.split(':').next().unwrap() {
                "a" => {
                    let key = int(op.split(':').nth(1).unwrap().parse().unwrap());
                    // SAFETY: `so` is a set object.
                    assert_eq!(unsafe { set::PySet_Add(so, key) }, 0);
                }
                "x" => {
                    let key = int(op.split(':').nth(1).unwrap().parse().unwrap());
                    // SAFETY: `so` is a set object.
                    unsafe { set::PySet_Discard(so, key) };
                }
                "p" => {
                    // SAFETY: `so` is a non-empty set object.
                    assert!(!unsafe { set::PySet_Pop(so) }.is_null());
                }
                "c" => {
                    // SAFETY: `so` is a set object.
                    assert_eq!(unsafe { set::PySet_Clear(so) }, 0);
                }
                other => panic!("unknown op {other} on line {line}"),
            }
        }
        let got: Vec<String> = set_items(so)
            .into_iter()
            .map(|key| key.to_string())
            .collect();
        assert_eq!(got.join(","), fields[1], "line {line}");
    }
}

#[test]
fn frozenset_hash_matches_cpython() {
    init();
    for (line, fields) in rows("frozenset_hash.tsv") {
        let tokens: Vec<&str> = fields[0].split_whitespace().collect();
        let items: Vec<*mut PyObject> = tokens
            .iter()
            .map(|token| int(token.parse().unwrap()))
            .collect();
        let source = fill_tuple(tuple::PyTuple_New(items.len() as isize), &items);
        // SAFETY: the source is a tuple of ints.
        let fs = unsafe { set::PyFrozenSet_New(source) };
        assert!(!fs.is_null(), "line {line}");
        // SAFETY: the static type's slot is `frozenset_hash`.
        let hash = unsafe { (*FROZENSET_TYPE).tp_hash.unwrap()(fs) };
        assert_eq!(hash.to_string(), fields[1], "line {line}");
    }
}

#[test]
fn tuple_entry_points_follow_the_cpython_contract() {
    init();

    let empty_a = tuple::PyTuple_New(0);
    let empty_b = tuple::PyTuple_New(0);
    assert!(core::ptr::eq(empty_a, empty_b), "the empty tuple is shared");

    let object = build_tuple(&["5", "6", "7"]);
    // SAFETY: the object is a tuple.
    unsafe {
        assert_eq!(tuple::PyTuple_Size(object), 3);

        let whole = tuple::PyTuple_GetSlice(object, 0, 3);
        assert!(core::ptr::eq(whole, object), "whole slices are the tuple");
        let middle = tuple::PyTuple_GetSlice(object, 1, 3);
        assert_eq!(tuple::PyTuple_Size(middle), 2);
        assert_eq!(int_value(tuple::PyTuple_GetItem(middle, 0)), 6);

        assert!(tuple::PyTuple_GetItem(object, 3).is_null());
        assert_error(exceptions::index_error());
    }

    // A caller-owned tuple can be resized in both directions.
    let mut grown = build_tuple(&["8", "9"]);
    // SAFETY: `grown` is an exact, singly referenced tuple.
    unsafe {
        assert_eq!(tuple::_PyTuple_Resize(&mut grown, 4), 0);
        assert_eq!(tuple::PyTuple_Size(grown), 4);
        assert_eq!(tuple::PyTuple_SetItem(grown, 2, int(10)), 0);
        assert_eq!(tuple::PyTuple_SetItem(grown, 3, int(11)), 0);
        assert_eq!(tuple::_PyTuple_Resize(&mut grown, 2), 0);
        assert_eq!(tuple::PyTuple_Size(grown), 2);
        assert_eq!(int_value(tuple::PyTuple_GetItem(grown, 1)), 9);
    }
}

#[test]
fn list_entry_points_follow_the_cpython_contract() {
    init();

    let object = list::PyList_New(0);
    // SAFETY: the object is a list throughout.
    unsafe {
        assert_eq!(list::PyList_Size(object), 0);
        for i in 0..8 {
            assert_eq!(list::PyList_Append(object, int(i)), 0);
        }
        assert_eq!(list::PyList_Size(object), 8);
        assert_eq!(int_value(list::PyList_GetItem(object, 7)), 7);

        assert!(list::PyList_GetItem(object, 8).is_null());
        assert_error(exceptions::index_error());
        assert_eq!(list::PyList_SetItem(object, 8, int(0)), -1);
        assert_error(exceptions::index_error());

        assert_eq!(list::PyList_Insert(object, 0, int(100)), 0);
        assert_eq!(int_value(list::PyList_GetItem(object, 0)), 100);
        assert_eq!(list::PyList_Size(object), 9);

        let slice = list::PyList_GetSlice(object, 0, 3);
        assert_eq!(list::PyList_Size(slice), 3);
        assert_eq!(int_value(list::PyList_GetItem(slice, 0)), 100);

        assert_eq!(list::PyList_SetSlice(object, 0, 1, std::ptr::null_mut()), 0);
        assert_eq!(int_value(list::PyList_GetItem(object, 0)), 0);
        assert_eq!(list::PyList_Size(object), 8);

        assert_eq!(list::PyList_Reverse(object), 0);
        assert_eq!(int_value(list::PyList_GetItem(object, 0)), 7);

        assert_eq!(list::PyList_Sort(object), 0);
        assert_eq!(int_value(list::PyList_GetItem(object, 0)), 0);
        assert_eq!(int_value(list::PyList_GetItem(object, 7)), 7);

        let as_tuple = list::PyList_AsTuple(object);
        assert_eq!(tuple::PyTuple_Size(as_tuple), 8);
        assert_eq!(int_value(tuple::PyTuple_GetItem(as_tuple, 7)), 7);
    }
}

#[test]
fn dict_entry_points_follow_the_cpython_contract() {
    init();

    let mp = dict::PyDict_New();
    let key = int(1);
    // SAFETY: `mp` is a dict object throughout.
    unsafe {
        assert_eq!(dict::PyDict_Size(mp), 0);
        assert_eq!(dict::PyDict_SetItem(mp, key, int(10)), 0);
        assert_eq!(dict::PyDict_Size(mp), 1);
        assert!(core::ptr::eq(
            dict::PyDict_GetItem(mp, key),
            dict::PyDict_GetItemWithError(mp, key)
        ));
        assert_eq!(int_value(dict::PyDict_GetItem(mp, key)), 10);
        assert!(tstate::occurred().is_null());

        let missing = int(99);
        assert!(dict::PyDict_GetItem(mp, missing).is_null());
        assert!(tstate::occurred().is_null(), "PyDict_GetItem swallows");
        assert!(dict::PyDict_GetItemWithError(mp, missing).is_null());
        assert!(tstate::occurred().is_null());
        assert_eq!(dict::PyDict_Contains(mp, key), 1);
        assert_eq!(dict::PyDict_Contains(mp, missing), 0);

        assert!(dict::PyDict_Pop(mp, missing, std::ptr::null_mut()).is_null());
        assert_error(exceptions::key_error());
        let fallback = int(-1);
        let popped = dict::PyDict_Pop(mp, missing, fallback);
        assert!(
            core::ptr::eq(popped, fallback),
            "the default comes back on a miss"
        );

        assert_eq!(dict::PyDict_SetItem(mp, key, int(11)), 0);
        assert_eq!(int_value(dict::PyDict_GetItem(mp, key)), 11);
        assert_eq!(dict::PyDict_DelItem(mp, key), 0);
        assert_eq!(dict::PyDict_Size(mp), 0);
        assert_eq!(dict::PyDict_DelItem(mp, key), -1);
        assert_error(exceptions::key_error());

        for i in 0..5 {
            assert_eq!(dict::PyDict_SetItem(mp, int(i), int(i * i)), 0);
        }
        let keys = dict::PyDict_Keys(mp);
        assert_eq!(list::PyList_Size(keys), 5);
        assert_eq!(int_value(list::PyList_GetItem(keys, 0)), 0);
        let values = dict::PyDict_Values(mp);
        assert_eq!(int_value(list::PyList_GetItem(values, 4)), 16);
        let items = dict::PyDict_Items(mp);
        let pair = list::PyList_GetItem(items, 2);
        assert_eq!(int_value(tuple::PyTuple_GetItem(pair, 0)), 2);
        assert_eq!(int_value(tuple::PyTuple_GetItem(pair, 1)), 4);

        let copy = dict::PyDict_Copy(mp);
        assert_eq!(dict_keys(copy), dict_keys(mp));
        assert_eq!(dict::PyDict_Size(copy), 5);

        let source = list::PyList_New(0);
        assert_eq!(
            list::PyList_Append(
                source,
                fill_tuple(tuple::PyTuple_New(2), &[int(5), int(25)])
            ),
            0
        );
        assert_eq!(dict::PyDict_MergeFromSeq2(copy, source, 1), 0);
        assert_eq!(int_value(dict::PyDict_GetItem(copy, int(5))), 25);
        assert_eq!(dict::PyDict_MergeFromSeq2(copy, int(3), 1), -1);
        assert_error(exceptions::type_error());

        assert_eq!(dict::PyDict_Merge(mp, copy, 1), 0);
        assert_eq!(dict::PyDict_Size(mp), 6);

        let absent = int(42);
        let defaulted = dict::PyDict_SetDefault(mp, absent, int(7));
        assert!(core::ptr::eq(defaulted, dict::PyDict_GetItem(mp, absent)));
        assert_eq!(int_value(dict::PyDict_GetItem(mp, absent)), 7);
        let kept = dict::PyDict_SetDefault(mp, absent, int(9));
        assert!(core::ptr::eq(kept, dict::PyDict_GetItem(mp, absent)));
        assert_eq!(
            int_value(dict::PyDict_GetItem(mp, absent)),
            7,
            "the stored value wins"
        );

        dict::PyDict_Clear(mp);
        assert_eq!(dict::PyDict_Size(mp), 0);
        assert!(dict_keys(mp).is_empty());
    }
}

#[test]
fn dict_version_tag_tracks_mutations() {
    init();

    let mp = dict::PyDict_New();
    let ptr = mp.cast::<dict::PyDictObject>();
    // SAFETY: `mp` is a dict object.
    let read = || unsafe { (*ptr).ma_version_tag };
    let fresh = read();

    let key = int(1);
    // SAFETY: `mp` is a dict object throughout.
    unsafe {
        assert_eq!(dict::PyDict_SetItem(mp, key, int(1)), 0);
        let inserted = read();
        assert_ne!(inserted, fresh);

        assert_eq!(dict::PyDict_SetItem(mp, key, int(1)), 0);
        assert_eq!(read(), inserted, "same-value overwrites keep the version");

        assert_eq!(dict::PyDict_SetItem(mp, key, int(2)), 0);
        let replaced = read();
        assert_ne!(replaced, inserted);

        assert_eq!(dict::PyDict_DelItem(mp, int(99)), -1);
        tstate::clear();
        assert_eq!(read(), replaced, "failed deletes keep the version");

        assert_eq!(dict::PyDict_DelItem(mp, key), 0);
        assert_ne!(read(), replaced);

        dict::PyDict_Clear(mp);
    }
}

#[test]
fn set_entry_points_follow_the_cpython_contract() {
    init();

    // SAFETY: a null iterable builds the empty set.
    let so = unsafe { set::PySet_New(std::ptr::null_mut()) };
    // SAFETY: the set objects below are sets or frozensets.
    unsafe {
        assert_eq!(set::PySet_Size(so), 0);
        for i in 0..4 {
            assert_eq!(set::PySet_Add(so, int(i)), 0);
        }
        assert_eq!(set::PySet_Size(so), 4);
        assert_eq!(set::PySet_Contains(so, int(2)), 1);
        assert_eq!(set::PySet_Contains(so, int(9)), 0);
        assert_eq!(set::PySet_Discard(so, int(2)), 1);
        assert_eq!(set::PySet_Discard(so, int(2)), 0);
        assert_eq!(set::PySet_Size(so), 3);
        assert!(!set::PySet_Pop(so).is_null());
        assert_eq!(set::PySet_Clear(so), 0);
        assert_eq!(set::PySet_Size(so), 0);

        let source = fill_tuple(tuple::PyTuple_New(3), &[int(1), int(2), int(2)]);
        let from_iterable = set::PySet_New(source);
        assert_eq!(set::PySet_Size(from_iterable), 2);

        let fs = set::PyFrozenSet_New(source);
        assert_eq!(set::PySet_Size(fs), 2);
        assert_eq!(set::PySet_Contains(fs, int(1)), 1);
        assert_eq!(set::PySet_Discard(fs, int(1)), -1, "frozensets are frozen");
        assert_error(exceptions::system_error());
        assert!(set::PySet_Pop(fs).is_null());
        assert_error(exceptions::system_error());
        // A frozenset still under construction (reference count 1) is the
        // one frozenset PySet_Add accepts, the C API's build path.
        assert_eq!(set::PySet_Add(fs, int(3)), 0);
        assert_eq!(set::PySet_Size(fs), 3);

        assert_eq!(set::PySet_Add(dict::PyDict_New(), int(3)), -1);
        assert_error(exceptions::system_error());

        let target = set::PySet_New(std::ptr::null_mut());
        assert_eq!(set::_PySet_Update(target, source), 0);
        assert_eq!(set::PySet_Size(target), 2);
    }
}

/// A raw container pointer to share with the hammer threads. `get`
/// takes the whole wrapper so edition-2024 closure captures keep the
/// pointer wrapped (a bare `shared.0` capture would shed `Send`).
// SAFETY: the runtime serializes every mutating entry point with the
// pointed-to object's lock, so sharing the pointer across threads is the
// concurrency model this test exercises.
#[derive(Clone, Copy)]
struct Shared(*mut PyObject);
unsafe impl Send for Shared {}
impl Shared {
    fn get(self) -> *mut PyObject {
        self.0
    }
}

#[test]
fn hammer_one_list_and_one_dict_from_eight_threads() {
    init();

    const THREADS: isize = 8;
    const PER_THREAD: isize = 500;
    const STRIDE: i64 = 1_000_000;

    let list_object = Shared(list::PyList_New(0));
    let mp = Shared(dict::PyDict_New());

    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            std::thread::spawn(move || {
                for i in 0..PER_THREAD {
                    let value = t as i64 * STRIDE + i as i64;
                    let object = int(value);
                    // SAFETY: the shared objects are a list and a dict,
                    // and the mutating entry points take their locks.
                    unsafe {
                        assert_eq!(list::PyList_Append(list_object.get(), object), 0);
                        assert_eq!(dict::PyDict_SetItem(mp.get(), object, object), 0);
                    }
                    if i > 0 {
                        // SAFETY: the shared object is a dict.
                        unsafe {
                            assert_eq!(dict::PyDict_DelItem(mp.get(), int(value - 1)), 0);
                        }
                    }
                    if i % 128 == 127 {
                        // SAFETY: the shared object is a list.
                        unsafe {
                            assert_eq!(list::PyList_Reverse(list_object.get()), 0);
                        }
                    }
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }

    let list_object = list_object.0;
    let mp = mp.0;
    // SAFETY: the threads are joined, so the objects are quiescent.
    unsafe {
        assert_eq!(list::PyList_Size(list_object), THREADS * PER_THREAD);
        assert_eq!(dict::PyDict_Size(mp), THREADS);

        assert_eq!(list::PyList_Sort(list_object), 0);
        for index in 0..THREADS * PER_THREAD {
            let item = list::PyList_GetItem(list_object, index);
            let expected = (index / PER_THREAD) as i64 * STRIDE + index as i64 % PER_THREAD as i64;
            assert_eq!(int_value(item), expected, "index {index}");
        }

        let keys = dict_keys(mp);
        assert_eq!(keys.len(), THREADS as usize);
        for key in keys {
            assert_eq!(
                key % STRIDE,
                PER_THREAD as i64 - 1,
                "each thread keeps its last key"
            );
            let stored = dict::PyDict_GetItem(mp, int(key));
            assert_eq!(int_value(stored), key);
        }
    }

    assert_eq!(lock_byte(list_object, list::LOCK), 0);
    assert_eq!(lock_byte(mp, dict::LOCK), 0);
}
