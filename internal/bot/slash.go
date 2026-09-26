package bot

import (
	"fmt"
	"strings"

	"github.com/bwmarrin/discordgo"
)

func SlashCommands() []*discordgo.ApplicationCommand {
	return []*discordgo.ApplicationCommand{
		{Name: "help", Description: "Show help"},
		{Name: "ps", Description: "List VMs"},
		{Name: "status", Description: "VM state + agent"},
		{Name: "start", Description: "Power on, wait for agent"},
		{Name: "stop", Description: "Graceful shutdown"},
		{Name: "restart", Description: "Reboot the VM"},
		{Name: "info", Description: "VM details + agent"},
		{Name: "run", Description: "Run a command in the VM", Options: []*discordgo.ApplicationCommandOption{
			{Type: discordgo.ApplicationCommandOptionString, Name: "cmd", Description: "Command to run", Required: true},
		}},
		{Name: "send", Description: "Send a host file from share/", Options: []*discordgo.ApplicationCommandOption{
			{Type: discordgo.ApplicationCommandOptionString, Name: "path", Description: "Absolute path under share/", Required: true},
		}},
		{Name: "upload", Description: "Upload a file into the VM", Options: []*discordgo.ApplicationCommandOption{
			{Type: discordgo.ApplicationCommandOptionAttachment, Name: "file", Description: "File to upload", Required: true},
			{Type: discordgo.ApplicationCommandOptionString, Name: "dir", Description: "Destination dir", Required: true},
		}},
		{Name: "shell", Description: "Show or set your shell", Options: []*discordgo.ApplicationCommandOption{
			{Type: discordgo.ApplicationCommandOptionString, Name: "name", Description: "fish or bash", Required: false},
		}},
		{Name: "botrestart", Description: "Restart the bot"},
		{Name: "user", Description: "Manage users", Options: []*discordgo.ApplicationCommandOption{
			{Type: discordgo.ApplicationCommandOptionString, Name: "action", Description: "add|remove|list", Required: true},
			{Type: discordgo.ApplicationCommandOptionUser, Name: "user", Description: "Target user", Required: false},
		}},
		{Name: "admin", Description: "Manage admins", Options: []*discordgo.ApplicationCommandOption{
			{Type: discordgo.ApplicationCommandOptionString, Name: "action", Description: "add|remove|list", Required: true},
			{Type: discordgo.ApplicationCommandOptionUser, Name: "user", Description: "Target user", Required: false},
		}},
		{Name: "notify", Description: "Boot message channel", Options: []*discordgo.ApplicationCommandOption{
			{Type: discordgo.ApplicationCommandOptionString, Name: "what", Description: "channel ID or off", Required: false},
		}},
		{Name: "purge_replies", Description: "Delete replies to my messages", Options: []*discordgo.ApplicationCommandOption{
			{Type: discordgo.ApplicationCommandOptionString, Name: "target", Description: "User/bot ID", Required: true},
			{Type: discordgo.ApplicationCommandOptionInteger, Name: "limit", Description: "How many to scan (max 100)", Required: false},
		}},
		{Name: "warmode", Description: "Arm or stand down protections", Options: []*discordgo.ApplicationCommandOption{
			{Type: discordgo.ApplicationCommandOptionBoolean, Name: "enabled", Description: "true/false", Required: true},
		}},
		{Name: "sayas", Description: "Post as artix", Options: []*discordgo.ApplicationCommandOption{
			{Type: discordgo.ApplicationCommandOptionString, Name: "message", Description: "Text to send", Required: false},
			{Type: discordgo.ApplicationCommandOptionString, Name: "reply_to", Description: "Message ID or link", Required: false},
			{Type: discordgo.ApplicationCommandOptionAttachment, Name: "file", Description: "File to send", Required: false},
		}},
		{Name: "ai", Description: "Configure AI chat", Options: []*discordgo.ApplicationCommandOption{
			{Type: discordgo.ApplicationCommandOptionBoolean, Name: "enabled", Description: "on/off", Required: false},
			{Type: discordgo.ApplicationCommandOptionString, Name: "model", Description: "Ollama model", Required: false},
			{Type: discordgo.ApplicationCommandOptionString, Name: "prompt", Description: "Backstory prompt", Required: false},
			{Type: discordgo.ApplicationCommandOptionBoolean, Name: "forget", Description: "Forget channel", Required: false},
			{Type: discordgo.ApplicationCommandOptionBoolean, Name: "think", Description: "Think mode", Required: false},
		}},
		{Name: "ask", Description: "Ask the AI", Options: []*discordgo.ApplicationCommandOption{
			{Type: discordgo.ApplicationCommandOptionString, Name: "question", Description: "Question", Required: true},
		}},
	}
}

func optStr(opts map[string]*discordgo.ApplicationCommandInteractionDataOption, name string) string {
	if o, ok := opts[name]; ok && o != nil {
		return o.StringValue()
	}
	return ""
}

func optBoolPtr(opts map[string]*discordgo.ApplicationCommandInteractionDataOption, name string) (string, bool) {
	if o, ok := opts[name]; ok && o != nil {
		if o.BoolValue() {
			return "true", true
		}
		return "false", true
	}
	return "", false
}

