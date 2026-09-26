/* Unit tests for base64 codec and the vm backend (via fake virsh). */
#include "test.h"

#include "../src/b64.h"
#include "../src/util.h"
#include "../src/vm.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

TEST(b64_roundtrip) {
    const char *vec[] = { "", "f", "fo", "foo", "foob", "fooba", "foobar",
                          "hello world, this is a longer string 12345!@#" };
    for (size_t i = 0; i < sizeof(vec) / sizeof(vec[0]); i++) {
        char *e = b64_encode(vec[i], strlen(vec[i]));
        CHECK(e != NULL);
        size_t n = 0;
        unsigned char *d = b64_decode(e, &n);
        CHECK(d != NULL);
        CHECK(n == strlen(vec[i]));
        CHECK(memcmp(d, vec[i], n) == 0);
        free(e);
        free(d);
    }
    /* known vectors */
    char *e = b64_encode("Man", 3);
    CHECK_STR_EQ(e, "TWFu");
    free(e);
    e = b64_encode("Ma", 2);
    CHECK_STR_EQ(e, "TWE=");
    free(e);
}

TEST(b64_rejects_garbage) {
    size_t n = 99;
    CHECK(b64_decode("!!!", &n) == NULL);
    CHECK(b64_decode("ABC", &n) == NULL);   /* bad length */
    CHECK(b64_decode("AB=C", &n) == NULL);  /* interior pad */
    CHECK(b64_decode("ABCD=", &n) == NULL); /* bad length */
    /* binary roundtrip incl. NUL bytes */
    unsigned char bin[256];
    for (int i = 0; i < 256; i++)
        bin[i] = (unsigned char)i;
    char *enc = b64_encode(bin, sizeof bin);
    CHECK(enc != NULL);
    unsigned char *dec = b64_decode(enc, &n);
    CHECK(dec != NULL && n == 256 && memcmp(dec, bin, 256) == 0);
    free(enc);
    free(dec);
}

/* Prepend tests/fakebin to PATH so "virsh" resolves to the fake. */
static void use_fake_virsh(void) {
    char state_tmpl[] = "/tmp/artixy-vmtest-XXXXXX";
    CHECK(mkdtemp(state_tmpl) != NULL);
    setenv("FAKE_STATE", state_tmpl, 1);
    unsetenv("FAKE_PING_FAIL");
    unsetenv("FAKE_EMPTY_FIRST");
    char path[8192];
    snprintf(path, sizeof path, "tests/fakebin:%s", getenv("PATH"));
    setenv("PATH", path, 1);
}

TEST(virsh_list_and_state) {
    use_fake_virsh();
    char *list_args[] = { "list", "--all", NULL };
    char *out = NULL;
    CHECK(vm_virsh(list_args, &out) == 0);
    CHECK(strstr(out, "artix") != NULL);
    free(out);
    char *st_args[] = { "domstate", "artix", NULL };
    CHECK(vm_virsh(st_args, &out) == 0);
    CHECK_STR_EQ(out, "shut off");
    free(out);
}

TEST(virsh_failure_message) {
    use_fake_virsh();
    char *args[] = { "domstate", "bogus", NULL };
    char *out = NULL;
    CHECK(vm_virsh(args, &out) == -1);
    CHECK(out == NULL);
    CHECK(strstr(vm_error(), "virsh domstate bogus failed:") != NULL);
    CHECK(strstr(vm_error(), "failed to get domain") != NULL);
}

TEST(agent_ping_and_wait) {
    use_fake_virsh();
    CHECK(vm_agent_ping("artix"));
    CHECK(vm_wait_agent("artix", 3));
    setenv("FAKE_PING_FAIL", "1", 1);
    CHECK(!vm_agent_ping("artix"));
    unsetenv("FAKE_PING_FAIL");
}

TEST(guest_exec_capture) {
    use_fake_virsh();
    char *args[] = { "-c", "echo hi", NULL };
    long long code = -1;
    char *o = NULL, *er = NULL;
    /* status exits on 3rd poll: ~2s of 1s sleeps */
    CHECK(vm_guest_exec("artix", "/bin/bash", args, true, 10, &code, &o, &er) == 0);
    CHECK(code == 0);
    CHECK_STR_EQ(o, "hello\n");
    CHECK_STR_EQ(er, "");
    free(o);
    free(er);
}

TEST(guest_exec_timeout) {
    use_fake_virsh();
    /* status never exits within 1s (needs 3 polls) */
    char *args[] = { "sleep", "5", NULL };
    long long code = -1;
    CHECK(vm_guest_exec("artix", "/bin/sh", args, false, 1, &code, NULL, NULL) == -1);
    CHECK(strstr(vm_error(), "timed out") != NULL);
}

TEST(guest_launch_empty_retry) {
    use_fake_virsh();
    setenv("FAKE_EMPTY_FIRST", "1", 1);
    char *args[] = { "x", NULL };
    long long pid = vm_guest_launch_raw("artix", "/bin/true", args, false);
    unsetenv("FAKE_EMPTY_FIRST");
    CHECK(pid == 4242);
}

TEST(guest_file_b64) {
    use_fake_virsh();
    size_t n = 0;
    unsigned char *d = vm_guest_file_b64("artix", "/some/file", 16 * 1024 * 1024, &n);
    CHECK(d != NULL);
    CHECK(n == 7 && memcmp(d, "data123", 7) == 0);
    free(d);
}

TEST(kill_tree_script) {
    char *s = vm_kill_tree_script(4242);
    CHECK(s != NULL);
    CHECK(strstr(s, "killtree 4242") != NULL);
    CHECK(strstr(s, "pgrep") != NULL);
    free(s);
    /* kill path must not crash against the fake */
    vm_guest_kill_tree("artix", 4242);
}

TEST(connect_uri_override) {
    /* default must survive NULL/empty resets */
    vm_set_connect_uri(NULL);
    vm_set_connect_uri("");
    vm_set_connect_uri("qemu+ssh://u@h/system");
    vm_set_connect_uri(NULL);
}

int main(void) {
    RUN(b64_roundtrip);
    RUN(b64_rejects_garbage);
    RUN(connect_uri_override);
    RUN(virsh_list_and_state);
    RUN(virsh_failure_message);
    RUN(agent_ping_and_wait);
    RUN(guest_exec_capture);
    RUN(guest_exec_timeout);
    RUN(guest_launch_empty_retry);
    RUN(guest_file_b64);
    RUN(kill_tree_script);
    TEST_REPORT();
}
