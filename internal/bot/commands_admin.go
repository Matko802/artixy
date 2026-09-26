package bot

import (
	_ "embed"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/Matko802/artixy/internal/ai"
	"github.com/Matko802/artixy/internal/config"
	"github.com/Matko802/artixy/internal/util"
	"github.com/Matko802/artixy/internal/vm"
	"github.com/bwmarrin/discordgo"
)

//go:embed lapeace.jpg
var lapeaceJPG []byte

func cmdShell(b *Bot, ctx *CmdCtx, arg string) {
	if !NeedAuth(b, ctx) {
		return
	}
	if strings.TrimSpace(arg) == "" {
		b.Data.Mu.RLock()
		cur := b.Data.Shells[ctx.AuthorID]
		b.Data.Mu.RUnlock()
		if cur == "" {
			cur = "bash"
		}
		ctx.Reply(fmt.Sprintf("Your shell: `%s`. Change with `/shell fish` or `/shell bash`.", cur))
		return
	}
	n := strings.ToLower(strings.TrimSpace(arg))
	if n != "fish" && n != "bash" {
		ctx.Reply("Only `fish` or `bash`.")
		return
	}
	b.Data.Mu.Lock()
	if b.Data.Shells == nil {
		b.Data.Shells = map[string]string{}
	}
	b.Data.Shells[ctx.AuthorID] = n
	b.Data.Mu.Unlock()
	_ = config.PersistRuntime(b.Data)
	ctx.Reply(fmt.Sprintf("Your shell is now `%s`.", n))
}

func cmdBotrestart(b *Bot, ctx *CmdCtx, arg string) {
	if !NeedAuth(b, ctx) {
		return
	}
	// On Nix the binary is read-only; on Pi (plain systemd) we can re-exec.
	if util.DeployedViaNix() {
		ctx.Reply("Deployed from Nix — I can't re-exec myself out of a read-only `/nix/store`. Restart with `systemctl --user restart artixy`.")
		return
	}
	ctx.Reply("Restarting…")
	exe, err := os.Executable()
	if err != nil {
		exe = filepath.Join(util.ProjectDir(), "artixy")
	}
	// Detached restart via systemd if available, else re-exec.
	proc := detachedRestart(exe)
	if !proc {
		ctx.Reply(util.Codeblock("restart failed to spawn"))
	}
}

func cmdUser(b *Bot, ctx *CmdCtx, arg string, targetID, targetName string) {
	// arg: "add"|"remove"|"list"
	switch strings.ToLower(strings.TrimSpace(arg)) {
	case "add":
		if targetID == "" {
			ctx.Reply("Pick a user: `/user action:add user:@user`.")
			return
		}
		doUserAdd(b, ctx, targetID, targetName)
	case "remove":
		if targetID == "" {
			ctx.Reply("Pick a user: `/user action:remove user:@user`.")
			return
		}
		doUserDel(b, ctx, targetID)
	default:
		doUsers(b, ctx)
	}
}

func doUsers(b *Bot, ctx *CmdCtx) {
	if !NeedAuth(b, ctx) {
		return
	}
	b.Data.Mu.RLock()
	owner := b.Data.Allowed.Owner
	admins := append([]uint64(nil), b.Data.Allowed.Admins...)
	type pair struct {
		id uint64
		lx string
	}
	var pairs []pair
	for _, u := range b.Data.Allowed.Users {
		pairs = append(pairs, pair{u, b.Data.Allowed.Linux[fmt.Sprintf("%d", u)]})
	}
	b.Data.Mu.RUnlock()
	msg := fmt.Sprintf("Owner: `%s`\nAdmins:", uname(b, owner))
	if len(admins) == 0 {
		msg += " none yet — set `admin_ids` in config"
	} else {
		for _, u := range admins {
			msg += fmt.Sprintf("\n`%s`", uname(b, u))
		}
	}
	msg += "\nManagers (discord → linux):"
	if len(pairs) == 0 {
		msg += " none yet — owner runs `/user add @user`"
	} else {
		for _, p := range pairs {
			name := uname(b, p.id)
			if p.lx != "" {
				msg += fmt.Sprintf("\n`%s` → `%s`", name, p.lx)
			} else {
				msg += fmt.Sprintf("\n`%s` → (no linux account)", name)
			}
		}
	}
	ctx.Reply(msg)
}