// HandleInteraction dispatches slash commands.
func (b *Bot) HandleInteraction(s *discordgo.Session, ic *discordgo.InteractionCreate) {
	if ic.Type != discordgo.InteractionApplicationCommand {
		return
	}
	// Acknowledge within Discord's 3s window first ("thinking..."); every
	// command reply below goes out as a followup. Without this, slow
	// commands (AI answers, /start, uploads) die with
	// "The application did not respond".
	if err := s.InteractionRespond(ic.Interaction, &discordgo.InteractionResponse{
		Type: discordgo.InteractionResponseDeferredChannelMessageWithSource,
	}); err != nil {
		return
	}
	defer func() {
		if r := recover(); r != nil {
			_, _ = s.FollowupMessageCreate(ic.Interaction, false, &discordgo.WebhookParams{
				Content: "Something broke on my side — check the bot log.",
			})
		}
	}()
	data := ic.ApplicationCommandData()
	opts := map[string]*discordgo.ApplicationCommandInteractionDataOption{}
	for _, o := range data.Options {
		opts[o.Name] = o
	}
	authorID := ""
	authorName := ""
	if ic.Member != nil && ic.Member.User != nil {
		authorID = ic.Member.User.ID
		authorName = ic.Member.User.Username
	} else if ic.User != nil {
		authorID = ic.User.ID
		authorName = ic.User.Username
	}
	ctx := &CmdCtx{
		Bot:         b,
		IsSlash:     true,
		Interaction: ic.Interaction,
		ChannelID:   ic.ChannelID,
		GuildID:     ic.GuildID,
		AuthorID:    authorID,
		AuthorName:  authorName,
		Options:     opts,
	}
	switch data.Name {
	case "help":
		cmdHelp(b, ctx, "")
	case "ps":
		cmdPs(b, ctx, "")
	case "status":
		cmdStatus(b, ctx, "")
	case "start":
		cmdStart(b, ctx, "")
	case "stop":
		cmdStop(b, ctx, "")
	case "restart":
		cmdRestart(b, ctx, "")
	case "info":
		cmdInfo(b, ctx, "")
	case "run":
		cmdRun(b, ctx, optStr(opts, "cmd"))
	case "send":
		cmdSend(b, ctx, optStr(opts, "path"))
	case "upload":
		dir := optStr(opts, "dir")
		// resolve attachment
		var url, name string
		var size int64
		if o, ok := opts["file"]; ok && o != nil {
			attID := o.Value
			_ = attID
			// discordgo resolves attachments in data.Resolved
			if data.Resolved != nil {
				for id, att := range data.Resolved.Attachments {
					_ = id
					url = att.URL
					name = att.Filename
					size = int64(att.Size)
					break
				}
			}
		}
		cmdUpload(b, ctx, dir, url, name, size)
	case "shell":
		cmdShell(b, ctx, optStr(opts, "name"))
	case "botrestart":
		cmdBotrestart(b, ctx, "")
	case "user":
		action := optStr(opts, "action")
		tid, tname := "", ""
		if o, ok := opts["user"]; ok && o != nil {
			if u := o.UserValue(b.Session); u != nil {
				tid, tname = u.ID, u.Username
			}
		}
		cmdUser(b, ctx, action, tid, tname)
	case "admin":
		action := optStr(opts, "action")
		tid := ""
		if o, ok := opts["user"]; ok && o != nil {
			if u := o.UserValue(b.Session); u != nil {
				tid = u.ID
			}
		}
		cmdAdmin(b, ctx, action, tid)
	case "notify":
		cmdNotify(b, ctx, optStr(opts, "what"))
	case "purge_replies":
		limit := 50
		if o, ok := opts["limit"]; ok && o != nil {
			limit = int(o.IntValue())
		}
		cmdPurgeReplies(b, ctx, optStr(opts, "target"), limit)
	case "warmode":
		v := "false"
		if o, ok := opts["enabled"]; ok && o != nil && o.BoolValue() {
			v = "true"
		}
		cmdWarmode(b, ctx, v)
	case "sayas":
		msg := optStr(opts, "message")
		replyTo := optStr(opts, "reply_to")
		var files []FileData
		if o, ok := opts["file"]; ok && o != nil && data.Resolved != nil {
			for _, att := range data.Resolved.Attachments {
				d, err := DownloadAttachment(att.URL, 25*1024*1024+1)
				if err == nil && int64(len(d)) <= 25*1024*1024 {
					files = append(files, FileData{SafeAttachName(att.Filename), d})
				}
				break
			}
		}
		cmdSayas(b, ctx, msg, replyTo, files)
	case "ai":
		args := map[string]string{}
		if v, ok := optBoolPtr(opts, "enabled"); ok {
			args["enabled"] = v
		}
		if v := optStr(opts, "model"); strings.TrimSpace(v) != "" {
			args["model"] = v
		}
		if v := optStr(opts, "prompt"); strings.TrimSpace(v) != "" {
			args["prompt"] = v
		}
		if v, ok := optBoolPtr(opts, "forget"); ok {
			args["forget"] = v
		}
		if v, ok := optBoolPtr(opts, "think"); ok {
			args["think"] = v
		}
		cmdAI(b, ctx, args)
	case "ask":
		cmdAsk(b, ctx, optStr(opts, "question"))
	default:
		_ = fmt.Sprintf("")
	}
}
