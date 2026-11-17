//! Cell type — `mp_obj_new_cell` (`objcell.zig`, 48 lines, whole file).
//!
//! `mp_type_cell` is file-local static, flags 0, name the empty
//! qstr (id 0), **all 11 slots null**; the only export is
//! `mp_obj_new_cell(obj)` (allocates a cell holding `obj`). The cell
//! payload is C0's `CellData { value: Option<Value> }`.
//!
//! Cells are the closure-variable boxes: the compiler creates one
//! per closed-over variable (C10) and the VM reads/writes it through
//! `cell_get` / `cell_set`. A fresh cell always holds its object
//! (`value: Some(obj)`).

use crate::memory::gc::{CallCtx, GcArena};
use crate::objects::value::{CellData, GcRef, Obj, TypeData, Value};

/// `mp_type_cell` ROM data. `dict` is the bootstrap's empty
/// placeholder — the original has no `locals_dict` (NULL).
/// `mro` is filled with `[self]` by the bootstrap after allocation
/// (a type's MRO starts with itself; `base` is `None`).
/// All slots are `None`: the original's 11 slots are all null.
pub fn cell_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: 0,
        flags: 0,
        base: None,
        mro: Vec::new(),
        dict,
        unary_op: None,
        binary_op: None,
        call: None,
        getiter: None,
        subscr: None,
        buffer: None,
        print: None,
        namedtuple_fields: None,
    }
}

/// `mp_obj_new_cell(obj)`: allocate a cell holding `obj`, typed by
/// the cell type object (`cell_type` is the bootstrap's ref to it).
pub fn new_cell(ctx: &mut CallCtx<'_>, cell_type: GcRef, obj: Value) -> GcRef {
    ctx.alloc(Some(cell_type), Obj::Cell(CellData { value: Some(obj) }))
}

/// Read a cell's held value (`None` if the cell is empty — only
/// before the compiler stores its first value).
pub fn cell_get(arena: &GcArena, cell: GcRef) -> Option<Value> {
    match arena.get(cell) {
        Obj::Cell(c) => c.value,
        _ => None,
    }
}

/// Store a value into a cell.
pub fn cell_set(arena: &mut GcArena, cell: GcRef, value: Value) {
    if let Obj::Cell(c) = arena.get_mut(cell) {
        c.value = Some(value);
    }
}
