CC ?= gcc
CFLAGS ?= -std=c11 -Wall -Wextra -Werror -O2 -g
CPPFLAGS += -Isrc -Ithird_party -D_POSIX_C_SOURCE=200809L -D_DEFAULT_SOURCE $(shell pkg-config --cflags jansson libcurl vterm)
LDFLAGS ?=
LDLIBS = $(shell pkg-config --libs jansson libcurl vterm) -lpthread -lm

SRC = src/main.c src/config.c src/util.c src/rest.c src/gateway.c src/commands.c src/b64.c src/vm.c src/bot.c src/commands_vm.c src/commands_admin.c src/ai.c src/live.c src/scrub.c src/termrender.c src/stb_impl.c src/events.c
OBJ = $(SRC:.c=.o)
BIN = artixy

TEST_BINS = tests/test_config tests/test_discord tests/test_vm tests/test_bot tests/test_ai tests/test_live tests/test_termrender
TEST_OBJ_CONFIG = tests/test_config.o src/config.o src/util.o
TEST_OBJ_DISCORD = tests/test_discord.o src/rest.o src/gateway.o src/commands.o src/commands_vm.o src/commands_admin.o src/bot.o src/ai.o src/live.o src/scrub.o src/termrender.o src/stb_impl.o src/events.o src/config.o src/util.o src/b64.o src/vm.o
TEST_OBJ_VM = tests/test_vm.o src/vm.o src/b64.o src/util.o
TEST_OBJ_BOT = tests/test_bot.o src/bot.o src/events.o src/config.o src/util.o src/b64.o src/vm.o src/rest.o src/gateway.o src/commands.o src/commands_vm.o src/commands_admin.o src/ai.o src/live.o src/scrub.o src/termrender.o src/stb_impl.o
TEST_OBJ_AI = tests/test_ai.o src/ai.o src/util.o
TEST_OBJ_LIVE = tests/test_live.o src/live.o src/scrub.o src/b64.o src/vm.o src/bot.o src/events.o src/config.o src/util.o src/rest.o src/gateway.o src/commands.o src/commands_vm.o src/commands_admin.o src/ai.o src/termrender.o src/stb_impl.o
TEST_OBJ_TERM = tests/test_termrender.o src/termrender.o src/stb_impl.o src/util.o
FUZZ_BINS = tests/fuzz_jsonc tests/fuzz_scrub
FUZZ_OBJ_JSONC = tests/fuzz_jsonc.o src/config.o src/util.o
FUZZ_OBJ_SCRUB = tests/fuzz_scrub.o src/scrub.o src/util.o

.PHONY: all clean test asan fuzz fuzz-asan

all: $(BIN)

$(BIN): $(OBJ)
	$(CC) $(CFLAGS) $(LDFLAGS) -o $@ $(OBJ) $(LDLIBS)

%.o: %.c
	$(CC) $(CFLAGS) $(CPPFLAGS) -MMD -MP -c -o $@ $<

# Auto-generated header dependencies (rebuilt every compile).
-include $(SRC:.c=.d) tests/test_config.d tests/test_discord.d tests/test_vm.d tests/test_bot.d tests/test_ai.d tests/test_live.d tests/test_termrender.d

# Vendored single-file libs: silence their internal warnings.
src/stb_impl.o: src/stb_impl.c
	$(CC) -std=c11 -O2 -g -D_POSIX_C_SOURCE=200809L -Ithird_party -w -c -o $@ $<

tests/test_config: $(TEST_OBJ_CONFIG)
	$(CC) $(CFLAGS) $(LDFLAGS) -o $@ $(TEST_OBJ_CONFIG) $(LDLIBS)

tests/test_discord: $(TEST_OBJ_DISCORD)
	$(CC) $(CFLAGS) $(LDFLAGS) -o $@ $(TEST_OBJ_DISCORD) $(LDLIBS)

tests/test_vm: $(TEST_OBJ_VM)
	$(CC) $(CFLAGS) $(LDFLAGS) -o $@ $(TEST_OBJ_VM) $(LDLIBS)

tests/test_bot: $(TEST_OBJ_BOT)
	$(CC) $(CFLAGS) $(LDFLAGS) -o $@ $(TEST_OBJ_BOT) $(LDLIBS)

tests/test_ai: $(TEST_OBJ_AI)
	$(CC) $(CFLAGS) $(LDFLAGS) -o $@ $(TEST_OBJ_AI) $(LDLIBS)

tests/test_live: $(TEST_OBJ_LIVE)
	$(CC) $(CFLAGS) $(LDFLAGS) -o $@ $(TEST_OBJ_LIVE) $(LDLIBS)

tests/test_termrender: $(TEST_OBJ_TERM)
	$(CC) $(CFLAGS) $(LDFLAGS) -o $@ $(TEST_OBJ_TERM) $(LDLIBS)

tests/fuzz_jsonc: $(FUZZ_OBJ_JSONC)
	$(CC) $(CFLAGS) $(LDFLAGS) -o $@ $(FUZZ_OBJ_JSONC) $(LDLIBS)

tests/fuzz_scrub: $(FUZZ_OBJ_SCRUB)
	$(CC) $(CFLAGS) $(LDFLAGS) -o $@ $(FUZZ_OBJ_SCRUB) $(LDLIBS)

test: tests/test_config tests/test_discord tests/test_vm tests/test_bot tests/test_ai tests/test_live tests/test_termrender
	chmod +x tests/fakebin/virsh
	./tests/test_config
	./tests/test_discord
	./tests/test_vm
	./tests/test_bot
	./tests/test_ai
	./tests/test_live
	./tests/test_termrender

clean:
	rm -f $(OBJ) $(TEST_OBJ_CONFIG) $(TEST_OBJ_DISCORD) $(TEST_OBJ_VM) $(TEST_OBJ_BOT) $(TEST_OBJ_AI) $(TEST_OBJ_LIVE) $(TEST_OBJ_TERM) $(FUZZ_OBJ_JSONC) $(FUZZ_OBJ_SCRUB) $(BIN) $(TEST_BINS) $(FUZZ_BINS)
	rm -f $(SRC:.c=.d) tests/*.d

# Full unit suite under ASan+UBSan (leak detection off: process-lifetime
# globals like history/fonts are intentionally never freed).
# Leaves a clean tree afterwards (sanitizer objects are not reusable).
asan:
	$(MAKE) clean >/dev/null
	LSAN_OPTIONS=detect_leaks=0 $(MAKE) CFLAGS="-std=c11 -Wall -Wextra -Werror -O1 -g -fsanitize=address,undefined -fno-omit-frame-pointer" test
	$(MAKE) clean >/dev/null

# Mutational fuzzers (built with ASan+UBSan), 20k iters each by default.
fuzz: tests/fuzz_jsonc tests/fuzz_scrub
	LSAN_OPTIONS=detect_leaks=0 ./tests/fuzz_jsonc 20000
	LSAN_OPTIONS=detect_leaks=0 ./tests/fuzz_scrub 20000

fuzz-asan:
	$(MAKE) clean >/dev/null
	LSAN_OPTIONS=detect_leaks=0 $(MAKE) CFLAGS="-std=c11 -Wall -Wextra -Werror -O1 -g -fsanitize=address,undefined -fno-omit-frame-pointer" tests/fuzz_jsonc tests/fuzz_scrub
	LSAN_OPTIONS=detect_leaks=0 ./tests/fuzz_jsonc 50000
	LSAN_OPTIONS=detect_leaks=0 ./tests/fuzz_scrub 50000
	$(MAKE) clean >/dev/null
