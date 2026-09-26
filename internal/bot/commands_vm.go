package bot

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/Matko802/artixy/internal/config"
	"github.com/Matko802/artixy/internal/live"
	"github.com/Matko802/artixy/internal/util"
	"github.com/Matko802/artixy/internal/vm"
)

func cmdHelp(b *Bot, ctx *CmdCtx, arg string) {
	ctx.Reply(HelpText)
}

func cmdPs(b *Bot, ctx *CmdCtx, arg string) {
	if !NeedAuth(b, ctx) {
		return
	}
	out, err := vm.Virsh([]string{"list", "--all"})
	if err != nil {
		ctx.Reply(util.Codeblock(err.Error()))
		return
	}
	ctx.Reply(util.Codeblock(out))
}

func cmdStatus(b *Bot, ctx *CmdCtx, arg string) {
	if !NeedAuth(b, ctx) {
		return
	}
	v := b.RequireVM(ctx.ChannelID)
	if v == "" {
		return
	}
	state, err := vm.Virsh([]string{"domstate", v})
	if err != nil {
		state = err.Error()
	}
	agent := "agent: n/a (off)"
	if strings.TrimSpace(state) == "running" {
		if vm.AgentPing(v) {
			agent = "agent: up"
		} else {
			agent = "agent: DOWN"
		}
	}
	ctx.Reply(fmt.Sprintf("`%s`: %s | %s", v, strings.TrimSpace(state), agent))
}

func cmdStart(b *Bot, ctx *CmdCtx, arg string) {
	if !NeedAuth(b, ctx) {
		return
	}
	v := b.RequireVM(ctx.ChannelID)
	if v == "" {
		return
	}
	state, _ := vm.Virsh([]string{"domstate", v})
	startedHere := false
	var t0 time.Time
	if strings.TrimSpace(state) == "running" {
		ctx.Reply(fmt.Sprintf("`%s` is already on. Waiting for the guest agent…", v))
	} else {
		if _, err := vm.Virsh([]string{"start", v}); err != nil {
			ctx.Reply(util.Codeblock(err.Error()))
			return
		}
		startedHere = true
		t0 = time.Now()
		ctx.Reply(fmt.Sprintf("`%s` starting. Waiting for the guest agent…", v))
	}
	if vm.WaitAgent(v, 90) {
		if startedHere {
			secs := int(time.Since(t0).Seconds())
			took := fmt.Sprintf("%ds", secs)
			if secs >= 60 {
				took = fmt.Sprintf("%dm %ds", secs/60, secs%60)
			}
			ctx.Reply(fmt.Sprintf("%s booted in %s.", v, took))
		} else {
			ctx.Reply(fmt.Sprintf("`%s` is on and the guest agent answers.", v))
		}
	} else {
		ctx.Reply("Artix bot is on /start to boot artix")
	}
}

func cmdStop(b *Bot, ctx *CmdCtx, arg string) {
	if !NeedAuth(b, ctx) {
		return
	}
	v := b.RequireVM(ctx.ChannelID)
	if v == "" {
		return
	}
	ctx.Reply(fmt.Sprintf("stopping %s…", v))
	if _, err := vm.Virsh([]string{"shutdown", v}); err != nil {
		ctx.Reply(util.Codeblock(err.Error()))
		return
	}
	for i := 0; i < 60; i++ {
		time.Sleep(time.Second)
		st, _ := vm.Virsh([]string{"domstate", v})
		if strings.TrimSpace(st) == "shut off" {
			ctx.Reply(fmt.Sprintf("%s has stopped.", v))
			return
		}
	}
	ctx.Reply(fmt.Sprintf("%s is still stopping — check `;status`.", v))
}

func cmdRestart(b *Bot, ctx *CmdCtx, arg string) {
	if !NeedAuth(b, ctx) {
		return
	}
	v := b.RequireVM(ctx.ChannelID)
	if v == "" {
		return
	}
	if _, err := vm.Virsh([]string{"reboot", v}); err != nil {
		ctx.Reply(util.Codeblock(err.Error()))
		return
	}
	ctx.Reply(fmt.Sprintf("`%s` rebooting.", v))
}

func cmdInfo(b *Bot, ctx *CmdCtx, arg string) {
	if !NeedAuth(b, ctx) {
		return
	}
	v := b.RequireVM(ctx.ChannelID)
	if v == "" {
		return
	}
	out, err := vm.Virsh([]string{"dominfo", v})
	if err != nil {
		ctx.Reply(util.Codeblock(err.Error()))
		return
	}
	agent := "guest agent: up"
	if !vm.AgentPing(v) {
		agent = "guest agent: DOWN (install qemu-guest-agent in Artix)"
	}
	ctx.Reply(util.Codeblock(out + "\n" + agent))
}

