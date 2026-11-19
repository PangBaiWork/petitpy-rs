//! References to the builtin type objects.
//!
//! Split out of `obj.rs` by the C13b merge integration (`obj.rs` was
//! 826 lines, over the 800-line cap — pre-existing debt from the C13
//! merge, which C13b only added 8 lines to). Re-exported from
//! `obj.rs`, so `crate::objects::obj::BuiltinTypes` keeps working.

use crate::objects::value::GcRef;

/// References to the builtin type objects. `obj.rs` takes them as a
/// parameter (the C5 explicit-parameter precedent) because the type
/// objects are built by C12 / the type-ROM bootstrap and do not exist
/// yet as a global registry; the VM owns one instance and passes it
/// down. Only the types `obj.zig` names by ROM pointer are here.
#[derive(Clone, Copy, Debug)]
pub struct BuiltinTypes {
    pub int: GcRef,
    pub str_: GcRef,
    pub bytes: GcRef,
    pub bool_: GcRef,
    pub none_type: GcRef,
    pub float: GcRef,
    pub complex: GcRef,
    pub tuple: GcRef,
    pub list: GcRef,
    /// `set` (C9c: the set/frozenset constructor and the in-place
    /// `update = isExactType(lhs, &mp_type_set)` check read it from
    /// `ctx.types`, like the int/bool builtins read theirs).
    pub set: GcRef,
    /// `frozenset` (C9c: the frozenset hash seed check
    /// `isExactType(self, &mp_type_frozenset)` and the frozenset
    /// constructor read it).
    pub frozenset: GcRef,
    /// `range` (C9c: `range.__getitem__` on a slice builds a plain
    /// `range`, as the Zig hardcodes `&mp_type_range`).
    pub range: GcRef,
    /// `dict` (C9a: the dict constructor and copy/fromkeys allocate
    /// with this type; the `dict` builtin reads it from `ctx.types`,
    /// like the int/bool builtins read theirs).
    pub dict: GcRef,
    /// `array` (C13b: the `array` make_new adapter needs the
    /// bytearray type — `array_new` picks the bytearray header
    /// when the typecode is `BYTEARRAY_TYPECODE` (1); probed on
    /// the reference binary: `array('\x01')` is a `bytearray`).
    pub array: GcRef,
    /// `bytearray` (C13b: the `bytearray` make_new adapter needs
    /// the array type for the shared `array_new` helper).
    pub bytearray: GcRef,
    /// `FileIO` (D2: `open()` in binary mode allocates with
    /// this header type; `sys.stdin.buffer` etc. are FileIO
    /// objects).
    pub fileio: GcRef,
    /// `TextIOWrapper` (D2: `open()` in text mode and the
    /// stdio objects allocate with this header type).
    pub textio: GcRef,
    /// The shared attrtuple type (D2: ONE ROM type for
    /// `os.stat` results and `sys.implementation` — probed:
    /// `type(os.stat('.')) is type(sys.implementation)` is
    /// True, while both `is tuple` are False. Built here so
    /// both modules read it from `ctx.types`).
    pub attrtuple: GcRef,
    /// `type` itself (the class-getitem branch of `mp_obj_subscr`).
    pub type_: GcRef,
    /// `object` (C12: the `isSubclassImpl` short-circuit, `mroOf`'s
    /// terminator, `typeBasesTuple`'s default parent, `super_attr`'s
    /// last-resort lookup).
    pub object: GcRef,
    /// `super` (C12: `newSuper` allocates with this header type).
    pub super_: GcRef,
    /// `staticmethod` (C12: `convert_member_lookup` unwraps it).
    pub staticmethod: GcRef,
    /// `classmethod` (C12: `convert_member_lookup` binds it).
    pub classmethod: GcRef,
    /// The bound-method type (C12: the `call_method` seam builds
    /// bound methods with this header type).
    pub boundmeth: GcRef,
    /// The ROM `()` singleton (C12a: the tuple-hash seed is this
    /// object's arena index, so `hash(()) == id(())`).
    pub empty_tuple: GcRef,
    /// The ROM empty-bytes singleton (C12a: `new_empty_bytes`
    /// returns this; `bytes() is b''` holds. Slicing allocates
    /// fresh — `b'ab'[:0] is b''` is False on the reference
    /// binary, so only `new_empty_bytes` uses the singleton).
    pub empty_bytes: GcRef,
    /// The `native_base_init_wrapper_obj` singleton (objtype.zig:734):
    /// the `__init__` wrapper primed into an instance's `subobj[0]`
    /// while its native base is still uninitialised. C12a's ROM
    /// builds the real wrapper; other fixtures hold a plain type.
    pub native_init_wrapper: GcRef,
    /// `property` (C12: the special-accessor scan tests
    /// `isExactType(value, property)`).
    pub property: GcRef,
    /// `dict_view` (E2: ONE ROM type for all three dict-view
    /// kinds — `dict_keys` / `dict_values` / `dict_items` all
    /// allocate with this header; the kind lives in the
    /// `IteratorData`. Built at bootstrap so `make_view` can
    /// read it from `ctx.types`).
    pub dict_view: GcRef,
    /// `dict_view_it` (E2: the view-iterator ROM type, name
    /// `iterator`, `FLAG_ITER_IS_ITERNEXT`; `make_view_it`
    /// allocates with this header).
    pub dict_view_it: GcRef,
    /// `iterator` (G4/H8: the `mp_type_polymorph_iter` ROM type,
    /// name `iterator`, `FLAG_ITER_IS_ITERNEXT`; the sequence
    /// iterators — list/tuple/str/bytes/set/frozenset/array/
    /// deque/getitem — allocate with this header, as the Zig's
    /// `objlist.zig:572` / `objtuple.zig:296` / `objset.zig:169` /
    /// `objarray.zig:743` / `objdeque.zig:272` do. Distinct from
    /// `dict_view_it` and `range_it`: the reference reports
    /// `type(iter([1])) is type(iter({'a': 1}))` False and
    /// `type(iter(range(3))) is type(iter([1]))` False.)
    pub iterator: GcRef,
    /// `range_it` (G4/H8: the `mp_type_range_it` ROM type, name
    /// `iterator`, `FLAG_ITER_IS_ITERNEXT`; range iterators
    /// allocate with this header — `objrange.zig:217`).
    pub range_it: GcRef,
    /// `socket` (D3: the `socket` module's type object, built by
    /// `build_socket_module` — not by the VM bootstrap, so the
    /// module entry is a plain constructor function per
    /// `modsocket.zig`, and the constructor needs the type
    /// object to allocate instances with. `None` until a
    /// socket module is built and registered; the registrar
    /// stores `Some(type_ref)` after calling the builder).
    pub socket: Option<GcRef>,
    /// `socketfile` (H-socket: the `socket.makefile()` wrapper
    /// type object, built by `build_socket_module` alongside
    /// the socket type — same shape as `socket`/`match_type`:
    /// `None` until the socket module is built and
    /// registered; the registrar stores `Some(type_ref)`
    /// after calling the builder).
    pub socketfile: Option<GcRef>,
    /// `deque` (D4: the native deque type object, built by
    /// `build_collections_module` — the `ucollections.deque`
    /// type as well, once Phase E builds that module; the
    /// frozen `collections` package re-exports it. `None`
    /// until the collections module is built and registered;
    /// the registrar stores `Some(type_ref)` after calling
    /// the builder — the `socket` precedent).
    pub deque: Option<GcRef>,
    /// `OrderedDict` (D4: the ordered-dict type object, built
    /// by `build_collections_module` — same shape as `deque`,
    /// `None` until registered).
    pub ordereddict: Option<GcRef>,
    /// `namedtuple` (D4: the factory `FunBuiltin`, built by
    /// `build_collections_module` — the `ucollections`
    /// factory as well; `None` until registered).
    pub namedtuple_fn: Option<GcRef>,
    /// `re` (D5: the compiled-pattern type object, built by
    /// `build_re_module` — the native `re`/`ure` type; `None`
    /// until the re module is built and registered; the
    /// registrar stores `Some(type_ref)` after calling the
    /// builder — the `socket` precedent).
    pub re_type: Option<GcRef>,
    /// `match` (D5: the match-object type object, built by
    /// `build_re_module` alongside `re_type`; same shape).
    pub match_type: Option<GcRef>,
}

