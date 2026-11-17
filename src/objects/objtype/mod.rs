//! Object-type machinery (`src/objects/objtype/`).
//!
//! Lane C12a owns the type-system core (`objtype.zig`, 2,214 lines):
//! `errors`, `instance`, `instance_create`, `instance_print`,
//! `lookup`, `rom`, `seams`, `subclass`, `typecreate` (plus their
//! tests). Lane C12b owns `objexcept`, `objmodule`, `objobject`
//! (plus `objexcept_types` and their tests). Merged by the main
//! agent at the C12a merge (union of both lanes' module lists).

pub mod errors;
pub mod instance;
pub mod instance_create;
pub mod instance_print;
pub mod lookup;
pub mod objexcept;
pub mod objexcept_types;
pub mod objmodule;
pub mod objobject;
pub mod rom;
pub mod seams;
pub mod subclass;
pub mod typecreate;

#[cfg(test)]
mod objtype_tests;
#[cfg(test)]
mod objtype_tests_b;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_instance;
#[cfg(test)]
mod tests_instance_create;
#[cfg(test)]
mod tests_lookup;
#[cfg(test)]
mod tests_rom;
#[cfg(test)]
mod tests_set_name;
#[cfg(test)]
mod tests_typecreate;

pub use errors::ObjTypeError;
pub use lookup::{
    binary_op_method_name, c3_mro, cached_mro, class_lookup, convert_member_lookup,
    has_slot, is_native_type, mro_of, mro_tuple, scan_special_accessors_flag,
    type_bases_tuple, type_locals_dict_readonly, unary_op_method_name, LookupHit,
    SlotKind, MAX_BASES,
};
pub use seams::{call_via_type_slot, VmSeams};
pub use subclass::{
    cast_to_native_base, is_exact_type, is_subclass, is_subclass_fast,
    is_subclass_of_value, is_type_value, isinstance_builtin, issubclass_builtin,
};
pub use instance_create::{
    count_native_bases, instance_make_new, load_super_method,
    native_base_init_wrapper_obj, new_instance, static_class_method_make_new,
};
pub use typecreate::{
    is_fun, mp_obj_new_type, super_attr, super_make_new, super_print, type_attr,
    type_call, type_make_new, type_print, type_store_attr, TYPE_BASE_FLAGS,
};
pub use objobject::object_type_data;
pub use rom::{
    build_rom, classmethod_type_data, staticmethod_type_data, super_type_data,
    type_type_data, Rom,
};
