/* Unit tests for bot validators, formatting, parsing, persist. */
#include "test.h"

#include "../src/bot.h"
#include "../src/config.h"
#include "../src/events.h"
#include "../src/util.h"

#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

TEST(sensitive_send_names_refused) {
    const char *bad[] = { ".env",        ".env.local",  "prod.env",
                          "config.jsonc", "Config.JSONC", "id_rsa",
                          "id_ed25519.pub", "backup.pem", "cert.key",
                          "store.p12",   "my_secret_notes.txt",
                          "db_credentials.json", "api_token.txt",
                          "discord_webhook.txt", ".hidden", "" };
    for (size_t i = 0; i < sizeof(bad) / sizeof(bad[0]); i++)
        CHECK(bot_sensitive_send_name(bad[i]));
    const char *good[] = { "report.pdf", "photo.png", "notes.txt",
                           "output.log" };
    for (size_t i = 0; i < sizeof(good) / sizeof(good[0]); i++)
        CHECK(!bot_sensitive_send_name(good[i]));
}

TEST(share_dot_components) {
    CHECK(bot_share_has_dot(".env"));
    CHECK(bot_share_has_dot("a/../.ssh/x"));
    CHECK(!bot_share_has_dot("docs/report.pdf"));
    CHECK(!bot_share_has_dot(""));
}

TEST(guest_dir_normalization) {
    char out[600];
    CHECK(bot_normalize_guest_dir("/tmp/artixy-uploads/a", out, sizeof out));
    CHECK_STR_EQ(out, "/tmp/artixy-uploads/a");
    CHECK(bot_normalize_guest_dir("/home/bob//docs/./", out, sizeof out));
    CHECK_STR_EQ(out, "/home/bob/docs");
    CHECK(bot_normalize_guest_dir("/home/bob/../bob/x", out, sizeof out));
    CHECK_STR_EQ(out, "/home/bob/x");
    CHECK(bot_normalize_guest_dir("/", out, sizeof out));
    CHECK_STR_EQ(out, "/");
    CHECK(!bot_normalize_guest_dir("tmp/x", out, sizeof out));
    CHECK(!bot_normalize_guest_dir("/../etc", out, sizeof out));
    CHECK(!bot_normalize_guest_dir("/etc/../../..", out, sizeof out));
}

TEST(upload_allowlist) {
    CHECK(bot_upload_allowed("/tmp/artixy-uploads", NULL));
    CHECK(bot_upload_allowed("/tmp/artixy-uploads/a/b", NULL));
    CHECK(!bot_upload_allowed("/tmp/other", NULL));
    CHECK(!bot_upload_allowed("/etc/cron.d", NULL));
    CHECK(!bot_upload_allowed("/", NULL));
    CHECK(bot_upload_allowed("/home/bob", "bob"));
    CHECK(bot_upload_allowed("/home/bob/docs", "bob"));
    CHECK(!bot_upload_allowed("/home/alice/docs", "bob"));
    CHECK(!bot_upload_allowed("/home/bob", "root"));
    CHECK(!bot_upload_allowed("/home/bob", NULL));
}

TEST(target_id_parse) {
    CHECK(parse_target_id("<@123>") == 123);
    CHECK(parse_target_id("<@!456>") == 456);
    CHECK(parse_target_id("789") == 789);
    CHECK(parse_target_id("0") == 0);
    CHECK(parse_target_id("abc") == 0);
    CHECK(parse_target_id("<@>") == 0);
}

TEST(message_ref_parse) {
    uint64_t ch = 0, mid = 0;
    CHECK(parse_message_ref("https://discord.com/channels/1/2/3", 9, &ch, &mid));
    CHECK(ch == 2 && mid == 3);
    CHECK(parse_message_ref("12345", 9, &ch, &mid));
    CHECK(ch == 9 && mid == 12345);
    CHECK(!parse_message_ref("0", 9, &ch, &mid));
    CHECK(!parse_message_ref("nope", 9, &ch, &mid));
    CHECK(!parse_message_ref("https://discord.com/channels/1/0/3", 9, &ch, &mid));
}

