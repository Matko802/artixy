package bot

import (
	"fmt"
	"os"
	"strings"

	"github.com/Matko802/artixy/internal/ai"
	"github.com/Matko802/artixy/internal/config"
	"github.com/Matko802/artixy/internal/live"
	"github.com/Matko802/artixy/internal/util"
	"github.com/Matko802/artixy/internal/vm"
	"github.com/bwmarrin/discordgo"
)

func isBooMessage(s string) bool {
	for _, w := range strings.FieldsFunc(strings.ToLower(s), func(c rune) bool {
		return !(c >= 'a' && c <= 'z' || c >= '0' && c <= '9')
	}) {
		if w == "boo" {
			return true
		}
	}
	return false
}

const taunt = "purged your message haha"

func artixyText(s string) (string, bool) {
	t := strings.TrimRight(s, " \t\n\r")
	if !strings.HasSuffix(t, ".ar") {
		return "", false
	}
	text := strings.TrimSpace(strings.TrimSuffix(t, ".ar"))
	if text == "" {
		return "", false
	}
	return text, true
}

// HandleMessage is the prefix + ambient event handler.
func (b *Bot) HandleMessage(s *discordgo.Session, m *discordgo.MessageCreate) {
	msg := m.Message
	if msg.WebhookID != "" {
		return
	}
	if msg.Author == nil || msg.Author.ID == b.BotID {
		return
	}
	id := msg.Author.ID

	// Snapshot access state.
	b.Data.Mu.RLock()
	isBlocked := false
	for _, x := range b.Data.Allowed.Blocked {
		if fmt.Sprintf("%d", x) == id {
			isBlocked = true
			break
		}
	}
	isOwnerOrAdmin := false
	ownerStr := fmt.Sprintf("%d", b.Data.Allowed.Owner)
	if id == ownerStr {
		isOwnerOrAdmin = true
	} else {
		for _, a := range b.Data.Allowed.Admins {
			if fmt.Sprintf("%d", a) == id {
				isOwnerOrAdmin = true
				break
			}
		}
	}
	war := b.Data.Settings.WarMode
	sayasAuto := b.Data.Settings.SayasEnabled
	b.Data.Mu.RUnlock()

	// War-mode guard for blocked users replying to the bot.
	if isBlocked && war {
		if ref := referencedMsg(s, msg); ref != nil && ref.Author != nil && ref.Author.ID == b.BotID {
			_ = s.ChannelMessageDelete(msg.ChannelID, msg.ID)
			if ref.Content != taunt {
				_, _ = s.ChannelMessageSend(msg.ChannelID, taunt)
			}
		}
		return
	}

	// Live-session input: reply to a live message with plain text.
	if len(msg.Attachments) == 0 && strings.TrimSpace(msg.Content) != "" {
		if refID := referencedID(msg); refID != "" {
			if e := b.Live.FindByMsg(msg.ChannelID, refID); e != nil {
				if e.AuthorID != id {
					_ = s.ChannelMessageDelete(msg.ChannelID, msg.ID)
					if ch, err := s.UserChannelCreate(id); err == nil {
						_, _ = s.ChannelMessageSend(ch.ID, "That live session belongs to someone else — typing into it is blocked.")
					}
					return
				}
				if e.InF == nil {
					_ = s.ChannelMessageDelete(msg.ChannelID, msg.ID)
					if ch, err := s.UserChannelCreate(id); err == nil {
						_, _ = s.ChannelMessageSend(ch.ID, "That live session isn't interactive (fifo missing).")
					}
					return
				}
				if !b.IsAuthed(id) {
					return
				}
				trimmed := strings.TrimRight(msg.Content, " \t\n\r")
				if trimmed == "" {
					return
				}
				payload := live.ExpandTypedInput(trimmed)
				runas := vm.LinkedUser(b.Data, parseU64(id))
				if !util.ValidRunas(runas) {
					runas = ""
				}
				b.Data.Mu.RLock()
				v := b.Data.VM
				b.Data.Mu.RUnlock()
				ok := live.ForwardTerminalInput(v, *e.InF, runas, payload)
				_ = s.ChannelMessageDelete(msg.ChannelID, msg.ID)
				if !ok {
					if ch, err := s.UserChannelCreate(id); err == nil {
						_, _ = s.ChannelMessageSend(ch.ID, "Couldn't type into that session (it just ended).")
					}
				}
				return
			}
		}
	}

	// Prefix commands (/cmd and ;cmd).
	if cmd, arg, ok := parsePrefix(msg.Content); ok {
		b.dispatchPrefix(msg, cmd, arg)
		return
	}

	// AI mention handling.
	if mentionsBot(msg, b.BotID) {
		prompt0 := ai.StripName(ai.StripMention(msg.Content, parseU64(b.BotID)))
		if strings.HasPrefix(prompt0, "/") || strings.HasPrefix(prompt0, ";") {
			return
		}
		if isBlocked {
			return
		}
		b.Data.Mu.RLock()
		aiOn := b.Data.Settings.AIEnabled
		model := b.Data.Settings.AIModel
		host := ai.ResolveHost(b.Data.Settings.OllamaHost)
		sysPrompt := b.Data.Settings.AIPrompt
		temp := b.Data.Settings.AITemperature
		think := b.Data.Settings.AIThink
		b.Data.Mu.RUnlock()
		if !aiOn {
			_, _ = s.ChannelMessageSendReply(msg.ChannelID, "AI is off — the owner runs `/ai true model:<name>` to enable me.", msg.Reference())
			return
		}
		prompt := prompt0
		speaker := DisplayName(msg)
		// replied message context
		if ref := referencedMsg(s, msg); ref != nil {
			qtext := strings.TrimSpace(ref.Content)
			if qtext == "" && len(ref.Attachments) > 0 {
				var names []string
				for _, a := range ref.Attachments {
					names = append(names, a.Filename)
				}
				qtext = "[attachment(s): " + strings.Join(names, ", ") + "]"
			}
			if qtext != "" {
				if len([]rune(qtext)) > 1500 {
					qtext = string([]rune(qtext)[:1500])
				}
				qdisplay := ref.Author.Username
				if ref.Author.GlobalName != "" {
					qdisplay = ref.Author.GlobalName
				}
				if strings.TrimSpace(prompt) == "" {
					prompt = fmt.Sprintf("(quoting %s): %s", ai.SpeakerTag(qdisplay), qtext)
				} else {
					prompt = fmt.Sprintf("%s\n(replying to %s): %s", prompt, ai.SpeakerTag(qdisplay), qtext)
				}
			}
		}
		// recent channel context
		if recent, err := s.ChannelMessages(msg.ChannelID, 15, "", "", ""); err == nil {
			// sort oldest first
			for i, j := 0, len(recent)-1; i < j; i, j = i+1, j-1 {
				recent[i], recent[j] = recent[j], recent[i]
			}
			var lines []string
			total := 0
			for _, rm := range recent {
				if rm.ID == msg.ID {
					continue
				}
				who := rm.Author.Username
				if rm.Author.GlobalName != "" {
					who = rm.Author.GlobalName
				}
				body := strings.TrimSpace(rm.Content)
				if body == "" && len(rm.Attachments) > 0 {
					var names []string
					for _, a := range rm.Attachments {
						names = append(names, a.Filename)
					}
					body = "[attachment(s): " + strings.Join(names, ", ") + "]"
				}
				if body == "" {
					continue
				}
				if len([]rune(body)) > 300 {
					body = string([]rune(body)[:300])
				}
				line := fmt.Sprintf("- %s: %s", ai.SpeakerTag(who), body)
				total += len(line)
				if total > 2500 {
					break
				}
				lines = append(lines, line)
			}
			if len(lines) > 0 {
				prompt = fmt.Sprintf("%s\n\n[recent messages in channel]:\n%s", prompt, strings.Join(lines, "\n"))
			}
		}
		if strings.TrimSpace(prompt) == "" {
			_, _ = s.ChannelMessageSendReply(msg.ChannelID, fmt.Sprintf("Ping me with a question — `@artixy <question>` or `artixy <question>` (model `%s`).", model), msg.Reference())
			return
		}
		if len([]rune(prompt)) > 7000 {
			prompt = string([]rune(prompt)[:7000])
		}
		_ = s.ChannelTyping(msg.ChannelID)
		text, err := ai.OllamaChat(host, model, parseU64(msg.ChannelID), speaker, prompt, sysPrompt, temp, think)
		if err != nil {
			fmt.Fprintf(os.Stderr, "ai chat failed (model %s on %s): %v\n", model, host, err)
			if ai.IsAPIFullErr(err.Error()) {
				text = ai.APIFullMessage()
			} else {
				text = ai.GlitchText(host, model, sysPrompt, temp, think)
			}
		}
		chunks := ai.ChunkReply(text)
		for i, c := range chunks {
			if i == 0 {
				_, _ = s.ChannelMessageSendReply(msg.ChannelID, c, msg.Reference())
			} else {
				if _, err := s.ChannelMessageSend(msg.ChannelID, c); err != nil {
					break
				}
			}
		}
		return
	}

	if !isOwnerOrAdmin {
		if war && isBooMessage(msg.Content) {
			_, _ = s.ChannelMessageSendReply(msg.ChannelID, "boo on you! :3", msg.Reference())
		}
		return
	}

	// Sayas auto mode.
	if sayasAuto {
		content := strings.TrimSpace(msg.Content)
		hasFiles := len(msg.Attachments) > 0
		plainText := content != "" && !strings.HasPrefix(content, "/") && !strings.HasPrefix(content, ";") && !strings.HasSuffix(content, ".ar")
		if plainText || (hasFiles && content == "") {
			var files []FileData
			for _, a := range msg.Attachments {
				d, err := DownloadAttachment(a.URL, 25*1024*1024+1)
				if err == nil && int64(len(d)) <= 25*1024*1024 {
					files = append(files, FileData{SafeAttachName(a.Filename), d})
				}
			}
			body := content
			if len([]rune(body)) > 2000 {
				files = append([]FileData{{util.AttachName(body), []byte(util.CapFileBody(util.StripSGR(body)))}}, files...)
				body = ""
			}
			if body == "" && len(files) == 0 {
				return
			}
			_ = s.ChannelMessageDelete(msg.ChannelID, msg.ID)
			ai.RecordArtixy(parseU64(msg.ChannelID), body)
			if body != "" && len(files) == 0 && len([]rune(body)) <= 2000 {
				if ref := referencedMsg(s, msg); ref != nil {
					_, _ = s.ChannelMessageSendReply(msg.ChannelID, body, ref.Reference())
				} else {
					_, _ = s.ChannelMessageSend(msg.ChannelID, body)
				}
			} else {
				if ref := referencedMsg(s, msg); ref != nil {
					_, _ = s.ChannelMessageSendComplex(msg.ChannelID, &discordgo.MessageSend{Content: body, Files: toDiscordFiles(files), Reference: ref.Reference()})
				} else {
					_, _ = s.ChannelMessageSendComplex(msg.ChannelID, &discordgo.MessageSend{Content: body, Files: toDiscordFiles(files)})
				}
			}
			return
		}
	}

	// .ar suffix.
	if text, ok := artixyText(msg.Content); ok {
		for i := 0; i < 3; i++ {
			if err := s.ChannelMessageDelete(msg.ChannelID, msg.ID); err == nil {
				break
			}
			fmt.Fprintf(os.Stderr, "artixy-say: delete attempt failed for %s\n", msg.ID)
		}
		var files []FileData
		for _, a := range msg.Attachments {
			d, err := DownloadAttachment(a.URL, 25*1024*1024+1)
			if err == nil && int64(len(d)) <= 25*1024*1024 {
				files = append(files, FileData{SafeAttachName(a.Filename), d})
			}
		}
		body := text
		if len([]rune(body)) > 2000 {
			files = append([]FileData{{util.AttachName(body), []byte(util.CapFileBody(util.StripSGR(body)))}}, files...)
			body = ""
		}
		ai.RecordArtixy(parseU64(msg.ChannelID), body)
		if body != "" && len(files) == 0 {
			if ref := referencedMsg(s, msg); ref != nil {
				_, _ = s.ChannelMessageSendReply(msg.ChannelID, body, ref.Reference())
			} else {
				_, _ = s.ChannelMessageSend(msg.ChannelID, body)
			}
		} else if body == "" && len(files) == 0 {
		} else {
			if ref := referencedMsg(s, msg); ref != nil {
				_, _ = s.ChannelMessageSendComplex(msg.ChannelID, &discordgo.MessageSend{Content: body, Files: toDiscordFiles(files), Reference: ref.Reference()})
			} else {
				_, _ = s.ChannelMessageSendComplex(msg.ChannelID, &discordgo.MessageSend{Content: body, Files: toDiscordFiles(files)})
			}
		}
	}
}

