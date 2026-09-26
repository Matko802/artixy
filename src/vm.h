#ifndef ARTIXY_VM_H
#define ARTIXY_VM_H

#include <stdbool.h>
#include <stddef.h>

/* Local system libvirt daemon (local-only by design). */
#define VM_VIRSH_CONNECT "qemu:///system"
/* Per-command timeout for plain virsh calls. */
#define VM_VIRSH_TIMEOUT_S 30u

/* Human-readable description of the last failure (thread-local). */
const char *vm_error(void);

/*
 * Run: virsh --connect qemu:///system <args...> (args NULL-terminated).
 * On success returns 0 with *out set to trimmed stdout (malloc'd, caller
 * frees). On failure returns -1 (see vm_error()); *out is NULL.
 */
int vm_virsh(char *const args[], char **out);

/* True when the guest agent answers guest-ping. */
bool vm_agent_ping(const char *vm);
/* Poll agent_ping up to secs; true if it ever answers. */
bool vm_wait_agent(const char *vm, unsigned secs);

/*
 * Query guest-exec-status for pid.
 * Returns 1 and sets *code_out when the process exited,
 * 0 when still running, -1 on agent/parse error.
 */
int vm_guest_status(const char *vm, long long pid, long long *code_out);

/*
 * Run path+args in the guest via qemu-guest-agent, waiting up to timeout_s.
 * On success returns 0 (code_out always set; out_txt and err_txt only
 * filled when capture is true, as malloc'd possibly-empty strings).
 * Returns -1 on error. args is NULL-terminated (may be NULL for no args).
 */
int vm_guest_exec(const char *vm, const char *path, char *const args[],
                  bool capture, unsigned timeout_s, long long *code_out,
                  char **out_txt, char **err_txt);

/*
 * Launch path+args via guest-exec, returning the guest pid, or -1 on error.
 * Retries up to 3x on empty agent responses (matches Rust/Go behavior).
 */
long long vm_guest_launch_raw(const char *vm, const char *path,
                              char *const args[], bool capture);

/* Fire-and-forget process-tree kill in the guest. */
void vm_guest_kill_tree(const char *vm, long long pid);

/*
 * Read a guest file via base64 (tries /usr/bin/base64 then /bin/base64).
 * Returns malloc'd bytes (*len_out set) or NULL on any failure/size cap.
 */
unsigned char *vm_guest_file_b64(const char *vm, const char *path,
                                 size_t max_b64, size_t *len_out);

/* killtree shell snippet for pid. Returns malloc'd string. */
char *vm_kill_tree_script(long long pid);

#endif
