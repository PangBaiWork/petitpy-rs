/* G4/H10 checks of the embedding API (include/petitpy.h):
 * `ppy_str` / `ppy_repr` with FunBc `__str__` / `__repr__`.
 * The original's own `tests/embed/api_test.c` (run UNMODIFIED
 * for the F1 C acceptance) only prints tuple/list/dict; this
 * file covers the dunder cases H10 fixed:
 *   - a working `__str__` / `__repr__` runs through the VM hook
 *     (`str(A())` is `A-str`, not `<A object at …>`)
 *   - a nested container renders the dunder (`[A-repr, 1]`)
 *   - an exception arg renders its dunder (`KeyError(A-repr,)`)
 *   - a RAISING dunder falls back (`<B object at …>`, PPY_OK):
 *     the ORIGINAL aborts here (rc=134, null-pointer panic at
 *     `mp_mirrors.zig:745` — the documented abort-vs-fallback
 *     divergence, do not reproduce), so these checks pin the
 *     Rust fallback, not the original's abort.
 * Build (mirroring make test-embed):
 *   cc -std=c99 -Wall -Wextra -Werror -Iinclude \
 *      tests/embed/api_h10_test.c target/release/libpetitpy_rs.a \
 *      -lpthread -lm -o api_h10_test_static
 *   cc ... -Iinclude tests/embed/api_h10_test.c -Ltarget/release \
 *      -lpetitpy_rs -lpthread -lm -o api_h10_test_cdylib
 * (cdylib runs need LD_LIBRARY_PATH=target/release.) */
#include <stdio.h>
#include <string.h>

#include "petitpy.h"

static int failed;

static void check(const char *name, int ok) {
    printf("%s  %s\n", ok ? "PASS" : "FAIL", name);
    failed |= !ok;
}

/* Is h a str equal to want? */
static int str_is(ppy_interp *py, ppy_handle h, const char *want) {
    const char *s;
    size_t n;
    return ppy_to_str(py, h, &s, &n) == PPY_OK && n == strlen(want) && memcmp(s, want, n) == 0;
}

/* Is h a str starting with prefix? (arena-index fallback). */
static int str_starts(ppy_interp *py, ppy_handle h, const char *prefix) {
    const char *s;
    size_t n;
    return ppy_to_str(py, h, &s, &n) == PPY_OK && n >= strlen(prefix) &&
           memcmp(s, prefix, strlen(prefix)) == 0;
}

int main(void) {
    ppy_interp *py = ppy_new(NULL);
    if (!py) {
        printf("FAIL ppy_new\n");
        return 1;
    }
    const char *src =
        "class A:\n"
        "    def __str__(self):\n"
        "        return 'A-str'\n"
        "    def __repr__(self):\n"
        "        return 'A-repr'\n"
        "class B:\n"
        "    def __str__(self):\n"
        "        raise ValueError('boom')\n"
        "    def __repr__(self):\n"
        "        raise ValueError('boom-r')\n"
        "a = A()\n"
        "b = B()\n"
        "xs = [A(), 1]\n"
        "ys = [B(), 1]\n"
        "ke = KeyError(A())\n";
    if (ppy_exec(py, src, strlen(src), "<h10>") != PPY_OK) {
        printf("FAIL exec\n");
        return 1;
    }
    ppy_handle a = 0, b = 0, xs = 0, ys = 0, ke = 0, r = 0;
    ppy_get_global(py, "a", &a);
    ppy_get_global(py, "b", &b);
    ppy_get_global(py, "xs", &xs);
    ppy_get_global(py, "ys", &ys);
    ppy_get_global(py, "ke", &ke);

    check("str(A()) == A-str",
          ppy_str(py, a, &r) == PPY_OK && str_is(py, r, "A-str"));
    check("repr(A()) == A-repr",
          ppy_repr(py, a, &r) == PPY_OK && str_is(py, r, "A-repr"));
    check("repr([A(), 1]) == [A-repr, 1]",
          ppy_repr(py, xs, &r) == PPY_OK && str_is(py, r, "[A-repr, 1]"));
    check("str(KeyError(A())) == A-str",
          ppy_str(py, ke, &r) == PPY_OK && str_is(py, r, "A-str"));
    check("repr(KeyError(A())) == KeyError(A-repr,)",
          ppy_repr(py, ke, &r) == PPY_OK && str_is(py, r, "KeyError(A-repr,)"));
    check("str(B()) PPY_OK + fallback",
          ppy_str(py, b, &r) == PPY_OK && str_starts(py, r, "<B object at "));
    check("repr(B()) PPY_OK + fallback",
          ppy_repr(py, b, &r) == PPY_OK && str_starts(py, r, "<B object at "));
    check("repr([B(), 1]) PPY_OK + fallback",
          ppy_repr(py, ys, &r) == PPY_OK && str_starts(py, r, "[<B object at "));

    ppy_free(py);
    printf(failed ? "FAILURES\n" : "all passed\n");
    return failed;
}
