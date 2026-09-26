/* libvirt / qemu-guest-agent backend. Local system daemon only. */
#include "vm.h"
#include "b64.h"
#include "util.h"

#include <errno.h>
#include <jansson.h>
#include <poll.h>
#include <signal.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static _Thread_local char errbuf[1024];

const char *vm_error(void) {
    return errbuf;
}

static void set_error(const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(errbuf, sizeof errbuf, fmt, ap);
    va_end(ap);
}

/* ------------------------------------------------------------------ */
/* process runner with timeout                                          */
/* ------------------------------------------------------------------ */

typedef struct {
    int exit_code; /* valid unless timed_out */
    char *out;
    char *err;
    bool timed_out;
} cmd_result_t;

static void result_free(cmd_result_t *r) {
    free(r->out);
    free(r->err);
    r->out = r->err = NULL;
}

static int buf_append(char **buf, size_t *len, size_t *cap, const char *chunk,
                      size_t n) {
    if (*len + n + 1 > *cap) {
        size_t ncap = (*len + n + 1) * 2;
        char *nb = xrealloc(*buf, ncap);
        if (!nb)
            return -1;
        *buf = nb;
        *cap = ncap;
    }
    memcpy(*buf + *len, chunk, n);
    *len += n;
    (*buf)[*len] = '\0';
    return 0;
}

/*
 * Run bin+argv (NULL-terminated), capturing stdout/stderr, killing the
 * child if it exceeds timeout_s. Returns 0 with r filled (check timed_out
 * and exit_code), -1 on spawn/OOM error.
 */
static int cmd_run(const char *bin, char *const argv[], unsigned timeout_s,
                   cmd_result_t *r) {
    memset(r, 0, sizeof *r);
    if (timeout_s < 1)
        timeout_s = 1;
    int outp[2] = { -1, -1 }, errp[2] = { -1, -1 };
    if (pipe(outp) != 0)
        return -1;
    if (pipe(errp) != 0) {
        close(outp[0]);
        close(outp[1]);
        return -1;
    }
    pid_t pid = fork();
    if (pid < 0) {
        close(outp[0]);
        close(outp[1]);
        close(errp[0]);
        close(errp[1]);
        return -1;
    }
    if (pid == 0) {
        dup2(outp[1], STDOUT_FILENO);
        dup2(errp[1], STDERR_FILENO);
        close(outp[0]);
        close(outp[1]);
        close(errp[0]);
        close(errp[1]);
        execvp(bin, argv);
        _exit(127);
    }
    close(outp[1]);
    close(errp[1]);

    size_t olen = 0, ocap = 0, elen = 0, ecap = 0;
    char *obuf = NULL, *ebuf = NULL;
    bool out_eof = false, err_eof = false;
    struct timespec deadline;
    clock_gettime(CLOCK_MONOTONIC, &deadline);
    deadline.tv_sec += timeout_s;
    int status = 0;
    bool reaped = false;
    int rc = 0;

    while (!out_eof || !err_eof || !reaped) {
        struct timespec now;
        clock_gettime(CLOCK_MONOTONIC, &now);
        long ms_left = (deadline.tv_sec - now.tv_sec) * 1000L +
                       (deadline.tv_nsec - now.tv_nsec) / 1000000L;
        if (ms_left < 0) {
            kill(pid, SIGKILL);
            r->timed_out = true;
            /* drain quickly then reap */
            struct timespec tiny = { 0, 50 * 1000000L };
            nanosleep(&tiny, NULL);
            while (waitpid(pid, &status, WNOHANG) == 0) {
                nanosleep(&tiny, NULL);
            }
            reaped = true;
            /* fall through to drain remaining pipe data without blocking */
        }
        struct pollfd pf[2];
        int nf = 0;
        if (!out_eof) {
            pf[nf].fd = outp[0];
            pf[nf].events = POLLIN;
            nf++;
        }
        if (!err_eof) {
            pf[nf].fd = errp[0];
            pf[nf].events = POLLIN;
            nf++;
        }
        int pr = 0;
        if (nf)
            pr = poll(pf, (nfds_t)nf, r->timed_out ? 0 : 200);
        if (pr > 0) {
            for (int i = 0; i < nf; i++) {
                if (pf[i].revents & (POLLIN | POLLHUP)) {
                    char tmp[4096];
                    ssize_t n = read(pf[i].fd, tmp, sizeof tmp);
                    if (n > 0) {
                        int ok;
                        if (pf[i].fd == outp[0])
                            ok = buf_append(&obuf, &olen, &ocap, tmp, (size_t)n);
                        else
                            ok = buf_append(&ebuf, &elen, &ecap, tmp, (size_t)n);
                        if (ok != 0) {
                            rc = -1;
                            goto done;
                        }
                    } else {
                        if (pf[i].fd == outp[0])
                            out_eof = true;
                        else
                            err_eof = true;
                    }
                } else if (pf[i].revents & (POLLERR | POLLNVAL)) {
                    if (pf[i].fd == outp[0])
                        out_eof = true;
                    else
                        err_eof = true;
                }
            }
        }
        pid_t w = waitpid(pid, &status, WNOHANG);
        if (w == pid)
            reaped = true;
        if (r->timed_out && out_eof && err_eof)
            break;
        if (!nf && reaped)
            break;
    }
done:
    close(outp[0]);
    close(errp[0]);
    if (rc != 0) {
        free(obuf);
        free(ebuf);
        if (!reaped) {
            kill(pid, SIGKILL);
            waitpid(pid, &status, 0);
        }
        return -1;
    }
    if (!reaped) {
        /* child exited between last checks; blocking reap is safe now
         * that both pipes hit EOF */
        waitpid(pid, &status, 0);
    }
    r->out = obuf ? obuf : xstrdup("");
    r->err = ebuf ? ebuf : xstrdup("");
    if (!r->out || !r->err) {
        result_free(r);
        return -1;
    }
    if (WIFEXITED(status))
        r->exit_code = WEXITSTATUS(status);
    else
        r->exit_code = 128 + (WIFSIGNALED(status) ? WTERMSIG(status) : 0);
    return 0;
}

