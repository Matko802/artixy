/* Unit tests for scrub + live helpers. Guest roundtrip runs only with
 * ARTIXY_LIVE_TEST=1 (needs the real Artix VM + agent). */
#include "test.h"

#include "../src/live.h"
#include "../src/scrub.h"
#include "../src/util.h"
#include "../src/vm.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

TEST(expand_keys) {
    char *e = live_expand_typed_input(";enter");
    CHECK_STR_EQ(e, "\r");
    free(e);
    e = live_expand_typed_input("hi ;space there");
    CHECK_STR_EQ(e, "hi   there");
    free(e);
    e = live_expand_typed_input("a\\nb");
    CHECK_STR_EQ(e, "a\nb");
    free(e);
    e = live_expand_typed_input(";up 3");
    CHECK_STR_EQ(e, "\x1b[A\x1b[A\x1b[A");
    free(e);
    e = live_expand_typed_input(";ctrl+c");
    CHECK(e[0] == '\x03' && e[1] == '\0');
    free(e);
    e = live_expand_typed_input("ls ;enter");
    CHECK_STR_EQ(e, "ls \r");
    free(e);
    e = live_expand_typed_input("plain text stays");
    CHECK_STR_EQ(e, "plain text stays");
    free(e);
}

TEST(build_runner) {
    char *s = live_build_runner("bash", "QUJD", "/tmp/o.out", "/tmp/o.code",
                                "/tmp/o.in", "bob");
    CHECK(s != NULL);
    CHECK(strstr(s, "QUJD") != NULL);
    CHECK(strstr(s, "/tmp/o.out") != NULL);
    CHECK(strstr(s, "cols 120") != NULL);
    CHECK(strstr(s, "su") != NULL || strstr(s, "bob") != NULL);
    free(s);
    s = live_build_runner("sh", "eA==", "/tmp/a", "/tmp/b", "/dev/null", "");
    CHECK(s != NULL && strstr(s, "/dev/null") != NULL);
    free(s);
    s = live_mkfifo_script("/tmp/f", "bob");
    CHECK(s != NULL && strstr(s, "chown bob") != NULL);
    free(s);
    s = live_mkfifo_script("/tmp/f", "");
    CHECK(s != NULL && strstr(s, "mkfifo") != NULL);
    free(s);
}

TEST(scrub_ips) {
    char *s = scrub_public_ip("my ip is 8.8.8.8 ok");
    CHECK_STR_EQ(s, "my ip is [redacted] ok");
    free(s);
    s = scrub_public_ip("local 192.168.1.5 stays");
    CHECK_STR_EQ(s, "local 192.168.1.5 stays");
    free(s);
    s = scrub_public_ip("loopback 127.0.0.1 stays");
    CHECK_STR_EQ(s, "loopback 127.0.0.1 stays");
    free(s);
    s = scrub_public_ip("v6 2001:db8::1 gone");
    CHECK(strstr(s, "[redacted]") != NULL);
    free(s);
    s = scrub_public_ip("v6 ::1 stays");
    CHECK(strstr(s, "[redacted]") == NULL);
    free(s);
    s = scrub_public_ip("no ips here");
    CHECK_STR_EQ(s, "no ips here");
    free(s);
}

TEST(live_guest_roundtrip) {
    if (!getenv("ARTIXY_LIVE_TEST")) {
        printf("(skip live guest roundtrip: set ARTIXY_LIVE_TEST=1)\n");
        return;
    }
    CHECK(vm_agent_ping("artix"));
    char fifo[128], landed[128];
    snprintf(fifo, sizeof fifo, "/tmp/artixy-live-test-%d.fifo", getpid());
    snprintf(landed, sizeof landed, "/tmp/artixy-live-test-%d.out", getpid());
    char *mkargs[] = { "-c", NULL, NULL };
    char mkscript[512];
    snprintf(mkscript, sizeof mkscript, "rm -f %s %s && mkfifo -m 600 %s",
             fifo, landed, fifo);
    mkargs[1] = mkscript;
    long long code = -1;
    CHECK(vm_guest_exec("artix", "/bin/bash", mkargs, false, 15, &code, NULL,
                        NULL) == 0 && code == 0);
    /* background reader: cat fifo into a file, bounded by timeout */
    char reader[320];
    snprintf(reader, sizeof reader, "timeout 8 sh -c 'cat %s > %s'", fifo,
             landed);
    char *rargs[] = { "-c", reader, NULL };
    long long rpid = vm_guest_launch_raw("artix", "/bin/bash", rargs, false);
    CHECK(rpid > 0);
    struct timespec ts = { 1, 0 };
    nanosleep(&ts, NULL);
    CHECK(live_forward_input("artix", fifo, "", "hello-live\n"));
    /* wait for the reader to finish */
    bool done = false;
    for (int i = 0; i < 12; i++) {
        long long c = 0;
        int st = vm_guest_status("artix", rpid, &c);
        if (st == 1) {
            done = true;
            break;
        }
        nanosleep(&ts, NULL);
    }
    CHECK(done);
    size_t n = 0;
    unsigned char *d = vm_guest_file_b64("artix", landed, 1024 * 1024, &n);
    CHECK(d != NULL);
    CHECK(n == 11 && memcmp(d, "hello-live\n", 11) == 0);
    free(d);
    char *rmargs[] = { "-f", fifo, landed, NULL };
    vm_guest_exec("artix", "/bin/rm", rmargs, false, 10, &code, NULL, NULL);
}

int main(void) {
    RUN(expand_keys);
    RUN(build_runner);
    RUN(scrub_ips);
    RUN(live_guest_roundtrip);
    TEST_REPORT();
}
