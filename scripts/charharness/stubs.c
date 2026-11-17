/* Link stubs for runtime symbols referenced by parsenum.c functions that
 * the harness never calls (mp_parse_num_integer / mp_parse_num_decimal /
 * raise_exc). mp_decimal_exp and mp_parse_float_internal touch none of
 * these. Any call aborts, so a stub firing would be loud, not silent. */
#include <stdlib.h>
int mp_type_SyntaxError;
int mp_type_ValueError;
void mp_obj_exception_add_traceback(void) { abort(); }
void nlr_jump(void) { abort(); }
int unichar_isspace(void) { abort(); return 0; }
void *mp_parse_num_base(void) { abort(); return 0; }
void *mp_obj_new_int_from_str_len(void) { abort(); return 0; }
void *mp_obj_new_exception_msg(void) { abort(); return 0; }
void mp_raise_ValueError(void) { abort(); }
void *mp_obj_new_float(void) { abort(); return 0; }
void *mp_obj_new_complex(void) { abort(); return 0; }
int mp_mul_mp_int_t_overflow(void) { abort(); return 0; }