/* ------------------------------------------------------------------ */
/* virsh                                                                  */
/* ------------------------------------------------------------------ */

static char *trim(char *s) {
    while (*s == ' ' || *s == '\t' || *s == '\n' || *s == '\r')
        s++;
    size_t n = strlen(s);
    while (n > 0 && (s[n - 1] == ' ' || s[n - 1] == '\t' || s[n - 1] == '\n' ||
                     s[n - 1] == '\r'))
        s[--n] = '\0';
    return s;
}

static int virsh_output(char *const args[], cmd_result_t *r) {
    /* argv: virsh --connect qemu:///system <args...> */
    size_t n = 0;
    while (args[n])
        n++;
    char **argv = xmalloc((n + 4) * sizeof *argv);
    if (!argv) {
        set_error("out of memory");
        return -1;
    }
    argv[0] = "virsh";
    argv[1] = "--connect";
    argv[2] = (char *)VM_VIRSH_CONNECT;
    for (size_t i = 0; i <= n; i++)
        argv[3 + i] = args[i];
    int rc = cmd_run("virsh", argv, VM_VIRSH_TIMEOUT_S, r);
    free(argv);
    if (rc != 0) {
        if (r->timed_out)
            set_error("virsh timed out after %us", VM_VIRSH_TIMEOUT_S);
        else
            set_error("virsh: spawn failed: %s", strerror(errno));
        result_free(r);
        return -1;
    }
    return 0;
}

