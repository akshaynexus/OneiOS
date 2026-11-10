/* Placeholder for bin/xnuhost until the booter's build exists (runtime/build.sh): prints one line and exits 2.
 * No libc: one static arm64 executable that runs on any Android kernel, so the OneOS fork can test
 * download, verification, staging and the exec itself. */
static const char msg[] = "xnuhost: not built yet (placeholder in the OneiOS runtime package)\n";

static long sys3(long n, long a, long b, long c)
{
    register long x8 __asm__("x8") = n;
    register long x0 __asm__("x0") = a;
    register long x1 __asm__("x1") = b;
    register long x2 __asm__("x2") = c;
    __asm__ volatile("svc #0" : "+r"(x0) : "r"(x8), "r"(x1), "r"(x2) : "memory");
    return x0;
}

void _start(void)
{
    sys3(64 /* write */, 1, (long)msg, sizeof msg - 1);
    for (;;) {
        sys3(94 /* exit_group */, 2, 0, 0);
    }
}
