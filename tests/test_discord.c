/* Unit tests: prefix parser, slash JSON, event parsing. */
#include "test.h"

#include "../src/commands.h"
#include "../src/discord.h"
#include "../src/util.h"

#include <jansson.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

TEST(prefix_basic) {
    char name[64];
    const char *arg = NULL;
    CHECK(cmd_parse_prefix("/status", name, sizeof name, &arg) == 0);
    CHECK_STR_EQ(name, "status");
    CHECK_STR_EQ(arg, "");
    CHECK(cmd_parse_prefix(";run ls -la", name, sizeof name, &arg) == 0);
    CHECK_STR_EQ(name, "run");
    CHECK_STR_EQ(arg, "ls -la");
    CHECK(cmd_parse_prefix("/purge_replies 123 50", name, sizeof name, &arg) == 0);
    CHECK_STR_EQ(name, "purge_replies");
    CHECK(cmd_parse_prefix("/purgereplies 123", name, sizeof name, &arg) == 0);
    CHECK_STR_EQ(name, "purge_replies");
}

TEST(prefix_rejects) {
    char name[64];
    const char *arg = NULL;
    CHECK(cmd_parse_prefix("hello", name, sizeof name, &arg) != 0);
    CHECK(cmd_parse_prefix("/notacommand x", name, sizeof name, &arg) != 0);
    CHECK(cmd_parse_prefix("/", name, sizeof name, &arg) != 0);
    CHECK(cmd_parse_prefix("", name, sizeof name, &arg) != 0);
    CHECK(cmd_parse_prefix("/STATUS  ", name, sizeof name, &arg) == 0);
    CHECK_STR_EQ(name, "status");
}

TEST(slash_json_wellformed) {
    char *s = commands_json();
    CHECK(s != NULL);
    json_error_t e;
    json_t *arr = json_loads(s, 0, &e);
    CHECK(arr != NULL);
    CHECK(json_is_array(arr));
    CHECK(json_array_size(arr) == 20);
    /* every entry has name + description */
    for (size_t i = 0; i < json_array_size(arr); i++) {
        json_t *c = json_array_get(arr, i);
        CHECK(json_is_string(json_object_get(c, "name")));
        CHECK(json_is_string(json_object_get(c, "description")));
    }
    json_decref(arr);
    free(s);
}

TEST(parse_message_json) {
    const char *raw = "{"
                      "\"id\":\"111\",\"channel_id\":\"222\",\"guild_id\":\"333\","
                      "\"author\":{\"id\":\"444\",\"username\":\"bob\",\"bot\":false},"
                      "\"content\":\"/status\","
                      "\"mentions\":[{\"id\":\"555\"}],"
                      "\"attachments\":[{\"id\":\"666\",\"filename\":\"f.txt\","
                      "\"url\":\"http://x/f\",\"size\":12}],"
                      "\"message_reference\":{\"channel_id\":\"222\",\"message_id\":\"999\"},"
                      "\"referenced_message\":{\"id\":\"999\","
                      "\"author\":{\"id\":\"777\",\"username\":\"artixy\"},"
                      "\"content\":\"hi\"}}";
    json_error_t e;
    json_t *o = json_loads(raw, 0, &e);
    CHECK(o != NULL);
    disc_message_t m;
    CHECK(disc_parse_message(o, &m) == 0);
    json_decref(o);
    CHECK(m.id == 111 && m.channel_id == 222 && m.guild_id == 333);
    CHECK(m.author_id == 444 && !m.author_bot);
    CHECK_STR_EQ(m.author_name, "bob");
    CHECK_STR_EQ(m.content, "/status");
    CHECK(m.n_mentions == 1 && m.mentions[0] == 555);
    CHECK(m.n_attachments == 1 && m.attachments[0].size == 12);
    CHECK_STR_EQ(m.attachments[0].filename, "f.txt");
    CHECK(m.has_reference && m.ref_message_id == 999);
    CHECK(m.has_ref_msg && m.ref_msg_author_id == 777);
    disc_message_free(&m);
}

TEST(parse_interaction_json) {
    const char *raw = "{"
                      "\"id\":\"1\",\"token\":\"tok\",\"type\":2,"
                      "\"channel_id\":\"10\",\"member\":{\"nick\":\"N\",\"user\":"
                      "{\"id\":\"20\",\"username\":\"ann\"}},"
                      "\"data\":{\"name\":\"run\",\"options\":"
                      "[{\"name\":\"cmd\",\"type\":3,\"value\":\"ls\"}]}}";
    json_error_t e;
    json_t *o = json_loads(raw, 0, &e);
    CHECK(o != NULL);
    disc_interaction_t in;
    CHECK(disc_parse_interaction(o, &in) == 0);
    json_decref(o);
    CHECK(in.channel_id == 10 && in.author_id == 20);
    CHECK_STR_EQ(in.command, "run");
    CHECK_STR_EQ(in.member_nick, "N");
    CHECK(in.n_options == 1 && in.options[0].type == 3);
    CHECK_STR_EQ(in.options[0].str_val, "ls");
    disc_interaction_free(&in);
}