int vm_virsh(char *const args[], char **out) {
    *out = NULL;
    cmd_result_t r;
    memset(&r, 0, sizeof r);
    if (virsh_output(args, &r) != 0)
        return -1;
    if (r.timed_out) {
        set_error("virsh timed out after %us", VM_VIRSH_TIMEOUT_S);
        result_free(&r);
        return -1;
    }
    if (r.exit_code != 0) {
        /* "virsh <args> failed:\n<stderr>" (matches Rust/Go) */
        size_t n = 0;
        while (args[n])
            n++;
        size_t need = 32;
        for (size_t i = 0; i < n; i++)
            need += strlen(args[i]) + 1;
        need += strlen(r.err) + 1;
        char *msg = xmalloc(need);
        if (!msg) {
            set_error("out of memory");
            result_free(&r);
            return -1;
        }
        int w = snprintf(msg, need, "virsh");
        for (size_t i = 0; i < n && w > 0; i++)
            w += snprintf(msg + w, need - (size_t)w, " %s", args[i]);
        snprintf(msg + (w > 0 ? (size_t)w : 0), need, " failed:\n%s", trim(r.err));
        set_error("%s", msg);
        free(msg);
        result_free(&r);
        return -1;
    }
    char *t = trim(r.out);
    *out = xstrdup(t);
    result_free(&r);
    if (!*out) {
        set_error("out of memory");
        return -1;
    }
    return 0;
}

bool vm_agent_ping(const char *vm) {
    char *args[] = { "qemu-agent-command", (char *)vm,
                     "{\"execute\":\"guest-ping\"}", NULL };
    cmd_result_t r;
    memset(&r, 0, sizeof r);
    if (virsh_output(args, &r) != 0)
        return false;
    bool ok = !r.timed_out && r.exit_code == 0;
    result_free(&r);
    return ok;
}

bool vm_wait_agent(const char *vm, unsigned secs) {
    if (secs < 1)
        secs = 1;
    for (unsigned i = 0; i < secs; i++) {
        if (vm_agent_ping(vm))
            return true;
        struct timespec ts = { 1, 0 };
        nanosleep(&ts, NULL);
    }
    return vm_agent_ping(vm);
}

/* ------------------------------------------------------------------ */
/* guest agent                                                            */
/* ------------------------------------------------------------------ */

static int guest_status_full(const char *vm, long long pid, long long *code_out,
                             char **out_txt, char **err_txt);
static char *b64_of_json_str(json_t *v);

int vm_guest_status(const char *vm, long long pid, long long *code_out) {
    long long code = 0;
    char *o = NULL, *e = NULL;
    int st = guest_status_full(vm, pid, &code, &o, &e);
    free(o);
    free(e);
    if (st == 1 && code_out)
        *code_out = code;
    return st;
}

/*
 * Full status query: 1 = exited (code + decoded out/err set, possibly
 * empty), 0 = still running or unknown (empty agent response),
 * -1 = transport/parse error. out_txt/err_txt always malloc'd on 1.
 *
 * NOTE: query results are single-shot on some agents — a second status
 * query for the same pid may come back empty/failed. Callers must use
 * the out/err from the response that reported exited (no re-query).
 */
static int guest_status_full(const char *vm, long long pid, long long *code_out,
                             char **out_txt, char **err_txt) {
    char payload[128];
    snprintf(payload, sizeof payload,
             "{\"execute\":\"guest-exec-status\",\"arguments\":{\"pid\":%lld}}", pid);
    char *args[] = { "qemu-agent-command", (char *)vm, payload, NULL };
    cmd_result_t r;
    memset(&r, 0, sizeof r);
    if (virsh_output(args, &r) != 0)
        return -1;
    if (r.timed_out || r.exit_code != 0) {
        result_free(&r);
        return -1;
    }
    char *t = trim(r.out);
    if (!*t) {
        /* empty response: agent hiccup, treat as unknown (not fatal) */
        result_free(&r);
        return 0;
    }
    json_error_t e;
    json_t *root = json_loads(t, 0, &e);
    if (!root) {
        set_error("status poll: %s", e.text);
        result_free(&r);
        return -1;
    }
    int ret = 0;
    json_t *rj = json_object_get(root, "return");
    if (json_is_object(rj)) {
        json_t *ex = json_object_get(rj, "exited");
        if (json_is_true(ex)) {
            char *o = b64_of_json_str(json_object_get(rj, "out-data"));
            char *er = b64_of_json_str(json_object_get(rj, "err-data"));
            json_t *cd = json_object_get(rj, "exitcode");
            long long code = -1;
            if (json_is_integer(cd))
                code = (long long)json_integer_value(cd);
            json_decref(root);
            result_free(&r);
            if (!o || !er) {
                free(o);
                free(er);
                set_error("out of memory");
                return -1;
            }
            *code_out = code;
            *out_txt = o;
            *err_txt = er;
            return 1;
        }
    }
    json_decref(root);
    result_free(&r);
    return ret;
}

