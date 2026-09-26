package main

import (
	"fmt"
	"os"
	"os/signal"
	"strings"
	"syscall"

	"github.com/Matko802/artixy/internal/ai"
	"github.com/Matko802/artixy/internal/bot"
	"github.com/Matko802/artixy/internal/config"
	"github.com/Matko802/artixy/internal/live"
	"github.com/Matko802/artixy/internal/vm"
	"github.com/bwmarrin/discordgo"
)

func main() {
	args := os.Args[1:]
	if len(args) > 0 && (args[0] == "help" || args[0] == "--help" || args[0] == "-h") {
		fmt.Println("artixy — run with no args to start the Discord bot.")
		return
	}
	config.EnsureConfigTemplate()
	fileCfg := config.LoadFileConfig()

	token := ""
	if fileCfg.DiscordToken != nil {
		token = *fileCfg.DiscordToken
	}
	if strings.TrimSpace(token) == "" {
		token = strings.TrimSpace(os.Getenv("DISCORD_TOKEN"))
	}
	if strings.TrimSpace(token) == "" {
		fmt.Fprintf(os.Stderr, "error: set discord_token in %s or DISCORD_TOKEN env\n", config.ConfigFilePath())
		os.Exit(1)
	}
	var owner uint64
	if fileCfg.OwnerID != nil {
		owner = *fileCfg.OwnerID
	} else {
		ownerStr := strings.TrimSpace(os.Getenv("OWNER_ID"))
		if ownerStr == "" {
			fmt.Fprintf(os.Stderr, "error: set owner_id in %s or OWNER_ID env\n", config.ConfigFilePath())
			os.Exit(1)
		}
		if _, err := fmt.Sscanf(ownerStr, "%d", &owner); err != nil || owner == 0 {
			fmt.Fprintln(os.Stderr, "error: OWNER_ID must be a number")
			os.Exit(1)
		}
	}

	vmName := ""
	if fileCfg.VMName != nil {
		vmName = strings.TrimSpace(*fileCfg.VMName)
	}
	if vmName == "" {
		vmName = strings.TrimSpace(os.Getenv("VM_NAME"))
	}
	if vmName == "" {
		fmt.Fprintf(os.Stderr, "warning: vm_name not set in %s or VM_NAME env — VM commands will reply with a friendly error until you set it\n", config.ConfigFilePath())
	}

	aiModel := fileCfg.AIModel
	if !ai.ValidModelName(aiModel) {
		aiModel = ai.DefaultModel()
	}
	ollamaHost := strings.TrimRight(strings.TrimSpace(fileCfg.OllamaHost), "/")
	if ollamaHost == "" {
		ollamaHost = ai.DefaultHost()
	}

	data := &config.Data{
		Allowed: config.Allowed{
			Owner:   owner,
			Users:   append([]uint64(nil), fileCfg.Managers...),
			Linux:   cloneMap(fileCfg.Linux),
			Blocked: append([]uint64(nil), fileCfg.BlockedIDs...),
			Admins:  append([]uint64(nil), fileCfg.AdminIDs...),
		},
		VM:         vmName,
		LibvirtURI: config.ResolveLibvirtURI(fileCfg.LibvirtURI),
		Settings: config.BotSettings{
			NotifyChannel: fileCfg.NotifyChannel,
			WarMode:       fileCfg.WarMode,
			SayasEnabled:  fileCfg.SayasEnabled,
			AIEnabled:     fileCfg.AIEnabled,
			AIModel:       aiModel,
			OllamaHost:    ollamaHost,
			AIPrompt:      fileCfg.AIPrompt,
			AITemperature: ai.ClampTemperature(fileCfg.AITemperature),
			AIThink:       fileCfg.AIThink,
		},
		Shells: cloneMap(fileCfg.Shells),
		Live:   nil,
	}
	lm := live.NewMap()

	dg, err := discordgo.New("Bot " + token)
	if err != nil {
		fmt.Fprintf(os.Stderr, "client build: %v\n", err)
		os.Exit(1)
	}
	dg.Identify.Intents = discordgo.IntentsGuildMessages | discordgo.IntentsDirectMessages | discordgo.IntentsGuilds | discordgo.IntentMessageContent

	b := bot.NewBot(dg, data, lm)

	botRef := b
	dg.AddHandler(func(s *discordgo.Session, m *discordgo.MessageCreate) {
		botRef.HandleMessage(s, m)
	})
	dg.AddHandler(func(s *discordgo.Session, ic *discordgo.InteractionCreate) {
		botRef.HandleInteraction(s, ic)
	})
	dg.AddHandler(func(s *discordgo.Session, r *discordgo.Ready) {
		fmt.Fprintf(os.Stderr, "artixy logged in as %s\n", r.User.String())
		// register slash commands globally (best effort)
		for _, cmd := range bot.SlashCommands() {
			if _, err := s.ApplicationCommandCreate(s.State.User.ID, "", cmd); err != nil {
				fmt.Fprintf(os.Stderr, "slash register %s: %v\n", cmd.Name, err)
			}
		}
		live.CleanupStaleLiveFiles(data.VM)
		data.Mu.RLock()
		ch := data.Settings.NotifyChannel
		data.Mu.RUnlock()
		if ch != nil {
			_, _ = s.ChannelMessageSend(fmt.Sprintf("%d", *ch), "```\n"+bot.BootArt+"\n```")
		}
	})

	vm.SetConnectionURI(data.LibvirtURI)
	go config.WatchConfig(data, func(cfg *config.FileConfig) {
		vm.SetConnectionURI(config.ResolveLibvirtURI(cfg.LibvirtURI))
	})

	if err := dg.Open(); err != nil {
		msg := err.Error()
		if strings.Contains(msg, "401") || strings.Contains(strings.ToLower(msg), "unauthorized") {
			fmt.Fprintf(os.Stderr, "Discord rejected the token (401 Unauthorized) — check discord_token in config: %v\n", err)
		} else {
			fmt.Fprintf(os.Stderr, "client start: %v\n", err)
		}
		// also surface vm import usage (avoid unused)
		_ = vm.AgentPing
		os.Exit(1)
	}
	// learn bot id for mention handling
	if u, err := dg.User("@me"); err == nil && u != nil {
		b.BotID = u.ID
	}

	fmt.Println("artixy is running. Press CTRL-C to exit.")
	sc := make(chan os.Signal, 1)
	signal.Notify(sc, syscall.SIGINT, syscall.SIGTERM, os.Interrupt)
	<-sc
	_ = dg.Close()
}

func cloneMap(m map[string]string) map[string]string {
	out := make(map[string]string, len(m))
	for k, v := range m {
		out[k] = v
	}
	return out
}
