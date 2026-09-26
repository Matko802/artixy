/* Unit tests for the JSONC config module. */
#include "test.h"

#include "../src/config.h"
#include "../src/util.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

TEST(strip_comments_and_trailing_commas) {
    const char *src = "{\n"
                      "  // line comment\n"
                      "  \"a\": 1, /* block */\n"
                      "  \"b\": \"x,}y // not a comment\",\n"
                      "  \"c\": [1, 2,],\n"
                      "}";
    char *out = config_strip_jsonc(src, strlen(src));
    CHECK(out != NULL);
    CHECK(strstr(out, "line comment") == NULL);
    CHECK(strstr(out, "block") == NULL);
    CHECK(strstr(out, "\"x,}y // not a comment\"") != NULL);
    CHECK(strstr(out, "2,]") == NULL);
    CHECK(strstr(out, "[1, 2]") != NULL);
    free(out);
}

TEST(block_comment_multiline) {
    const char *src = "{\"a\": /* multi\nline\ncomment */ 5}";
    char *out = config_strip_jsonc(src, strlen(src));
    CHECK(out != NULL);
    CHECK_STR_EQ(out, "{\"a\":  5}");
    free(out);
}

TEST(escaped_quotes_survive) {
    const char *src = "{\"a\": \"q\\\"}//,x\", \"b\": 1,}";
    char *out = config_strip_jsonc(src, strlen(src));
    CHECK(out != NULL);
    CHECK(strstr(out, "\"q\\\"}//,x\"") != NULL);
    CHECK(strstr(out, "1,}") == NULL);
    free(out);
}

/* Point config at a scratch dir. */
static void use_tmpdir(char *tmpl_out, size_t n) {
    char *tmp = getenv("TMPDIR");
    snprintf(tmpl_out, n, "%s/artixy-test-XXXXXX", tmp ? tmp : "/tmp");
    CHECK(mkdtemp(tmpl_out) != NULL);
    setenv("XDG_CONFIG_HOME", tmpl_out, 1);
}

TEST(template_parses_with_defaults) {
    char dir[4096];
    use_tmpdir(dir, sizeof dir);
    config_ensure_template();
    file_config_t c;
    CHECK(config_load(&c) == 0);
    CHECK_STR_EQ(c.ai_model, "llama3.1");
    CHECK_STR_EQ(c.ollama_host, "http://127.0.0.1:11434");
    CHECK(!c.has_owner_id);
    CHECK(c.discord_token == NULL);
    struct stat st;
    char path[8192];
    snprintf(path, sizeof path, "%s/artixy/config.jsonc", dir);
    CHECK(stat(path, &st) == 0);
    CHECK((st.st_mode & 0777) == 0600);
    file_config_free(&c);
}

TEST(load_full_config) {
    char dir[4096];
    use_tmpdir(dir, sizeof dir);
    char path[8192];
    snprintf(path, sizeof path, "%s/artixy", dir);
    CHECK(mkdir_p(path) == 0);
    snprintf(path, sizeof path, "%s/artixy/config.jsonc", dir);
    FILE *f = fopen(path, "w");
    CHECK(f != NULL);
    fputs("{\n"
          "  // comment\n"
          "  \"owner_id\": 123,\n"
          "  \"discord_token\": \"tok\",\n"
          "  \"vm_name\": \"artix\",\n"
          "  \"blocked_ids\": [7,],\n"
          "  \"notify_channel\": 42,\n"
          "  \"managers\": [1, 2],\n"
          "  \"admin_ids\": [],\n"
          "  \"linux\": {\"1\": \"bob\",},\n"
          "  \"shells\": {\"1\": \"fish\"},\n"
          "  \"ai_enabled\": true,\n"
          "  \"ai_model\": \"qwen3.5:4b\",\n"
          "  \"ai_prompt\": \"hi\\nthere\",\n"
          "  \"ai_temperature\": 9.5,\n"
          "  \"war_mode\": true,\n"
          "}\n",
          f);
    fclose(f);
    file_config_t c;
    CHECK(config_load(&c) == 0);
    CHECK(c.has_owner_id && c.owner_id == 123);
    CHECK_STR_EQ(c.discord_token, "tok");
    CHECK_STR_EQ(c.vm_name, "artix");
    CHECK(c.n_blocked == 1 && c.blocked_ids[0] == 7);
    CHECK(c.has_notify_channel && c.notify_channel == 42);
    CHECK(c.n_managers == 2);
    CHECK_STR_EQ(strmap_get(&c.linux, "1"), "bob");
    CHECK_STR_EQ(strmap_get(&c.shells, "1"), "fish");
    CHECK(c.ai_enabled);
    CHECK_STR_EQ(c.ai_model, "qwen3.5:4b");
    CHECK_STR_EQ(c.ai_prompt, "hi\nthere");
    CHECK(c.ai_temperature == 2.0); /* clamped */
    CHECK(c.war_mode);
    file_config_free(&c);
}

TEST(parse_error_keeps_old_settings) {
    char dir[4096];
    use_tmpdir(dir, sizeof dir);
    char path[8192];
    snprintf(path, sizeof path, "%s/artixy", dir);
    CHECK(mkdir_p(path) == 0);
    snprintf(path, sizeof path, "%s/artixy/config.jsonc", dir);
    FILE *f = fopen(path, "w");
    CHECK(f != NULL);
    fputs("{ broken json,,,", f);
    fclose(f);
    file_config_t c;
    CHECK(config_load(&c) == -2);
    file_config_free(&c);
}

TEST(model_name_validation) {
    CHECK(config_valid_model_name("llama3.1"));
    CHECK(config_valid_model_name("qwen2.5-coder:7b"));
    CHECK(!config_valid_model_name(""));
    CHECK(!config_valid_model_name("bad name"));
    CHECK(!config_valid_model_name("../evil"));
    CHECK(!config_valid_model_name(".lead"));
    CHECK(!config_valid_model_name("trail."));
}

TEST(temperature_clamp) {
    CHECK(config_clamp_temperature(0.7) == 0.7);
    CHECK(config_clamp_temperature(-1.0) == 0.0);
    CHECK(config_clamp_temperature(9.0) == 2.0);
}

int main(void) {
    RUN(strip_comments_and_trailing_commas);
    RUN(block_comment_multiline);
    RUN(escaped_quotes_survive);
    RUN(template_parses_with_defaults);
    RUN(load_full_config);
    RUN(parse_error_keeps_old_settings);
    RUN(model_name_validation);
    RUN(temperature_clamp);
    TEST_REPORT();
}
