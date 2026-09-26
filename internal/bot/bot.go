package bot

import (
	"fmt"
	"io"
	"net/http"
	"os"
	"sort"
	"strings"

	"github.com/Matko802/artixy/internal/ai"
	"github.com/Matko802/artixy/internal/config"
	"github.com/Matko802/artixy/internal/live"
	"github.com/Matko802/artixy/internal/util"
	"github.com/bwmarrin/discordgo"
)

const BootArt = `          .        :-------:
        ^/ \^      :Im here:
        ●   ●     <:-------:
       /  ω  \
      /_/   \_\`

const HelpText = `artixy — type commands as a plain message or as slash. One VM, no names needed.

VM (owner + added users)
/ps — list VMs
/status — state + agent
/start — power on, wait for agent
/stop — graceful shutdown
/restart — reboot
/info — details + agent
/run <cmd> — run in the VM as your linked user (e.g. /run ls -la)
  reply to its live message to type into it (;return ;space ;enter ;esc ;up ;down ;left ;right, add a number to repeat)
/send </abs/path> — send a host file from share/ (~20MB max, no secrets)
/upload <file> <dir> — attach a file into /tmp/artixy-uploads/ or your /home/<you>/

accounts (owner/admin)
/user add @u | remove @u | list — linux account + bot access
/admin add @u | remove @u | list — bot admins (add/remove owner only)
/shell fish|bash — your shell
/notify <channel-id> | off — boot message channel
/purge_replies <user-id> [limit] — delete their replies to my messages
/warmode true|false — arm or stand down protections

as artix (owner/admin)
/sayas [message] — post as artix, no args toggles auto mode
<text>.ar — post that line as artix

ai
@artixy <question> — chat (needs /ai true)
/ask <question> — ask the AI, answers you directly
/ai true|false — on/off
/ai model:<name> — e.g. model:llama3.1
/ai prompt:<text> | prompt:clear — backstory (long text goes in ai_prompt in config)
/ai think:true|false — model reasoning, off is fast (default)
/ai forget:true — forget this channel