func referencedID(msg *discordgo.Message) string {
	if msg.MessageReference != nil && msg.MessageReference.MessageID != "" {
		return msg.MessageReference.MessageID
	}
	return ""
}

func referencedMsg(s *discordgo.Session, msg *discordgo.Message) *discordgo.Message {
	if msg.ReferencedMessage != nil {
		return msg.ReferencedMessage
	}
	if msg.MessageReference != nil && msg.MessageReference.MessageID != "" {
		if m, err := s.ChannelMessage(msg.MessageReference.ChannelID, msg.MessageReference.MessageID); err == nil {
			return m
		}
	}
	return nil
}

func mentionsBot(msg *discordgo.Message, botID string) bool {
	for _, u := range msg.Mentions {
		if u.ID == botID {
			return true
		}
	}
	if strings.Contains(msg.Content, "<@"+botID+">") || strings.Contains(msg.Content, "<@!"+botID+">") {
		return true
	}
	return ai.MentionsName(msg.Content)
}

func parsePrefix(content string) (string, string, bool) {
	t := strings.TrimSpace(content)
	if t == "" {
		return "", "", false
	}
	var rest string
	if strings.HasPrefix(t, "/") {
		rest = strings.TrimSpace(t[1:])
	} else if strings.HasPrefix(t, ";") {
		rest = strings.TrimSpace(t[1:])
	} else {
		return "", "", false
	}
	if rest == "" {
		return "", "", false
	}
	parts := strings.SplitN(rest, " ", 2)
	cmd := strings.ToLower(strings.TrimSpace(parts[0]))
	arg := ""
	if len(parts) > 1 {
		arg = strings.TrimSpace(parts[1])
	}
	// strip trailing bot mention noise? no
	switch cmd {
	case "help", "ps", "status", "start", "stop", "restart", "info", "run", "send", "upload",
		"shell", "botrestart", "user", "admin", "notify", "purge_replies", "purgereplies",
		"warmode", "sayas", "ai", "ask":
		if cmd == "purgereplies" {
			cmd = "purge_replies"
		}
		return cmd, arg, true
	}
	return "", "", false
}

