/* Unit tests for the AI module (pure parts + error paths). */
#include "test.h"

#include "../src/ai.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

TEST(valid_model_names) {
    CHECK(ai_valid_model_name("llama3.1"));
    CHECK(ai_valid_model_name("qwen2.5-coder:7b"));
    CHECK(ai_valid_model_name("qwen3.5:4b"));
    CHECK(!ai_valid_model_name(""));
    CHECK(!ai_valid_model_name("bad name"));
    CHECK(!ai_valid_model_name("../evil"));
    CHECK(!ai_valid_model_name(".lead"));
    CHECK(!ai_valid_model_name("trail."));
}

TEST(temperature_clamp) {
    CHECK(ai_clamp_temperature(0.7) == 0.7);
    CHECK(ai_clamp_temperature(-1.0) == 0.0);
    CHECK(ai_clamp_temperature(9.0) == 2.0);
}

TEST(rate_and_full_errors) {
    CHECK(ai_is_rate_limit_err("ollama 429: too many requests"));
    CHECK(ai_is_rate_limit_err("Rate limit exceeded, try again"));
    CHECK(ai_is_rate_limit_err("quota exceeded"));
    CHECK(!ai_is_rate_limit_err("connection refused"));
    CHECK(ai_is_api_full_err("ollama 503: overloaded"));
    CHECK(ai_is_api_full_err("server is busy, try again in a bit"));
    CHECK(ai_is_api_full_err("ollama 429: too many requests"));
    CHECK(!ai_is_api_full_err("connection refused"));
    CHECK(ai_stale_history_line(ai_api_full_message()));
}

TEST(transcript_build) {
    ai_hist_item_t past[] = {
        { "user", "[bob]: hi" },
        { "assistant", "Sorry, I'm running hot right now (API full/rate limited) — try again in a minute." },
        { "assistant", "hello" },
    };
    const char *names[] = { "bob" };
    char *t = ai_build_transcript(past, 3, "[bob]: yo", names, 1);
    CHECK(t != NULL);
    CHECK(strstr(t, "[bob]: hi") != NULL);
    CHECK(strstr(t, "hello") != NULL);
    size_t n = strlen(t);
    CHECK(n >= 9 && strcmp(t + n - 9, "[bob]: yo") == 0);
    CHECK(strstr(t, "running hot") == NULL);
    free(t);
}

TEST(speaker_tag) {
    char *t = ai_speaker_tag("  Bob [x]  ");
    CHECK(t != NULL);
    CHECK_STR_EQ(t, "Bob x");
    free(t);
    t = ai_speaker_tag("   ");
    CHECK_STR_EQ(t, "someone");
    free(t);
}

TEST(mention_helpers) {
    CHECK(ai_mentions_name("hey artixy what is this"));
    CHECK(!ai_mentions_name("hello there"));
    char *s = ai_strip_mention("hello <@123> world <@!123>", 123);
    CHECK_STR_EQ(s, "hello  world");
    free(s);
    s = ai_strip_name("hey artixy buddy Artixy!");
    CHECK_STR_EQ(s, "hey buddy");
    free(s);
}

TEST(chunk_reply) {
    size_t n = 0;
    char **c = ai_chunk_reply("hi", &n);
    CHECK(c != NULL && n == 1);
    CHECK_STR_EQ(c[0], "hi");
    ai_chunks_free(c, n);
    /* long input -> multiple bounded chunks */
    char *big = malloc(6000);
    CHECK(big != NULL);
    memset(big, 'a', 5999);
    big[5999] = '\0';
    c = ai_chunk_reply(big, &n);
    free(big);
    CHECK(c != NULL && n >= 2 && n <= 4);
    for (size_t i = 0; i < n; i++)
        CHECK(strlen(c[i]) <= 1900);
    ai_chunks_free(c, n);
}

TEST(clean_and_sanitize) {
    char *c = ai_clean_reply("a\n\n\nb\n");
    CHECK_STR_EQ(c, "a\n\nb");
    free(c);
    const char *names[] = { "bob" };
    c = ai_sanitize_reply("[bob]: hello", names, 1);
    CHECK_STR_EQ(c, "hello");
    free(c);
    c = ai_sanitize_reply("To summarize, blah. Real answer here.", NULL, 0);
    CHECK(strstr(c, "Real answer") != NULL);
    CHECK(strstr(c, "To summarize") == NULL);
    free(c);
}

TEST(history_push_clear_record) {
    uint64_t ch = 41481u;
    ai_history_clear(ch);
    CHECK(ai_history_count(ch) == 0);
    ai_history_push(ch, "user", "hello");
    ai_history_push(ch, "assistant", "hi there");
    CHECK(ai_history_count(ch) == 2);
    ai_record_artixy(ch, "   ");
    CHECK(ai_history_count(ch) == 2);
    ai_record_artixy(ch, "noted");
    CHECK(ai_history_count(ch) == 3);
    /* overflow trims to 10 */
    for (int i = 0; i < 20; i++)
        ai_history_push(ch, "user", "spam message number to overflow history");
    CHECK(ai_history_count(ch) == 10);
    ai_history_clear(ch);
    CHECK(ai_history_count(ch) == 0);
}

TEST(chat_error_path_fast) {
    /* refused host must fail quickly (no 180s hang) */
    char *out = NULL;
    int rc = ai_chat("http://127.0.0.1:1", "m", 0xBEEFu, "u", "hi", "", 0.8,
                     false, &out);
    CHECK(rc != 0 && out == NULL);
    char *g = ai_glitch_text("http://127.0.0.1:1", "m", "", 0.8, false);
    CHECK(g != NULL);
    CHECK(strstr(g, "glitched out") != NULL);
    free(g);
    CHECK(ai_model_present("http://127.0.0.1:1", "m") == -1);
}

TEST(resolve_host) {
    char out[512];
    unsetenv("OLLAMA_HOST");
    unsetenv("OLLAMA_URL");
    ai_resolve_host("http://x:1/", out, sizeof out);
    CHECK_STR_EQ(out, "http://x:1");
    ai_resolve_host("", out, sizeof out);
    CHECK_STR_EQ(out, "http://127.0.0.1:11434");
    setenv("OLLAMA_HOST", "http://y:2/", 1);
    ai_resolve_host("http://x:1", out, sizeof out);
    CHECK_STR_EQ(out, "http://y:2");
    unsetenv("OLLAMA_HOST");
}

int main(void) {
    RUN(valid_model_names);
    RUN(temperature_clamp);
    RUN(rate_and_full_errors);
    RUN(transcript_build);
    RUN(speaker_tag);
    RUN(mention_helpers);
    RUN(chunk_reply);
    RUN(clean_and_sanitize);
    RUN(history_push_clear_record);
    RUN(chat_error_path_fast);
    RUN(resolve_host);
    TEST_REPORT();
}