Warning: managers can power the machine on/off. Keep the token secret: config.jsonc only, never git.`

type Bot struct {
	Session *discordgo.Session
	Data    *config.Data
	Live    *live.Map
	BotID   string
}

func NewBot(session *discordgo.Session, data *config.Data, lm *live.Map) *Bot {
	return &Bot{Session: session, Data: data, Live: lm}
}

func (b *Bot) IsBlocked(uid string) bool {
	id := parseU64(uid)
	b.Data.Mu.RLock()
	defer b.Data.Mu.RUnlock()
	for _, x := range b.Data.Allowed.Blocked {
		if x == id {
			return true
		}
	}
	return false
}

func (b *Bot) IsAuthed(uid string) bool {
	id := parseU64(uid)
	b.Data.Mu.RLock()
	defer b.Data.Mu.RUnlock()
	return config.AccessAllowed(b.Data.Allowed.Owner, b.Data.Allowed.Users, b.Data.Allowed.Blocked, id)
}

func (b *Bot) IsOwner(uid string) bool {
	id := parseU64(uid)
	b.Data.Mu.RLock()
	defer b.Data.Mu.RUnlock()
	for _, x := range b.Data.Allowed.Blocked {
		if x == id {
			return false
		}
	}
	return id == b.Data.Allowed.Owner
}

func (b *Bot) IsElevated(uid string) bool {
	id := parseU64(uid)
	b.Data.Mu.RLock()
	defer b.Data.Mu.RUnlock()
	return config.ElevatedAllowed(b.Data.Allowed.Owner, b.Data.Allowed.Admins, b.Data.Allowed.Blocked, id)
}

func (b *Bot) RequireVM(channelID string) string {
	b.Data.Mu.RLock()
	defer b.Data.Mu.RUnlock()
	if strings.TrimSpace(b.Data.VM) == "" {
		b.Session.ChannelMessageSend(channelID, fmt.Sprintf("VM not configured — set `vm_name` in `%s` or `VM_NAME` env (config edits hot-apply, no restart needed).", config.ConfigFilePath()))
		return ""
	}
	return b.Data.VM
}

// CmdCtx unifies prefix + slash invocation.
type CmdCtx struct {
	Bot         *Bot
	IsSlash     bool
	Interaction *discordgo.Interaction
	Msg         *discordgo.Message
	ChannelID   string
	GuildID     string
	AuthorID    string
	AuthorName  string
	Nick        string
	Options     map[string]*discordgo.ApplicationCommandInteractionDataOption
	Attachments []*discordgo.MessageAttachment // prefix msg attachments
}

func (c *CmdCtx) Reply(text string) {
	if c.IsSlash {
		// HandleInteraction already deferred the response, so every reply
		// is a followup — this keeps slow commands (AI, /start, uploads)
		// inside Discord's 3s interaction window.
		_, _ = c.Bot.Session.FollowupMessageCreate(c.Interaction, false, &discordgo.WebhookParams{
			Content: truncate(text, 2000),
		})
		return
	}
	_, _ = c.Bot.Session.ChannelMessageSend(c.ChannelID, truncate(text, 2000))
}

func (c *CmdCtx) ReplyEphemeral(text string) {
	if c.IsSlash {
		_, _ = c.Bot.Session.FollowupMessageCreate(c.Interaction, false, &discordgo.WebhookParams{
			Content: truncate(text, 2000),
			Flags:   discordgo.MessageFlagsEphemeral,
		})
		return
	}
	// prefix fallback: DM, else channel
	ch, err := c.Bot.Session.UserChannelCreate(c.AuthorID)
	if err == nil {
		_, _ = c.Bot.Session.ChannelMessageSend(ch.ID, truncate(text, 2000))
		return
	}
	_, _ = c.Bot.Session.ChannelMessageSend(c.ChannelID, truncate(text, 2000))
}

func (c *CmdCtx) ReplyWithFiles(text string, files []FileData) {
	if c.IsSlash {
		// Already deferred by HandleInteraction — reply via followup.
		dfs := make([]*discordgo.File, 0, len(files))
		for _, f := range files {
			dfs = append(dfs, &discordgo.File{Name: f.Name, Reader: bytesReader(f.Data)})
		}
		_, _ = c.Bot.Session.FollowupMessageCreate(c.Interaction, false, &discordgo.WebhookParams{
			Content: text,
			Files:   dfs,
		})
		return
	}
	dfs := make([]*discordgo.File, 0, len(files))
	for _, f := range files {
		dfs = append(dfs, &discordgo.File{Name: f.Name, Reader: bytesReader(f.Data)})
	}
	_, _ = c.Bot.Session.ChannelMessageSendComplex(c.ChannelID, &discordgo.MessageSend{
		Content: text,
		Files:   dfs,
	})
}

func truncate(s string, n int) string {
	r := []rune(s)
	if len(r) <= n {
		return s
	}
	return string(r[:n])
}

type FileData struct {
	Name string
	Data []byte
}

func DownloadAttachment(url string, maxBytes int64) ([]byte, error) {
	resp, err := http.Get(url)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	return io.ReadAll(io.LimitReader(resp.Body, maxBytes+1))
}

func (b *Bot) PostText(channelID, content string) *discordgo.Message {
	m, _ := b.Session.ChannelMessageSend(channelID, content)
	return m
}

func (b *Bot) PostDenied(ctx *CmdCtx, content string) {
	mention := fmt.Sprintf("<@%s> %s", ctx.AuthorID, content)
	if ctx.IsSlash {
		_, _ = b.Session.FollowupMessageCreate(ctx.Interaction, false, &discordgo.WebhookParams{
			Content: mention,
			Flags:   discordgo.MessageFlagsEphemeral,
		})
		return
	}
	_, _ = b.Session.ChannelMessageSend(ctx.ChannelID, mention)
}

func NeedAuth(b *Bot, ctx *CmdCtx) bool {
	if b.IsAuthed(ctx.AuthorID) {
		return true
	}
	fmt.Fprintf(os.Stderr, "denied: %s (id %s)\n", ctx.AuthorName, ctx.AuthorID)
	b.PostDenied(ctx, "Not authorized. Ask the owner to run `/user add @you`.")
	return false
}

func NeedPublic(b *Bot, ctx *CmdCtx) bool {
	if !b.IsBlocked(ctx.AuthorID) {
		return true
	}
	fmt.Fprintf(os.Stderr, "denied (blocked): %s (id %s)\n", ctx.AuthorName, ctx.AuthorID)
	b.PostDenied(ctx, "Not allowed.")
	return false
}

func parseU64(s string) uint64 {
	var v uint64
	fmt.Sscanf(strings.TrimSpace(s), "%d", &v)
	return v
}

func ParseTargetID(s string) (uint64, bool) {
	t := strings.TrimSpace(s)
	if strings.HasPrefix(t, "<@") && strings.HasSuffix(t, ">") {
		inner := t[2 : len(t)-1]
		inner = strings.TrimPrefix(inner, "!")
		t = inner
	}
	var v uint64
	if _, err := fmt.Sscanf(t, "%d", &v); err != nil || v == 0 {
		return 0, false
	}
	return v, true
}

func ParseMessageRef(s, currentChannel string) (string, string, bool) {
	t := strings.Trim(strings.Trim(s, "<>"), " \t\n")
	if idx := strings.Index(t, "?"); idx >= 0 {
		t = strings.TrimSpace(t[:idx])
	}
	if idx := strings.Index(t, "/channels/"); idx >= 0 {
		rest := t[idx+len("/channels/"):]
		parts := strings.Split(rest, "/")
		if len(parts) != 3 {
			return "", "", false
		}
		if parts[1] == "0" || parts[2] == "0" {
			return "", "", false
		}
		return parts[1], parts[2], true
	}
	t = strings.TrimSpace(t)
	if t == "" || t == "0" {
		return "", "", false
	}
	for _, c := range t {
		if c < '0' || c > '9' {
			return "", "", false
		}
	}
	return currentChannel, t, true
}

func IsSensitiveSendName(name string) bool {
	n := strings.ToLower(strings.TrimSpace(name))
	if n == "" || strings.HasPrefix(n, ".") {
		return true
	}
	if n == "config.jsonc" || n == ".env" || n == "token" {
		return true
	}
	if strings.Contains(n, ".env") || strings.Contains(n, "config.jsonc") {
		return true
	}
	for _, suf := range []string{".pem", ".key", ".p12", ".pfx", ".token"} {
		if strings.HasSuffix(n, suf) {
			return true
		}
	}
	for _, pre := range []string{"id_rsa", "id_ed25519", "id_ecdsa", "id_dsa"} {
		if strings.HasPrefix(n, pre) {
			return true
		}
	}
	for _, inf := range []string{"secret", "credential", "private_key", "token", "webhook"} {
		if strings.Contains(n, inf) {
			return true
		}
	}
	return false
}

func ShareRelHasDotComponent(rel string) bool {
	for _, part := range strings.Split(rel, "/") {
		if strings.HasPrefix(part, ".") {
			return true
		}
	}
	return false
}

func NormalizeGuestDir(dir string) (string, bool) {
	d := strings.TrimSpace(dir)
	if !strings.HasPrefix(d, "/") || strings.Contains(d, "\x00") || len(d) > 512 {
		return "", false
	}
	var stack []string
	for _, part := range strings.Split(d, "/") {
		switch part {
		case "", ".":
		case "..":
			if len(stack) == 0 {
				return "", false
			}
			stack = stack[:len(stack)-1]
		default:
			stack = append(stack, part)
		}
	}
	if len(stack) == 0 {
		return "/", true
	}
	for _, p := range stack {
		if len(p) > 128 {
			return "", false
		}
	}
	return "/" + strings.Join(stack, "/"), true
}

func AllowedUploadDir(normalized, linked string) bool {
	if normalized == "/tmp/artixy-uploads" || strings.HasPrefix(normalized, "/tmp/artixy-uploads/") {
		return true
	}
	if linked != "" && util.ValidRunas(linked) {
		home := "/home/" + linked
		if normalized == home || strings.HasPrefix(normalized, home+"/") {
			return true
		}
	}
	return false
}

func SafeAttachName(raw string) string {
	base := raw
	if idx := strings.LastIndex(base, "/"); idx >= 0 {
		base = base[idx+1:]
	}
	var sb strings.Builder
	for _, c := range base {
		if (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '.' || c == '-' || c == '_' {
			sb.WriteRune(c)
		}
	}
	clean := strings.Trim(sb.String(), ".")
	if clean == "" {
		return "file.bin"
	}
	r := []rune(clean)
	if len(r) > 100 {
		return string(r[:100])
	}
	return clean
}

func DisplayName(m *discordgo.Message) string {
	nick := ""
	if m.Member != nil && m.Member.Nick != "" {
		nick = m.Member.Nick
	} else if m.Author != nil && m.Author.GlobalName != "" {
		nick = m.Author.GlobalName
	} else if m.Author != nil {
		nick = m.Author.Username
	}
	if nick == "" {
		return "someone"
	}
	if m.Author != nil && nick != m.Author.Username {
		return fmt.Sprintf("%s (@%s)", nick, m.Author.Username)
	}
	return nick
}

func sortedKeys(m map[string]*discordgo.ApplicationCommandInteractionDataOption) []string {
	ks := make([]string, 0, len(m))
	for k := range m {
		ks = append(ks, k)
	}
	sort.Strings(ks)
	return ks
}

var _ = sortedKeys
var _ = ai.ChunkReply
