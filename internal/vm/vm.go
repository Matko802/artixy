package vm

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"os/exec"
	"strings"
	"time"

	"github.com/Matko802/artixy/internal/config"
)

const VirshTimeoutSecs = 30

func cmdOutput(bin string, args []string, secs uint64) ([]byte, []byte, error) {
	if secs < 1 {
		secs = 1
	}
	ctx, cancel := context.WithTimeout(context.Background(), time.Duration(secs)*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, bin, args...)
	var stderr []byte
	out, err := cmd.Output()
	if ctx.Err() == context.DeadlineExceeded {
		return nil, nil, fmt.Errorf("%s %s timed out after %ds", bin, strings.Join(args, " "), secs)
	}
	if err != nil {
		if ee, ok := err.(*exec.ExitError); ok {
			stderr = ee.Stderr
		}
		return out, stderr, err
	}
	return out, stderr, nil
}

func virshOutput(args []string) ([]byte, []byte, error) {
	full := append([]string{"--connect", "qemu:///system"}, args...)
	return cmdOutput("virsh", full, VirshTimeoutSecs)
}

// Virsh runs virsh --connect qemu:///system <args> and returns trimmed stdout.
func Virsh(args []string) (string, error) {
	out, stderr, err := virshOutput(args)
	if err != nil {
		return "", fmt.Errorf("virsh %s failed:\n%s", strings.Join(args, " "), strings.TrimSpace(string(stderr)))
	}
	return strings.TrimSpace(string(out)), nil
}

func AgentPing(vm string) bool {
	payload := `{"execute":"guest-ping"}`
	_, _, err := virshOutput([]string{"qemu-agent-command", vm, payload})
	return err == nil
}

func WaitAgent(vm string, secs uint64) bool {
	if secs < 1 {
		secs = 1
	}
	for i := uint64(0); i < secs; i++ {
		if AgentPing(vm) {
			return true
		}
		time.Sleep(time.Second)
	}
	return AgentPing(vm)
}

func GuestStatus(vm string, pid int64) (*int64, error) {
	payload := fmt.Sprintf(`{"execute":"guest-exec-status","arguments":{"pid":%d}}`, pid)
	out, _, err := virshOutput([]string{"qemu-agent-command", vm, payload})
	if err != nil {
		return nil, nil
	}
	if len(strings.TrimSpace(string(out))) == 0 {
		return nil, nil
	}
	var v struct {
		Return struct {
			Exited   bool   `json:"exited"`
			Exitcode *int64 `json:"exitcode"`
		} `json:"return"`
	}
	if err := json.Unmarshal(out, &v); err != nil {
		return nil, fmt.Errorf("status poll: %w", err)
	}
	if v.Return.Exited {
		code := int64(-1)
		if v.Return.Exitcode != nil {
			code = *v.Return.Exitcode
		}
		return &code, nil
	}
	return nil, nil
}

// GuestExec runs path+args in the guest via qemu-guest-agent and waits up to timeoutS.
func GuestExec(vm, path string, args []string, capture bool, timeoutS uint64) (int64, string, string, error) {
	pid, err := GuestLaunchRaw(vm, path, args, capture)
	if err != nil {
		return 0, "", "", err
	}
	deadline := time.Now().Add(time.Duration(max1(timeoutS)) * time.Second)
	for {
		payload := fmt.Sprintf(`{"execute":"guest-exec-status","arguments":{"pid":%d}}`, pid)
		out, _, err := virshOutput([]string{"qemu-agent-command", vm, payload})
		if err == nil && len(strings.TrimSpace(string(out))) > 0 {
			var s struct {
				Return struct {
					Exited   bool    `json:"exited"`
					Exitcode int64   `json:"exitcode"`
					OutData  *string `json:"out-data"`
					ErrData  *string `json:"err-data"`
				} `json:"return"`
			}
			if json.Unmarshal(out, &s) == nil && s.Return.Exited {
				if !capture {
					return s.Return.Exitcode, "", "", nil
				}
				dec := func(v *string) string {
					if v == nil {
						return ""
					}
					b, err := base64.StdEncoding.DecodeString(*v)
					if err != nil {
						return ""
					}
					return string(b)
				}
				return s.Return.Exitcode, dec(s.Return.OutData), dec(s.Return.ErrData), nil
			}
		}
		if time.Now().After(deadline) {
			break
		}
		time.Sleep(time.Second)
	}
	return 0, "", "", fmt.Errorf("guest-exec timed out waiting for exit (it may still be running in the guest)")
}

func max1(v uint64) uint64 {
	if v < 1 {
		return 1
	}
	return v
}

func GuestLaunchRaw(vm, path string, args []string, capture bool) (int64, error) {
	for i := 0; i < 3; i++ {
		payloadMap := map[string]interface{}{
			"execute": "guest-exec",
			"arguments": map[string]interface{}{
				"path": path, "arg": args, "capture-output": capture,
			},
		}
		raw, _ := json.Marshal(payloadMap)
		out, stderr, err := virshOutput([]string{"qemu-agent-command", vm, string(raw)})
		if err != nil {
			return 0, fmt.Errorf("guest-exec launch failed:\n%s", strings.TrimSpace(string(stderr)))
		}
		if len(strings.TrimSpace(string(out))) == 0 {
			time.Sleep(time.Second)
			continue
		}
		var v struct {
			Return struct {
				Pid *int64 `json:"pid"`
			} `json:"return"`
		}
		if err := json.Unmarshal(out, &v); err != nil {
			return 0, fmt.Errorf("launch: %w", err)
		}
		if v.Return.Pid == nil {
			return 0, fmt.Errorf("guest-exec: no pid returned")
		}
		return *v.Return.Pid, nil
	}
	return 0, fmt.Errorf("launch: agent returned empty response 3x")
}

func LinkedUser(d *config.Data, uid uint64) string {
	d.Mu.RLock()
	defer d.Mu.RUnlock()
	key := fmt.Sprintf("%d", uid)
	if v, ok := d.Allowed.Linux[key]; ok {
		return v
	}
	return ""
}

func KillTreeScript(pid int64) string {
	return fmt.Sprintf("killtree() { for c in $(pgrep -P \"$1\"); do killtree \"$c\"; done; kill \"$1\" 2>/dev/null; }; killtree %d", pid)
}

func GuestFileB64(vm, path string, maxB64 int) []byte {
	for _, bin := range []string{"/usr/bin/base64", "/bin/base64"} {
		code, out, _, _ := GuestExec(vm, bin, []string{"-w0", "--", path}, true, 20)
		if code == 0 {
			if out == "" || len(out) > maxB64 {
				return nil
			}
			b, err := base64.StdEncoding.DecodeString(strings.TrimRight(out, "\n"))
			if err != nil {
				return nil
			}
			return b
		}
	}
	return nil
}

func GuestKillTree(vm string, pid int64) {
	snippet := KillTreeScript(pid)
	_, _, _, _ = GuestExec(vm, "/bin/bash", []string{"-c", snippet}, false, 15)
}