func (b *Bot) dispatchPrefix(msg *discordgo.Message, cmd, arg string) {
	ctx := &CmdCtx{
		Bot:        b,
		IsSlash:    false,
		Msg:        msg,
		ChannelID:  msg.ChannelID,
		GuildID:    msg.GuildID,
		AuthorID:   msg.Author.ID,
		AuthorName: msg.Author.Username,
	}
	if msg.Member != nil && msg.Member.Nick != "" {
		ctx.Nick = msg.Member.Nick
	}
	for _, a := range msg.Attachments {
		ctx.Attachments = append(ctx.Attachments, &discordgo.MessageAttachment{
			ID: a.ID, Filename: a.Filename, Size: a.Size, URL: a.URL,
		})
	}
	switch cmd {
	case "help":
		cmdHelp(b, ctx, arg)
	case "ps":
		cmdPs(b, ctx, arg)
	case "status":
		cmdStatus(b, ctx, arg)
	case "start":
		cmdStart(b, ctx, arg)
	case "stop":
		cmdStop(b, ctx, arg)
	case "restart":
		cmdRestart(b, ctx, arg)
	case "info":
		cmdInfo(b, ctx, arg)
	case "run":
		cmdRun(b, ctx, arg)
	case "send":
		cmdSend(b, ctx, arg)
	case "upload":
		// /upload <dir> with first attachment
		if len(msg.Attachments) == 0 {
			ctx.Reply("Attach a file: `/upload <file> <dir>`.")
			return
		}
		a := msg.Attachments[0]
		cmdUpload(b, ctx, arg, a.URL, a.Filename, int64(a.Size))
	case "shell":
		cmdShell(b, ctx, arg)
	case "botrestart":
		cmdBotrestart(b, ctx, arg)
	case "user":
		action, tid, tname := parseUserArgs(msg, arg)
		cmdUser(b, ctx, action, tid, tname)
	case "admin":
		action, tid, _ := parseUserArgs(msg, arg)
		cmdAdmin(b, ctx, action, tid)
	case "notify":
		cmdNotify(b, ctx, arg)
	case "purge_replies":
		fields := strings.Fields(arg)
		target := ""
		limit := 50
		if len(fields) > 0 {
			target = fields[0]
		}
		if len(fields) > 1 {
			fmt.Sscanf(fields[1], "%d", &limit)
		}
		cmdPurgeReplies(b, ctx, target, limit)
	case "warmode":
		cmdWarmode(b, ctx, arg)
	case "sayas":
		// /sayas [message] — attachments from msg
		var files []FileData
		for _, a := range msg.Attachments {
			d, err := DownloadAttachment(a.URL, 25*1024*1024+1)
			if err == nil && int64(len(d)) <= 25*1024*1024 {
				files = append(files, FileData{SafeAttachName(a.Filename), d})
			}
		}
		// reply_to parsing: last token could be reply target? Rust slash has reply_to option;
		// prefix version: treat whole arg as message (reply via Discord reply feature instead).
		cmdSayas(b, ctx, arg, "", files)
	case "ai":
		cmdAI(b, ctx, parseAIArgs(arg))
	case "ask":
		cmdAsk(b, ctx, arg)
	}
}