TEST(discord_name_sanitize) {
    char out[64];
    bot_sanitize_discord_name("Tall-Is_99!", out, sizeof out);
    CHECK_STR_EQ(out, "tall-is_99");
    bot_sanitize_discord_name("9lives", out, sizeof out);
    CHECK_STR_EQ(out, "lives");
    bot_sanitize_discord_name("!!!", out, sizeof out);
    CHECK_STR_EQ(out, "");
}

TEST(runas_validation) {
    CHECK(bot_valid_runas("matko802"));
    CHECK(bot_valid_runas("_tallis_"));
    CHECK(!bot_valid_runas(""));
    CHECK(!bot_valid_runas("root"));
    CHECK(!bot_valid_runas("9bob"));
    CHECK(!bot_valid_runas("has space"));
}

TEST(sudoers_script) {
    char *s = bot_sudoers_script("bob");
    CHECK(s != NULL);
    CHECK(strstr(s, "bob") != NULL);
    CHECK(strstr(s, "NOPASSWD") != NULL);
    free(s);
    CHECK(bot_sudoers_script("root") == NULL);
    CHECK(bot_sudoers_script("bad name") == NULL);
}

TEST(sh_escape) {
    char out[64];
    bot_sh_escape("a'b", out, sizeof out);
    CHECK_STR_EQ(out, "'a'\\''b'");
    bot_sh_escape("plain", out, sizeof out);
    CHECK_STR_EQ(out, "'plain'");
}

TEST(strip_sgr) {
    char *s = bot_strip_sgr("\x1b[31mhi\x1b[0m");
    CHECK(s != NULL);
    CHECK_STR_EQ(s, "hi");
    free(s);
}

TEST(codeblock_and_tail) {
    char out[2048];
    bot_codeblock("hi", out, sizeof out);
    CHECK_STR_EQ(out, "```\nhi\n```");
    bot_plain_tail("$ ls\nfoo\n", out, sizeof out);
    CHECK(strstr(out, "$ ls") != NULL && strstr(out, "foo") != NULL);
}

TEST(access_checks) {    bot_state_t st;
    memset(&st, 0, sizeof st);
    pthread_rwlock_init(&st.mu, NULL);
    strmap_init(&st.linux);
    strmap_init(&st.shells);
    st.owner = 1;
    uint64_t users[] = { 2 };
    st.users = users;
    st.n_users = 1;
    uint64_t blocked[] = { 3 };
    st.blocked = blocked;
    st.n_blocked = 1;
    uint64_t admins[] = { 4 };
    st.admins = admins;
    st.n_admins = 1;
    CHECK(bot_is_authed(&st, 1));
    CHECK(bot_is_authed(&st, 2));
    CHECK(!bot_is_authed(&st, 3));
    CHECK(!bot_is_authed(&st, 9));
    CHECK(bot_is_owner(&st, 1));
    CHECK(!bot_is_owner(&st, 2));
    CHECK(bot_is_elevated(&st, 1));
    CHECK(bot_is_elevated(&st, 4));
    CHECK(!bot_is_elevated(&st, 2));
    CHECK(bot_is_blocked(&st, 3));
    pthread_rwlock_destroy(&st.mu);
}

static void use_tmpdir(char *dir_out, size_t n) {
    char *tmp = getenv("TMPDIR");
    snprintf(dir_out, n, "%s/artixy-bottest-XXXXXX", tmp ? tmp : "/tmp");
    CHECK(mkdtemp(dir_out) != NULL);
    setenv("XDG_CONFIG_HOME", dir_out, 1);
}

