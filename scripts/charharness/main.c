/* Direct oracle harness: calls the REAL mp_format_float compiled from the
 * original vendor sources (formatfloat.c + parsenum.c) with the reference
 * build's config (full: -DPETITPY_CONFIG_FULL=1, asserts on, no NDEBUG).
 * Input lines on stdin: <bits_hex> <fmt_char> <prec> <sign> <bufsize>
 *   bits_hex: f64 bit pattern, hex (e.g. 3ff0000000000000); sign: a char,
 *   "0" for no sign, "S" for a space sign (a literal space cannot be
 *   passed as an sscanf token). Output per line: <len> <bytes> (raw, no NUL). */
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <stdlib.h>
#include "py/formatfloat.h"

int main(void) {
    char line[512];
    static char buf[4096];
    while (fgets(line, sizeof(line), stdin)) {
        unsigned long long bits;
        char fmtc[8], signc[8];
        int prec, bufsize;
        if (sscanf(line, "%llx %7s %d %7s %d", &bits, fmtc, &prec, signc, &bufsize) != 5) {
            fprintf(stderr, "bad line: %s", line);
            return 2;
        }
        double f;
        uint64_t u = (uint64_t)bits;
        memcpy(&f, &u, 8);
        memset(buf, 0xEE, sizeof(buf));
        char fmt = fmtc[0];
        char sign = (signc[0] == '0' && signc[1] == '\0') ? '\0'
            : (signc[0] == 'S' && signc[1] == '\0') ? ' '
            : signc[0];
        int len = mp_format_float(f, buf, (size_t)bufsize, fmt, prec, sign);
        printf("%d ", len);
        fwrite(buf, 1, (size_t)len, stdout);
        printf("\n");
        fflush(stdout);
    }
    return 0;
}
