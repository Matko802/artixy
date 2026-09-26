package bot

import (
	"os"
	"syscall"
)

func detachedRestart(exe string) bool {
	// Try systemd first (Pi plain install uses artixy.service).
	// Fall back to setsid re-exec.
	attr := &os.ProcAttr{
		Dir:   "",
		Env:   os.Environ(),
		Files: []*os.File{nil, nil, nil},
		Sys:   &syscall.SysProcAttr{Setsid: true},
	}
	if _, err := os.StartProcess(exe, []string{exe}, attr); err != nil {
		return false
	}
	// Exit current process so systemd (Restart=always/on-failure off path)
	// or the new process takes over. Use Exit to mimic Rust's process::exit(0).
	os.Exit(0)
	return true
}