func uname(b *Bot, uid uint64) string {
	u, err := b.Session.User(fmt.Sprintf("%d", uid))
	if err != nil || u == nil {
		return fmt.Sprintf("%d", uid)
	}
	return u.Username
}

func sanitizeDiscordName(s string) string {
	var sb strings.Builder
	for _, c := range strings.ToLower(s) {
		if (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '_' || c == '-' {
			sb.WriteRune(c)
		}
	}
	out := sb.String()
	if len(out) > 32 {
		out = out[:32]
	}
	for len(out) > 0 {
		c := out[0]
		if c == '_' || (c >= 'a' && c <= 'z') {
			break
		}
		out = out[1:]
	}
	return out
}

func sudoersScript(user string) string {
	if !util.ValidRunas(user) {
		return ""
	}
	return fmt.Sprintf("set -eu; u='%s'; f=\"/etc/sudoers.d/$u\"; printf '%%s ALL=(ALL) NOPASSWD: ALL\\n' \"$u\" >\"$f.tmp\"; printf 'Defaults:%%s !requiretty\\n' \"$u\" >>\"$f.tmp\"; chmod 0440 \"$f.tmp\"; mv -f \"$f.tmp\" \"$f\"; chmod 0440 \"$f\"; if command -v visudo >/dev/null 2>&1; then visudo -c -f \"$f\" >/dev/null; fi; if getent group wheel >/dev/null 2>&1; then usermod -aG wheel \"$u\" || true; elif getent group sudo >/dev/null 2>&1; then usermod -aG sudo \"$u\" || true; fi; h=\"$(getent passwd \"$u\" | cut -d: -f6)\"; if [ -n \"$h\" ] && [ -d \"$h\" ]; then chown \"$u\" \"$h\" 2>/dev/null || true; for d in \"$h/.cargo\" \"$h/.rustup\"; do [ -e \"$d\" ] && chown -R \"$u\" \"$d\" 2>/dev/null || true; done; fi; true", user)
}

func ensurePasswordlessSudo(v, user string) error {
	script := sudoersScript(user)
	if script == "" {
		return fmt.Errorf("refusing sudo for invalid linux name `%s`", user)
	}
	code, _, _, err := vm.GuestExec(v, "/bin/bash", []string{"-c", script}, false, 15)
	if err != nil && strings.Contains(err.Error(), "No such file") {
		code, _, _, err = vm.GuestExec(v, "/bin/sh", []string{"-c", script}, false, 15)
	}
	if err != nil {
		return err
	}
	if code != 0 {
		return fmt.Errorf("sudo setup for `%s` failed (code %d)", user, code)
	}
	return nil
}

func doUserAdd(b *Bot, ctx *CmdCtx, targetID, targetName string) {
	if !b.IsElevated(ctx.AuthorID) {
		b.PostDenied(ctx, "Owner or admin only.")
		return
	}
	uid := parseU64(targetID)
	name := sanitizeDiscordName(targetName)
	if name == "" {
		name = fmt.Sprintf("u%d", uid)
	}
	v := b.RequireVM(ctx.ChannelID)
	if v == "" {
		return
	}
	state, _ := vm.Virsh([]string{"domstate", v})
	if strings.TrimSpace(state) != "running" {
		ctx.Reply(fmt.Sprintf("Linux account not created: `%s` is off — run `/start` first, then rerun `/user add @user`.", v))
		return
	}
	if !vm.WaitAgent(v, 60) {
		ctx.Reply("Linux account not created: the guest agent is silent. Install `qemu-guest-agent` in Artix, then rerun `/user add @user`.")
		return
	}
	code, _, _, err := vm.GuestExec(v, "/usr/bin/useradd", []string{"-m", "-s", "/bin/bash", name}, false, 30)
	if err != nil && strings.Contains(err.Error(), "No such file") {
		code, _, _, err = vm.GuestExec(v, "/usr/sbin/useradd", []string{"-m", "-s", "/bin/bash", name}, false, 30)
	}
	if err != nil {
		ctx.Reply(fmt.Sprintf("Linux account not created: `useradd` in the VM failed:\n%s", util.Codeblock(err.Error())))
		return
	}
	if code == 9 {
		ctx.Reply(fmt.Sprintf("Linux user `%s` already exists, linking it.", name))
	} else if code != 0 {
		ctx.Reply(fmt.Sprintf("Linux account not created: `useradd` in the VM failed (code %d).", code))
		return
	}
	b.Data.Mu.Lock()
	if b.Data.Allowed.Linux == nil {
		b.Data.Allowed.Linux = map[string]string{}
	}
	b.Data.Allowed.Linux[fmt.Sprintf("%d", uid)] = name
	// collect targets for sudo
	seen := map[string]bool{}
	var targets []string
	all := append([]string{name}, values(b.Data.Allowed.Linux)...)
	for _, n := range all {
		if util.ValidRunas(n) && !seen[n] {
			seen[n] = true
			targets = append(targets, n)
		}
	}
	b.Data.Mu.Unlock()
	_ = config.PersistRuntime(b.Data)
	var failed []string
	for _, t := range targets {
		if err := ensurePasswordlessSudo(v, t); err != nil {
			fmt.Fprintf(os.Stderr, "sudo setup for `%s` failed: %v\n", t, err)
			failed = append(failed, t)
		}
	}
	if len(failed) == 0 {
		ctx.Reply(fmt.Sprintf("added user \"%s\" linked to `%d` — account created, passwordless sudo enabled.", name, uid))
	} else {
		ctx.Reply(fmt.Sprintf("added user \"%s\" linked to `%d` — account created, but passwordless sudo failed for: `%s`. Re-run `/user add @user` once the guest is healthy.", name, uid, strings.Join(failed, "`, `")))
	}
}

func values(m map[string]string) []string {
	out := make([]string, 0, len(m))
	for _, v := range m {
		out = append(out, v)
	}
	return out
}

func doUserDel(b *Bot, ctx *CmdCtx, targetID string) {
	if !b.IsElevated(ctx.AuthorID) {
		b.PostDenied(ctx, "Owner or admin only.")
		return
	}
	uid := parseU64(targetID)
	b.Data.Mu.Lock()
	wasManager := false
	for i, u := range b.Data.Allowed.Users {
		if u == uid {
			b.Data.Allowed.Users = append(b.Data.Allowed.Users[:i], b.Data.Allowed.Users[i+1:]...)
			wasManager = true
			break
		}
	}
	linked, hasLinked := b.Data.Allowed.Linux[fmt.Sprintf("%d", uid)]
	if hasLinked {
		delete(b.Data.Allowed.Linux, fmt.Sprintf("%d", uid))
	}
	b.Data.Mu.Unlock()
	_ = config.PersistRuntime(b.Data)
	revoked := ""
	if wasManager {
		revoked = " Bot access revoked."
	}
	if hasLinked && util.ValidRunas(linked) {
		v := b.RequireVM(ctx.ChannelID)
		if v == "" {
			return
		}
		dropin := "/etc/sudoers.d/" + linked
		_, _, _, _ = vm.GuestExec(v, "/bin/rm", []string{"-f", dropin}, false, 10)
		code, _, _, err := vm.GuestExec(v, "/usr/sbin/userdel", []string{"-r", linked}, false, 30)
		if err != nil && strings.Contains(err.Error(), "No such file") {
			code, _, _, err = vm.GuestExec(v, "/usr/bin/userdel", []string{"-r", linked}, false, 30)
		}
		if err != nil {
			ctx.Reply(fmt.Sprintf("Deleting linux `%s` failed:\n%s%s", linked, util.Codeblock(err.Error()), revoked))
		} else if code == 0 {
			ctx.Reply(fmt.Sprintf("Deleted linux `%s` for <@%d>.%s", linked, uid, revoked))
		} else {
			ctx.Reply(fmt.Sprintf("Deleting linux `%s` failed (code %d). Remove it by hand in the VM.%s", linked, code, revoked))
		}
		return
	}
	if hasLinked {
		ctx.Reply(fmt.Sprintf("Linked name `%s` looked invalid, left alone in the VM.%s", linked, revoked))
		return
	}
	if wasManager {
		ctx.Reply(fmt.Sprintf("Removed <@%d> from the bot (no linux account was linked).", uid))
	} else {
		ctx.Reply(fmt.Sprintf("<@%d> has no linked linux account.", uid))
	}
}

func cmdAdmin(b *Bot, ctx *CmdCtx, arg string, targetID string) {
	switch strings.ToLower(strings.TrimSpace(arg)) {
	case "add":
		if targetID == "" {
			ctx.Reply("Pick a user: `/admin action:add user:@user`.")
			return
		}
		if !b.IsOwner(ctx.AuthorID) {
			b.PostDenied(ctx, "Owner only.")
			return
		}
		uid := parseU64(targetID)
		b.Data.Mu.Lock()
		if uid == b.Data.Allowed.Owner {
			b.Data.Mu.Unlock()
			ctx.Reply("That user is the owner already.")
			return
		}
		for _, a := range b.Data.Allowed.Admins {
			if a == uid {
				b.Data.Mu.Unlock()
				ctx.Reply(fmt.Sprintf("<@%d> is already an admin.", uid))
				return
			}
		}
		b.Data.Allowed.Admins = append(b.Data.Allowed.Admins, uid)
		b.Data.Mu.Unlock()
		_ = config.PersistRuntime(b.Data)
		ctx.Reply(fmt.Sprintf("Added <@%d> as admin.", uid))
	case "remove":
		if targetID == "" {
			ctx.Reply("Pick a user: `/admin action:remove user:@user`.")
			return
		}
		if !b.IsOwner(ctx.AuthorID) {
			b.PostDenied(ctx, "Owner only.")
			return
		}
		uid := parseU64(targetID)
		b.Data.Mu.Lock()
		found := -1
		for i, a := range b.Data.Allowed.Admins {
			if a == uid {
				found = i
				break
			}
		}
		if found >= 0 {
			b.Data.Allowed.Admins = append(b.Data.Allowed.Admins[:found], b.Data.Allowed.Admins[found+1:]...)
			b.Data.Mu.Unlock()
			_ = config.PersistRuntime(b.Data)
			ctx.Reply(fmt.Sprintf("Removed <@%d> from admins.", uid))
		} else {
			b.Data.Mu.Unlock()
			ctx.Reply(fmt.Sprintf("<@%d> is not an admin.", uid))
		}
	default:
		if !b.IsElevated(ctx.AuthorID) {
			b.PostDenied(ctx, "Owner or admin only.")
			return
		}
		b.Data.Mu.RLock()
		admins := append([]uint64(nil), b.Data.Allowed.Admins...)
		b.Data.Mu.RUnlock()
		if len(admins) == 0 {
			ctx.Reply("No admins yet — owner runs `/admin add @user`.")
			return
		}
		msg := "Admins:"
		for _, u := range admins {
			msg += fmt.Sprintf("\n`%s`", uname(b, u))
		}
		ctx.Reply(msg)
	}
}

func cmdNotify(b *Bot, ctx *CmdCtx, arg string) {
	if !b.IsElevated(ctx.AuthorID) {
		b.PostDenied(ctx, "Owner or admin only.")
		return
	}
	t := strings.TrimSpace(arg)
	if t == "" {
		b.Data.Mu.RLock()
		ch := b.Data.Settings.NotifyChannel
		b.Data.Mu.RUnlock()
		if ch != nil {
			ctx.Reply(fmt.Sprintf("Boot messages go to <#%d>.", *ch))
		} else {
			ctx.Reply("Boot messages are OFF (no channel set).")
		}
		return
	}
	if strings.EqualFold(t, "off") {
		b.Data.Mu.Lock()
		b.Data.Settings.NotifyChannel = nil
		b.Data.Mu.Unlock()
		_ = config.PersistRuntime(b.Data)
		ctx.Reply("Boot messages OFF.")
		return
	}
	var id uint64
	if _, err := fmt.Sscanf(t, "%d", &id); err != nil || id == 0 {
		ctx.Reply("Usage: `/notify <channel-id>` or `/notify off`.")
		return
	}
	b.Data.Mu.Lock()
	b.Data.Settings.NotifyChannel = &id
	b.Data.Mu.Unlock()
	_ = config.PersistRuntime(b.Data)
	ctx.Reply(fmt.Sprintf("Boot messages will go to `<#%d>`.\n```\n%s\n```", id, BootArt))
}

func cmdWarmode(b *Bot, ctx *CmdCtx, arg string) {
	if !b.IsElevated(ctx.AuthorID) {
		b.PostDenied(ctx, "Owner or admin only.")
		return
	}
	enabled := strings.EqualFold(strings.TrimSpace(arg), "true") || strings.TrimSpace(arg) == "1" || strings.EqualFold(strings.TrimSpace(arg), "on")
	b.Data.Mu.Lock()
	b.Data.Settings.WarMode = enabled
	b.Data.Mu.Unlock()
	_ = config.PersistRuntime(b.Data)
	if enabled {
		ctx.Reply("War mode on! >:3")
		return
	}
	ctx.ReplyWithFiles("war mode disabled, peace?", []FileData{{Name: "lapeace.jpg", Data: lapeaceJPG}})
}

func cmdPurgeReplies(b *Bot, ctx *CmdCtx, target string, limit int) {
	if !b.IsElevated(ctx.AuthorID) {
		b.PostDenied(ctx, "Owner or admin only.")
		return
	}
	tid, ok := ParseTargetID(target)
	if !ok {
		ctx.Reply("Usage: `/purge_replies <user-id> [limit]`.")
		return
	}
	if limit < 1 {
		limit = 50
	}
	if limit > 100 {
		limit = 100
	}
	msgs, err := b.Session.ChannelMessages(ctx.ChannelID, limit, "", "", "")
	if err != nil {
		ctx.Reply(util.Codeblock(fmt.Sprintf("could not read channel history: %v", err)))
		return
	}
	scanned := len(msgs)
	authored := 0
	deleted := 0
	failed := 0
	for _, m := range msgs {
		if parseU64(m.Author.ID) != tid {
			continue
		}
		authored++
		repliedToMe := false
		if m.MessageReference != nil && m.MessageReference.MessageID != "" {
			if orig, err := b.Session.ChannelMessage(m.MessageReference.ChannelID, m.MessageReference.MessageID); err == nil && orig != nil {
				if orig.Author.ID == b.BotID {
					repliedToMe = true
				}
			}
		}
		// discordgo embeds referenced message differently; also check ReferencedMessage
		if m.ReferencedMessage != nil && m.ReferencedMessage.Author != nil && m.ReferencedMessage.Author.ID == b.BotID {
			repliedToMe = true
		}
		if !repliedToMe {
			continue
		}
		if err := b.Session.ChannelMessageDelete(ctx.ChannelID, m.ID); err != nil {
			failed++
			fmt.Fprintf(os.Stderr, "purge_replies: failed to delete %s: %v\n", m.ID, err)
		} else {
			deleted++
		}
	}
	ctx.Reply(fmt.Sprintf("Scanned %d recent messages, `<@%d>` authored %d, deleted %d replies to my messages, %d deletes failed.", scanned, tid, authored, deleted, failed))
}

func cmdAI(b *Bot, ctx *CmdCtx, args map[string]string) {
	// args keys: enabled, model, prompt, forget, think
	if f, ok := args["forget"]; ok && (strings.EqualFold(f, "true") || f == "1") {
		if !NeedPublic(b, ctx) {
			return
		}
		ai.ClearHistory(parseU64chan(ctx.ChannelID))
		ctx.Reply("Forgot the conversation here.")
		return
	}
	changing := false
	for _, k := range []string{"enabled", "model", "prompt", "think"} {
		if v, ok := args[k]; ok && strings.TrimSpace(v) != "" {
			changing = true
			_ = v
		}
	}
	if changing && !b.IsElevated(ctx.AuthorID) {
		b.PostDenied(ctx, "Owner or admin only.")
		return
	}
	if !NeedPublic(b, ctx) {
		return
	}
	if !changing {
		b.Data.Mu.RLock()
		model := b.Data.Settings.AIModel
		prompt := b.Data.Settings.AIPrompt
		b.Data.Mu.RUnlock()
		backstory := "none"
		if strings.TrimSpace(prompt) != "" {
			backstory = prompt
		}
		ctx.Reply(fmt.Sprintf("**model:** `%s`\n**backstory:** %s", model, backstory))
		return
	}
	modelTouched := false
	if m, ok := args["model"]; ok && strings.TrimSpace(m) != "" {
		modelTouched = true
		_ = modelTouched
	}
	var notice string
	b.Data.Mu.Lock()
	if e, ok := args["enabled"]; ok && strings.TrimSpace(e) != "" {
		b.Data.Settings.AIEnabled = strings.EqualFold(e, "true") || e == "1" || strings.EqualFold(e, "on")
	}
	if t, ok := args["think"]; ok && strings.TrimSpace(t) != "" {
		b.Data.Settings.AIThink = strings.EqualFold(t, "true") || t == "1" || strings.EqualFold(t, "on")
	}
	if m, ok := args["model"]; ok && strings.TrimSpace(m) != "" {
		mm := strings.TrimSpace(m)
		if !ai.ValidModelName(mm) {
			b.Data.Mu.Unlock()
			ctx.Reply("Bad model name — use letters, numbers and `._-:/` only (e.g. `llama3.1`, `qwen2.5-coder:7b`), max 128 chars.")
			return
		}
		b.Data.Settings.AIModel = mm
	}
	if p, ok := args["prompt"]; ok && strings.TrimSpace(p) != "" {
		pp := strings.TrimSpace(p)
		if strings.EqualFold(pp, "clear") {
			b.Data.Settings.AIPrompt = ""
		} else {
			b.Data.Settings.AIPrompt = pp
		}
	}
	thinkStr := "off (fast)"
	if b.Data.Settings.AIThink {
		thinkStr = "on (slow)"
	}
	onStr := "disabled"
	if b.Data.Settings.AIEnabled {
		onStr = "enabled"
	}
	notice = fmt.Sprintf("AI chat is **%s** — model `%s` on `%s` (think %s).", onStr, b.Data.Settings.AIModel, ai.ResolveHost(b.Data.Settings.OllamaHost), thinkStr)
	if strings.TrimSpace(b.Data.Settings.AIPrompt) == "" {
		notice += "\nBackstory: none."
	} else {
		notice += fmt.Sprintf("\nBackstory: %d chars.", len([]rune(b.Data.Settings.AIPrompt)))
	}
	host := ai.ResolveHost(b.Data.Settings.OllamaHost)
	model := b.Data.Settings.AIModel
	enabledNow := b.Data.Settings.AIEnabled
	b.Data.Mu.Unlock()
	_ = config.PersistRuntime(b.Data)
	if (enabledNow && args["enabled"] != "") || modelTouched {
		if present := ai.ModelPresent(host, model); present != nil && !*present {
			notice += fmt.Sprintf("\nWarning: `%s` isn't in `ollama list` on %s — run `ollama pull %s` there.", model, host, model)
		}
	}
	ctx.Reply(notice)
}

func parseU64chan(s string) uint64 { return parseU64(s) }

func cmdAsk(b *Bot, ctx *CmdCtx, question string) {
	if !NeedPublic(b, ctx) {
		return
	}
	b.Data.Mu.RLock()
	aiOn := b.Data.Settings.AIEnabled
	model := b.Data.Settings.AIModel
	host := ai.ResolveHost(b.Data.Settings.OllamaHost)
	prompt := b.Data.Settings.AIPrompt
	temp := b.Data.Settings.AITemperature
	think := b.Data.Settings.AIThink
	b.Data.Mu.RUnlock()
	if !aiOn {
		msg := "AI is off — the owner runs `/ai true model:<name>` to enable me."
		if ctx.IsSlash {
			ctx.ReplyEphemeral(msg)
		} else {
			ctx.Reply(msg)
		}
		return
	}
	q := strings.TrimSpace(question)
	if q == "" {
		msg := "Ask me something — `/ask <question>`."
		if ctx.IsSlash {
			ctx.ReplyEphemeral(msg)
		} else {
			ctx.Reply(msg)
		}
		return
	}
	if len([]rune(q)) > 4000 {
		q = string([]rune(q)[:4000])
	}
	speaker := ctx.AuthorName
	answer, err := ai.OllamaChat(host, model, parseU64(ctx.ChannelID), speaker, q, prompt, temp, think)
	if err != nil {
		fmt.Fprintf(os.Stderr, "ask failed (model %s on %s): %v\n", model, host, err)
		if ai.IsAPIFullErr(err.Error()) {
			answer = ai.APIFullMessage()
		} else {
			answer = ai.GlitchText(host, model, prompt, temp, think)
		}
	}
	for _, c := range ai.ChunkReply(answer) {
		ctx.Reply(c)
	}
	_ = time.Now
}

func cmdSayas(b *Bot, ctx *CmdCtx, message, replyTo string, files []FileData) {
	if !b.IsElevated(ctx.AuthorID) {
		if ctx.IsSlash {
			ctx.ReplyEphemeral("Owner or admin only.")
		} else {
			b.PostDenied(ctx, "Owner or admin only.")
		}
		return
	}
	text := strings.TrimRight(message, " \t\n\r")
	if strings.TrimSpace(text) == "" && len(files) == 0 {
		// toggle
		b.Data.Mu.Lock()
		b.Data.Settings.SayasEnabled = !b.Data.Settings.SayasEnabled
		enabled := b.Data.Settings.SayasEnabled
		b.Data.Mu.Unlock()
		_ = config.PersistRuntime(b.Data)
		msg := "Say-as-artix: **disabled**."
		if enabled {
			msg = "Say-as-artix: **enabled** — your messages will now be sent as artix (toggle again to disable)."
		}
		if ctx.IsSlash {
			ctx.ReplyEphemeral(msg)
		} else {
			ctx.Reply(msg)
		}
		return
	}
	body := text
	if len([]rune(body)) > 2000 {
		files = append([]FileData{{Name: util.AttachName(body), Data: []byte(util.CapFileBody(util.StripSGR(body)))}}, files...)
		body = ""
	}
	// delete invoking prefix message
	if !ctx.IsSlash && ctx.Msg != nil {
		_ = b.Session.ChannelMessageDelete(ctx.ChannelID, ctx.Msg.ID)
	}
	if body == "" && len(files) == 0 {
		ctx.Reply("Nothing to send — attach a file or type a message.")
		return
	}
	if replyTo != "" {
		chID, msgID, ok := ParseMessageRef(replyTo, ctx.ChannelID)
		if !ok {
			ctx.Reply("Couldn't read that reply target — give a message ID or a full message link.")
			return
		}
		target, err := b.Session.ChannelMessage(chID, msgID)
		if err != nil {
			ctx.Reply("Couldn't fetch that message (wrong channel, or I can't see it).")
			return
		}
		dfs := toDiscordFiles(files)
		_, _ = b.Session.ChannelMessageSendReply(ctx.ChannelID, body, target.Reference())
		_ = dfs
		if len(files) > 0 {
			_, _ = b.Session.ChannelMessageSendComplex(ctx.ChannelID, &discordgo.MessageSend{Content: body, Files: toDiscordFiles(files), Reference: target.Reference()})
		}
	} else {
		if body != "" && len(files) == 0 && len([]rune(body)) <= 2000 {
			_, _ = b.Session.ChannelMessageSend(ctx.ChannelID, body)
		} else if len(files) > 0 && body == "" {
			_, _ = b.Session.ChannelMessageSendComplex(ctx.ChannelID, &discordgo.MessageSend{Files: toDiscordFiles(files)})
		} else if len(files) > 0 {
			_, _ = b.Session.ChannelMessageSendComplex(ctx.ChannelID, &discordgo.MessageSend{Content: body, Files: toDiscordFiles(files)})
		} else {
			_, _ = b.Session.ChannelMessageSendComplex(ctx.ChannelID, &discordgo.MessageSend{Content: "", Files: []*discordgo.File{{Name: util.AttachName(body), Reader: bytesReader([]byte(util.CapFileBody(util.StripSGR(body))))}}})
		}
	}
	ai.RecordArtixy(parseU64(ctx.ChannelID), body)
}

func toDiscordFiles(files []FileData) []*discordgo.File {
	out := make([]*discordgo.File, 0, len(files))
	for _, f := range files {
		out = append(out, &discordgo.File{Name: f.Name, Reader: bytesReader(f.Data)})
	}
	return out
}

var _ = discordgo.PermissionManageMessages