static char *b64_of_json_str(json_t *v) {
    /* Decode a base64 JSON string value into malloc'd NUL-terminated text.
     * Returns empty string when absent; NULL only on OOM/invalid. */
    if (!json_is_string(v))
        return xstrdup("");
    size_t n = 0;
    unsigned char *raw = b64_decode(json_string_value(v), &n);
    if (!raw)
        return xstrdup("");
    char *s = xmalloc(n + 1);
    if (!s) {
        free(raw);
        return NULL;
    }
    memcpy(s, raw, n);
    s[n] = '\0';
    free(raw);
    return s;
}

long long vm_guest_launch_raw(const char *vm, const char *path,
                              char *const args[], bool capture) {
    for (int attempt = 0; attempt < 3; attempt++) {
        json_t *payload = json_object();
        json_t *jargs = json_array();
        json_t *jargobj = json_object();
        if (!payload || !jargs || !jargobj) {
            json_decref(payload);
            json_decref(jargs);
            json_decref(jargobj);
            set_error("out of memory");
            return -1;
        }
        if (args) {
            for (size_t i = 0; args[i]; i++) {
                if (json_array_append_new(jargs, json_string(args[i])) != 0) {
                    json_decref(payload);
                    json_decref(jargs);
                    json_decref(jargobj);
                    set_error("out of memory");
                    return -1;
                }
            }
        }
        json_object_set_new(jargobj, "path", json_string(path));
        json_object_set_new(jargobj, "arg", jargs); /* steals jargs */
        json_object_set_new(jargobj, "capture-output",
                            json_boolean(capture));
        json_object_set_new(payload, "execute", json_string("guest-exec"));
        json_object_set_new(payload, "arguments", jargobj); /* steals */
        if (!json_object_get(payload, "execute") ||
            !json_object_get(payload, "arguments")) {
            json_decref(payload);
            set_error("out of memory");
            return -1;
        }
        char *body = json_dumps(payload, JSON_COMPACT);
        json_decref(payload);
        if (!body) {
            set_error("out of memory");
            return -1;
        }
        char *argv[] = { "qemu-agent-command", (char *)vm, body, NULL };
        cmd_result_t r;
        memset(&r, 0, sizeof r);
        int ok = virsh_output(argv, &r);
        free(body);
        if (ok != 0)
            return -1; /* virsh-level failure (error already set) */
        if (r.timed_out || r.exit_code != 0) {
            set_error("guest-exec launch failed:\n%s", trim(r.err));
            result_free(&r);
            return -1;
        }
        char *t = trim(r.out);
        if (!*t) {
            /* empty response: wait a beat and retry (agent hiccup) */
            result_free(&r);
            struct timespec ts = { 1, 0 };
            nanosleep(&ts, NULL);
            continue;
        }
        json_error_t e;
        json_t *root = json_loads(t, 0, &e);
        if (!root) {
            set_error("launch: %s", e.text);
            result_free(&r);
            return -1;
        }
        long long pid = -1;
        json_t *rj = json_object_get(root, "return");
        if (json_is_object(rj)) {
            json_t *p = json_object_get(rj, "pid");
            if (json_is_integer(p))
                pid = (long long)json_integer_value(p);
        }
        json_decref(root);
        result_free(&r);
        if (pid < 0) {
            set_error("guest-exec: no pid returned");
            return -1;
        }
        return pid;
    }
    set_error("launch: agent returned empty response 3x");
    return -1;
}