TEST(persist_roundtrip) {
    char dir[4096];
    use_tmpdir(dir, sizeof dir);
    config_ensure_template();
    /* seed owner/token/vm which persist must preserve */
    {
        char path[8192];
        snprintf(path, sizeof path, "%s/artixy/config.jsonc", dir);
        FILE *f = fopen(path, "w");
        CHECK(f != NULL);
        fputs("{\"owner_id\":11,\"discord_token\":\"tok\","
              "\"vm_name\":\"artix\"}",
              f);
        fclose(f);
        chmod(path, 0600);
    }
    bot_state_t st;
    file_config_t cfg;
    CHECK(config_load(&cfg) == 0);
    CHECK(bot_state_init(&st, &cfg) == 0);
    file_config_free(&cfg);
    /* mutate live state */
    pthread_rwlock_wrlock(&st.mu);
    uint64_t u[] = { 22 };
    free(st.users);
    st.users = malloc(sizeof u);
    memcpy(st.users, u, sizeof u);
    st.n_users = 1;
    strmap_set(&st.linux, "22", "bob");
    strmap_set(&st.shells, "22", "fish");
    st.war_mode = true;
    st.has_notify = true;
    st.notify_channel = 99;
    pthread_rwlock_unlock(&st.mu);
    CHECK(bot_persist(&st) == 0);
    bot_state_free(&st);
    /* reload and verify */
    file_config_t c2;
    CHECK(config_load(&c2) == 0);
    CHECK(c2.has_owner_id && c2.owner_id == 11);
    CHECK(c2.discord_token && strcmp(c2.discord_token, "tok") == 0);
    CHECK(c2.vm_name && strcmp(c2.vm_name, "artix") == 0);
    CHECK(c2.n_managers == 1 && c2.managers[0] == 22);
    CHECK(strmap_get(&c2.linux, "22") &&
          strcmp(strmap_get(&c2.linux, "22"), "bob") == 0);
    CHECK(strmap_get(&c2.shells, "22") &&
          strcmp(strmap_get(&c2.shells, "22"), "fish") == 0);
    CHECK(c2.war_mode);
    CHECK(c2.has_notify_channel && c2.notify_channel == 99);
    file_config_free(&c2);
    /* parse the persisted file as strict JSON (comments allowed but must
     * still be valid JSONC our loader accepts — reload proves it) */
}

TEST(persist_refuses_broken_file) {
    char dir[4096];
    use_tmpdir(dir, sizeof dir);
    char path[8192];
    snprintf(path, sizeof path, "%s/artixy", dir);
    CHECK(mkdir_p(path) == 0);
    snprintf(path, sizeof path, "%s/artixy/config.jsonc", dir);
    FILE *f = fopen(path, "w");
    CHECK(f != NULL);
    fputs("{ broken,,", f);
    fclose(f);
    bot_state_t st;
    memset(&st, 0, sizeof st);
    pthread_rwlock_init(&st.mu, NULL);
    strmap_init(&st.linux);
    strmap_init(&st.shells);
    st.ai_prompt = strdup("");
    CHECK(bot_persist(&st) == -1);
    /* file untouched */
    char *raw = NULL;
    size_t len = 0;
    CHECK(read_file(path, &raw, &len) == 0);
    CHECK(strstr(raw, "broken") != NULL);
    free(raw);
    free(st.ai_prompt);
    pthread_rwlock_destroy(&st.mu);
}

TEST(boo_detection) {
    CHECK(events_is_boo("boo"));
    CHECK(events_is_boo("Boo!"));
    CHECK(events_is_boo("well, boo on you"));
    CHECK(!events_is_boo("book"));
    CHECK(!events_is_boo("boos"));
    CHECK(!events_is_boo("hello"));
    CHECK(!events_is_boo(""));
}

TEST(artixy_suffix) {
    char *t = events_artixy_text("hello there.ar");
    CHECK(t != NULL);
    CHECK_STR_EQ(t, "hello there");
    free(t);
    t = events_artixy_text("hi.ar   ");
    CHECK(t != NULL);
    CHECK_STR_EQ(t, "hi");
    free(t);
    CHECK(events_artixy_text("no suffix") == NULL);
    CHECK(events_artixy_text(".ar") == NULL);
    CHECK(events_artixy_text("") == NULL);
}

int main(void) {
    RUN(sensitive_send_names_refused);
    RUN(share_dot_components);
    RUN(guest_dir_normalization);
    RUN(upload_allowlist);
    RUN(target_id_parse);
    RUN(message_ref_parse);
    RUN(discord_name_sanitize);
    RUN(runas_validation);
    RUN(sudoers_script);
    RUN(sh_escape);
    RUN(strip_sgr);
    RUN(codeblock_and_tail);
    RUN(access_checks);
    RUN(persist_roundtrip);
    RUN(persist_refuses_broken_file);
    RUN(boo_detection);
    RUN(artixy_suffix);
    TEST_REPORT();
}