func cmdRun(b *Bot, ctx *CmdCtx, arg string) {
	if !NeedAuth(b, ctx) {
		return
	}
	if strings.TrimSpace(arg) == "" {
		ctx.Reply("Usage: `/run <command>`.")
		return
	}
	v := b.RequireVM(ctx.ChannelID)
	if v == "" {
		return
	}
	if !vm.AgentPing(v) {
		ctx.Reply("Artix is off.")
		return
	}
	ack, _ := b.Session.ChannelMessageSend(ctx.ChannelID, fmt.Sprintf("`run: %s` starting…", strings.TrimSpace(arg)))
	if ack == nil {
		return
	}
	linked := vm.LinkedUser(b.Data, parseU64(ctx.AuthorID))
	runas := ""
	if util.ValidRunas(linked) {
		runas = linked
	}
	// scrub IP for non-owners (matches Rust: !is_owner)
	scrubIP := !b.IsOwner(ctx.AuthorID)
	live.BeginRun(b.Session, b.Data, b.Live, ctx.ChannelID, ack.ID, ctx.AuthorID, ctx.AuthorName, v, strings.TrimSpace(arg), runas, scrubIP)
}

func cmdSend(b *Bot, ctx *CmdCtx, arg string) {
	if !NeedAuth(b, ctx) {
		return
	}
	p := strings.TrimSpace(arg)
	if !strings.HasPrefix(p, "/") {
		ctx.Reply("Absolute path only.")
		return
	}
	root := util.ProjectDir()
	// resolve share dir
	share := filepath.Join(root, "share")
	shareAbs, err := filepath.Abs(share)
	if err != nil {
		ctx.Reply(util.Codeblock(fmt.Sprintf("can't resolve project dir: %v", err)))
		return
	}
	if _, err := os.Stat(shareAbs); err != nil {
		ctx.Reply("Nothing is sendable yet — create a `share/` dir in the bot's project dir and put files there.")
		return
	}
	targetAbs, err := filepath.Abs(p)
	if err != nil {
		ctx.Reply("No readable file there (must exist, absolute path, under the bot's `share/` dir, ~20MB max).")
		return
	}
	// ensure target is under share (lexical + symlink-safe best effort)
	realTarget, err := filepath.EvalSymlinks(targetAbs)
	if err != nil {
		ctx.Reply("No readable file there (must exist, absolute path, under the bot's `share/` dir, ~20MB max).")
		return
	}
	realShare, _ := filepath.EvalSymlinks(shareAbs)
	if realTarget != realShare && !strings.HasPrefix(realTarget, realShare+string(os.PathSeparator)) {
		ctx.Reply("That path is outside the bot's `share/` dir — not sending it.")
		return
	}
	rel, _ := filepath.Rel(realShare, realTarget)
	if ShareRelHasDotComponent(rel) {
		ctx.Reply("Dotfiles and dot-dirs are never sent.")
		return
	}
	fileName := filepath.Base(realTarget)
	if IsSensitiveSendName(fileName) {
		fmt.Fprintf(os.Stderr, "send refused (sensitive name): %s\n", fileName)
		ctx.Reply("Refusing to send secrets, keys, tokens or config files.")
		return
	}
	if cfgReal, err := filepath.EvalSymlinks(config.ConfigFilePath()); err == nil && realTarget == cfgReal {
		ctx.Reply("Refusing to send secrets, keys, tokens or config files.")
		return
	}
	fi, err := os.Stat(realTarget)
	if err != nil || !fi.Mode().IsRegular() || fi.Size() >= 20*1024*1024 {
		ctx.Reply("No readable file there (absolute path under the bot's `share/` dir, ~20MB max).")
		return
	}
	data, err := os.ReadFile(realTarget)
	if err != nil {
		ctx.Reply(util.Codeblock(fmt.Sprintf("attach failed: %v", err)))
		return
	}
	ctx.ReplyWithFiles("", []FileData{{Name: fileName, Data: data}})
}

func shEscape(s string) string {
	return "'" + strings.ReplaceAll(s, "'", "'\\''") + "'"
}

func guestMkdir(v, dir string) error {
	code, _, _, err := vm.GuestExec(v, "/bin/mkdir", []string{"-p", "--", dir}, false, 15)
	if err != nil && strings.Contains(err.Error(), "No such file") {
		code, _, _, err = vm.GuestExec(v, "/usr/bin/mkdir", []string{"-p", "--", dir}, false, 15)
	}
	if err != nil {
		return err
	}
	if code != 0 {
		return fmt.Errorf("mkdir failed (code %d)", code)
	}
	return nil
}