int vm_guest_exec(const char *vm, const char *path, char *const args[],
                  bool capture, unsigned timeout_s, long long *code_out,
                  char **out_txt, char **err_txt) {
    if (timeout_s < 1)
        timeout_s = 1;
    long long pid = vm_guest_launch_raw(vm, path, args, capture);
    if (pid < 0)
        return -1;
    struct timespec deadline;
    clock_gettime(CLOCK_MONOTONIC, &deadline);
    deadline.tv_sec += timeout_s;
    for (;;) {
        long long code = 0;
        char *o = NULL, *e = NULL;
        int st = guest_status_full(vm, pid, &code, &o, &e);
        if (st == 1) {
            *code_out = code;
            if (!capture) {
                free(o);
                free(e);
                if (out_txt)
                    *out_txt = NULL;
                if (err_txt)
                    *err_txt = NULL;
                return 0;
            }
            if (!o || !e) {
                free(o);
                free(e);
                set_error("out of memory");
                return -1;
            }
            if (out_txt)
                *out_txt = o;
            else
                free(o);
            if (err_txt)
                *err_txt = e;
            else
                free(e);
            return 0;
        }
        free(o);
        free(e);
        if (st < 0) {
            /* transient poll error: keep waiting until deadline */
        }
        struct timespec now;
        clock_gettime(CLOCK_MONOTONIC, &now);
        if (now.tv_sec > deadline.tv_sec ||
            (now.tv_sec == deadline.tv_sec && now.tv_nsec >= deadline.tv_nsec)) {
            set_error("guest-exec timed out waiting for exit "
                      "(it may still be running in the guest)");
            return -1;
        }
        struct timespec ts = { 1, 0 };
        nanosleep(&ts, NULL);
    }
}

char *vm_kill_tree_script(long long pid) {
    char tmp[256];
    snprintf(tmp, sizeof tmp,
             "killtree() { for c in $(pgrep -P \"$1\"); do killtree \"$c\"; "
             "done; kill \"$1\" 2>/dev/null; }; killtree %lld}",
             pid);
    return xstrdup(tmp);
}

void vm_guest_kill_tree(const char *vm, long long pid) {
    char *snippet = vm_kill_tree_script(pid);
    if (!snippet)
        return;
    char *args[] = { "/bin/bash", "-c", snippet, NULL };
    long long code = 0;
    vm_guest_exec(vm, "/bin/bash", args, false, 15, &code, NULL, NULL);
    free(snippet);
}

unsigned char *vm_guest_file_b64(const char *vm, const char *path,
                                 size_t max_b64, size_t *len_out) {
    static const char *bins[] = { "/usr/bin/base64", "/bin/base64" };
    for (size_t i = 0; i < 2; i++) {
        char *args[] = { "-w0", "--", (char *)path, NULL };
        long long code = 0;
        char *out = NULL, *err = NULL;
        int erc = vm_guest_exec(vm, bins[i], args, true, 20, &code, &out, &err);
        if (erc != 0) {
            free(out);
            free(err);
            continue;
        }
        free(err);
        unsigned char *data = NULL;
        if (code == 0 && out && *trim(out) && strlen(trim(out)) <= max_b64) {
            size_t n = 0;
            /* trim trailing newlines for the decoder */
            char *t = trim(out);
            size_t tl = strlen(t);
            while (tl > 0 && (t[tl - 1] == '\n' || t[tl - 1] == '\r'))
                t[--tl] = '\0';
            data = b64_decode(t, &n);
            if (data && len_out)
                *len_out = n;
            else
                free(data), data = NULL;
        }
        free(out);
        if (data)
            return data;
    }
    return NULL;
}