func parseUserArgs(msg *discordgo.Message, arg string) (action, tid, tname string) {
	fields := strings.Fields(arg)
	if len(fields) == 0 {
		return "list", "", ""
	}
	action = strings.ToLower(fields[0])
	if action != "add" && action != "remove" && action != "list" {
		return "list", "", ""
	}
	if len(msg.Mentions) > 0 {
		tid = msg.Mentions[0].ID
		tname = msg.Mentions[0].Username
		return action, tid, tname
	}
	if len(fields) > 1 {
		if id, ok := ParseTargetID(fields[1]); ok {
			tid = fmt.Sprintf("%d", id)
			tname = fields[1]
		}
	}
	return action, tid, tname
}

func parseAIArgs(arg string) map[string]string {
	out := map[string]string{}
	t := strings.TrimSpace(arg)
	if t == "" {
		return out
	}
	// forget:true shortcut
	low := strings.ToLower(t)
	if strings.Contains(low, "forget:true") || strings.Contains(low, "forget: true") {
		out["forget"] = "true"
		return out
	}
	for _, tok := range strings.Fields(t) {
		if strings.Contains(tok, ":") {
			kv := strings.SplitN(tok, ":", 2)
			k := strings.ToLower(strings.TrimSpace(kv[0]))
			v := strings.TrimSpace(kv[1])
			switch k {
			case "model", "prompt", "think", "forget", "enabled", "true", "false":
				out[k] = v
			}
		} else if strings.EqualFold(tok, "true") || strings.EqualFold(tok, "false") || strings.EqualFold(tok, "on") || strings.EqualFold(tok, "off") {
			out["enabled"] = tok
		} else if strings.EqualFold(tok, "clear") {
			out["prompt"] = "clear"
		}
	}
	// prompt with spaces: "prompt:xxx yyy" — capture remainder after "prompt:"
	if idx := strings.Index(low, "prompt:"); idx >= 0 {
		v := strings.TrimSpace(t[idx+len("prompt:"):])
		if v != "" {
			// stop at known flags
			for _, flag := range []string{" think:", " model:", " forget:"} {
				if i := strings.Index(strings.ToLower(v), flag); i >= 0 {
					v = strings.TrimSpace(v[:i])
					break
				}
			}
			out["prompt"] = v
		}
	}
	_ = config.ConfigFilePath
	return out
}