func cmdUpload(b *Bot, ctx *CmdCtx, arg string, attachURL, attachName string, attachSize int64) {
	if !NeedAuth(b, ctx) {
		return
	}
	// arg is dir; attachment comes from prefix msg or slash option
	if attachURL == "" {
		ctx.Reply("Attach a file: `/upload <file> <dir>`.")
		return
	}
	if attachSize > 20*1024*1024 {
		ctx.Reply("That file is over ~20MB — too big to upload.")
		return
	}
	name := filepath.Base(strings.TrimSpace(attachName))
	if name == "" || name == "." || name == ".." {
		ctx.Reply("Bad file name.")
		return
	}
	dir, ok := NormalizeGuestDir(arg)
	if !ok {
		ctx.Reply("Bad destination dir — use `/tmp/artixy-uploads/...` or your own `/home/<you>/...`.")
		return
	}
	linked := vm.LinkedUser(b.Data, parseU64(ctx.AuthorID))
	if !util.ValidRunas(linked) {
		linked = ""
	}
	if !AllowedUploadDir(dir, linked) {
		fmt.Fprintf(os.Stderr, "upload refused: %s -> %s\n", ctx.AuthorID, dir)
		if linked != "" {
			ctx.Reply(fmt.Sprintf("That dir is off-limits — use `/tmp/artixy-uploads/` or your own `/home/%s/`.", linked))
		} else {
			ctx.Reply("That dir is off-limits — use `/tmp/artixy-uploads/` (link a linux account with `/user add` for home uploads).")
		}
		return
	}
	v := b.RequireVM(ctx.ChannelID)
	if v == "" {
		return
	}
	data, err := DownloadAttachment(attachURL, 20*1024*1024+1)
	if err != nil {
		ctx.Reply(util.Codeblock(fmt.Sprintf("download failed: %v", err)))
		return
	}
	if int64(len(data)) > 20*1024*1024 {
		ctx.Reply("That file is over ~20MB — too big to upload.")
		return
	}
	if err := guestMkdir(v, dir); err != nil {
		ctx.Reply(util.Codeblock(fmt.Sprintf("mkdir failed: %v", err)))
		return
	}
	dest := strings.TrimRight(dir, "/") + "/" + name
	tmp := fmt.Sprintf("/tmp/artixy-up-%s.b64", util.RandomSuffix())
	b64 := base64Std(data)
	first := true
	for i := 0; i < len(b64); i += 512 * 1024 {
		end := i + 512*1024
		if end > len(b64) {
			end = len(b64)
		}
		piece := b64[i:end]
		op := ">>"
		if first {
			op = ">"
		}
		first = false
		script := fmt.Sprintf("printf '%%s' '%s' %s %s", piece, op, shEscape(tmp))
		code, _, _, err := vm.GuestExec(v, "/bin/bash", []string{"-c", script}, false, 30)
		if err != nil && strings.Contains(err.Error(), "No such file") {
			code, _, _, err = vm.GuestExec(v, "/bin/sh", []string{"-c", script}, false, 30)
		}
		if err != nil || code != 0 {
			_, _, _, _ = vm.GuestExec(v, "/bin/rm", []string{"-f", tmp}, false, 10)
			ctx.Reply(util.Codeblock(fmt.Sprintf("upload failed (code %d)", code)))
			return
		}
	}
	script := fmt.Sprintf("base64 -d %s > %s && rm -f %s && wc -c < %s", shEscape(tmp), shEscape(dest), shEscape(tmp), shEscape(dest))
	code, out, _, err := vm.GuestExec(v, "/bin/bash", []string{"-c", script}, true, 60)
	if err != nil && strings.Contains(err.Error(), "No such file") {
		code, out, _, err = vm.GuestExec(v, "/bin/sh", []string{"-c", script}, true, 60)
	}
	if err != nil || code != 0 {
		_, _, _, _ = vm.GuestExec(v, "/bin/rm", []string{"-f", tmp}, false, 10)
		ctx.Reply(util.Codeblock(fmt.Sprintf("decode failed (code %d)", code)))
		return
	}
	var landed int64 = -1
	fmt.Sscanf(strings.TrimSpace(out), "%d", &landed)
	if landed != int64(len(data)) {
		ctx.Reply(util.Codeblock(fmt.Sprintf("size mismatch: sent %d but landed %d", len(data), landed)))
		return
	}
	suffix := ""
	if linked != "" {
		code, _, _, _ := vm.GuestExec(v, "/usr/bin/chown", []string{linked + ":", dest}, false, 15)
		if code != 0 {
			code2, _, _, _ := vm.GuestExec(v, "/bin/chown", []string{linked + ":", dest}, false, 15)
			if code2 != 0 {
				suffix = " (root-owned, use sudo)"
			}
		}
	}
	ctx.Reply(fmt.Sprintf("uploaded `%s` (%d bytes) to `%s`%s.", name, len(data), dest, suffix))
}