TEST(parse_interaction_user_option) {    const char *raw = "{"
                      "\"id\":\"1\",\"token\":\"tok\",\"type\":2,\"channel_id\":\"10\","
                      "\"user\":{\"id\":\"30\",\"username\":\"zed\"},"
                      "\"data\":{\"name\":\"user\",\"resolved\":"
                      "{\"users\":{\"40\":{\"id\":\"40\",\"username\":\"tar\"}}},"
                      "\"options\":[{\"name\":\"action\",\"type\":3,\"value\":\"add\"},"
                      "{\"name\":\"user\",\"type\":6,\"value\":\"40\"}]}}";
    json_error_t e;
    json_t *o = json_loads(raw, 0, &e);
    CHECK(o != NULL);
    disc_interaction_t in;
    CHECK(disc_parse_interaction(o, &in) == 0);
    json_decref(o);
    CHECK(in.guild_id == 0); /* DM: no guild_id */
    CHECK(in.n_options == 2 && in.options[1].user_id == 40);
    CHECK(in.n_resolved_users == 1);
    CHECK_STR_EQ(in.resolved_users[0].username, "tar");
    disc_interaction_free(&in);
}

TEST(clone_message_roundtrip) {
    const char *raw = "{"
                      "\"id\":\"111\",\"channel_id\":\"222\","
                      "\"author\":{\"id\":\"444\",\"username\":\"bob\"},"
                      "\"content\":\"hi\","
                      "\"mentions\":[{\"id\":\"555\"}],"
                      "\"attachments\":[{\"id\":\"666\",\"filename\":\"f.txt\","
                      "\"url\":\"http://x/f\",\"size\":12}]}";
    json_error_t e;
    json_t *o = json_loads(raw, 0, &e);
    CHECK(o != NULL);
    disc_message_t m, c;
    CHECK(disc_parse_message(o, &m) == 0);
    json_decref(o);
    CHECK(disc_message_clone(&m, &c) == 0);
    CHECK(c.id == 111 && c.author_id == 444);
    CHECK_STR_EQ(c.content, "hi");
    CHECK(c.n_mentions == 1 && c.mentions[0] == 555);
    CHECK(c.n_attachments == 1);
    CHECK_STR_EQ(c.attachments[0].url, "http://x/f");
    disc_message_free(&m);
    /* clone survives source free */
    CHECK_STR_EQ(c.author_name, "bob");
    disc_message_free(&c);
}

TEST(clone_interaction_roundtrip) {
    const char *raw = "{"
                      "\"id\":\"1\",\"token\":\"tok\",\"type\":2,"
                      "\"channel_id\":\"10\","
                      "\"user\":{\"id\":\"30\",\"username\":\"zed\"},"
                      "\"data\":{\"name\":\"run\",\"options\":"
                      "[{\"name\":\"cmd\",\"type\":3,\"value\":\"ls\"}]}}";
    json_error_t e;
    json_t *o = json_loads(raw, 0, &e);
    CHECK(o != NULL);
    disc_interaction_t in, c;
    CHECK(disc_parse_interaction(o, &in) == 0);
    json_decref(o);
    CHECK(disc_interaction_clone(&in, &c) == 0);
    CHECK_STR_EQ(c.token, "tok");
    CHECK_STR_EQ(c.command, "run");
    CHECK(c.n_options == 1);
    CHECK_STR_EQ(c.options[0].str_val, "ls");
    disc_interaction_free(&in);
    CHECK_STR_EQ(c.author_name, "zed");
    disc_interaction_free(&c);
}

static const char stress_raw[] =
    "{\"id\":\"111\",\"channel_id\":\"222\","
    "\"author\":{\"id\":\"444\",\"username\":\"bob\"},"
    "\"content\":\"/status\","
    "\"mentions\":[{\"id\":\"555\"}],"
    "\"referenced_message\":{\"id\":\"999\","
    "\"author\":{\"id\":\"777\",\"username\":\"artixy\"},"
    "\"content\":\"hi\"}}";

static void *stress_worker(void *arg) {
    (void)arg;
    for (int i = 0; i < 300; i++) {
        json_error_t e;
        json_t *o = json_loads(stress_raw, 0, &e);
        if (!o)
            return (void *)1;
        disc_message_t m, c;
        if (disc_parse_message(o, &m) != 0) {
            json_decref(o);
            return (void *)1;
        }
        json_decref(o);
        if (disc_message_clone(&m, &c) != 0) {
            disc_message_free(&m);
            return (void *)1;
        }
        if (c.author_id != 444 || c.ref_msg_author_id != 777) {
            disc_message_free(&m);
            disc_message_free(&c);
            return (void *)1;
        }
        disc_message_free(&m);
        disc_message_free(&c);
    }
    return NULL;
}

TEST(clone_thread_stress) {
    pthread_t th[8];
    for (int i = 0; i < 8; i++) {
        if (pthread_create(&th[i], NULL, stress_worker, NULL) != 0) {
            CHECK(0); /* thread spawn failed */
            return;
        }
    }
    for (int i = 0; i < 8; i++) {
        void *ret = NULL;
        pthread_join(th[i], &ret);
        CHECK(ret == NULL);
    }
}

int main(void) {
    RUN(prefix_basic);
    RUN(prefix_rejects);
    RUN(slash_json_wellformed);
    RUN(parse_message_json);
    RUN(parse_interaction_json);
    RUN(parse_interaction_user_option);
    RUN(clone_message_roundtrip);
    RUN(clone_interaction_roundtrip);
    RUN(clone_thread_stress);
    TEST_REPORT();
}
