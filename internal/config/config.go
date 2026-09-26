package config

import (
	"bytes"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/Matko802/artixy/internal/ai"
)

// BotSettings mirrors the runtime-tunable subset of FileConfig.
type BotSettings struct {
	NotifyChannel *uint64 `json:"notify_channel"`
	WarMode       bool    `json:"war_mode"`
	SayasEnabled  bool    `json:"sayas_enabled"`
	AIEnabled     bool    `json:"ai_enabled"`
	AIModel       string  `json:"ai_model"`
	OllamaHost    string  `json:"ollama_host"`
	AIPrompt      string  `json:"ai_prompt"`
	AITemperature float32 `json:"ai_temperature"`
	AIThink       bool    `json:"ai_think"`
}

// FileConfig is the on-disk ~/.config/artixy/config.jsonc schema.
type FileConfig struct {
	OwnerID       *uint64           `json:"owner_id"`
	BlockedIDs    []uint64          `json:"blocked_ids"`
	DiscordToken  *string           `json:"discord_token"`
	VMName        *string           `json:"vm_name"`
	LibvirtURI    string            `json:"libvirt_uri"`
	WarMode       bool              `json:"war_mode"`
	SayasEnabled  bool              `json:"sayas_enabled"`
	NotifyChannel *uint64           `json:"notify_channel"`
	Managers      []uint64          `json:"managers"`
	AdminIDs      []uint64          `json:"admin_ids"`
	Linux         map[string]string `json:"linux"`
	Shells        map[string]string `json:"shells"`
	AIEnabled     bool              `json:"ai_enabled"`
	AIModel       string            `json:"ai_model"`
	OllamaHost    string            `json:"ollama_host"`
	AIPrompt      string            `json:"ai_prompt"`
	AITemperature float32           `json:"ai_temperature"`
	AIThink       bool              `json:"ai_think"`
}

// Allowed is the live access-control snapshot.
type Allowed struct {
	Owner   uint64
	Users   []uint64
	Linux   map[string]string
	Blocked []uint64
	Admins  []uint64
}

// Data is shared bot state.
type Data struct {
	Mu         sync.RWMutex
	Allowed    Allowed
	VM         string
	LibvirtURI string
	Settings   BotSettings
	Shells     map[string]string

	Live *LiveRegistry
}

// LiveRegistry is implemented in the live package to avoid an import cycle;
// we keep an interface here so config stays leaf-level.
type LiveRegistry interface{}

func ConfigFilePath() string {
	if xdg := os.Getenv("XDG_CONFIG_HOME"); strings.TrimSpace(xdg) != "" {
		return filepath.Join(xdg, "artixy", "config.jsonc")
	}
	home, err := os.UserHomeDir()
	if err != nil || strings.TrimSpace(home) == "" {
		return "config.jsonc"
	}
	return filepath.Join(home, ".config", "artixy", "config.jsonc")
}

const configTemplate = `// artixy config — edits hot-apply within seconds, no restart needed.
// File location: ~/.config/artixy/config.jsonc (NOT the project dir).
// This is JSONC: plain JSON plus // and /* */ comments and trailing commas.
{
  // Your Discord user ID (number). Get it via Discord: Settings > Advanced > Developer Mode,
  // then right-click your name > Copy User ID.
  "owner_id": 0,
  // Bot token from https://discord.com/developers/applications. Keep secret, never git.
  "discord_token": "",
  // libvirt domain name, e.g. "artix". VM commands error out until this is set.
  "vm_name": "",
  // libvirt connection URI. Local default talks to this machine's daemon.
  // To manage a VM on another machine (e.g. bot on Pi, VM on PC):
  // "libvirt_uri": "qemu+ssh://matko@fishy.local/system?keyfile=/home/matko/.ssh/artixy_libvirt&no_verify=1",
  "libvirt_uri": "qemu:///system",
  "blocked_ids": [],
  "war_mode": false,
  "sayas_enabled": false,
  // Channel ID for boot messages, or null to turn them off.
  "notify_channel": null,
  "managers": [],
  "admin_ids": [],
  // Discord user ID (as string) -> linux username in the VM.
  "linux": {},
  // Discord user ID (as string) -> "fish" or "bash".
  "shells": {},
  "ai_enabled": false,
  "ai_model": "llama3.1",
  "ollama_host": "http://127.0.0.1:11434",
  // Backstory/system prompt. Use \n for newlines; long text is fine on one line.
  "ai_prompt": "",
  "ai_temperature": 0.8,
  "ai_think": false,
}
`

func EnsureConfigTemplate() {
	p := ConfigFilePath()
	if _, err := os.Stat(p); err == nil {
		return
	}
	_ = os.MkdirAll(filepath.Dir(p), 0o700)
	_ = os.WriteFile(p, []byte(configTemplate), 0o600)
	LockConfigPrivate()
}

func LockConfigPrivate() {
	p := ConfigFilePath()
	if fi, err := os.Stat(p); err == nil {
		_ = fi
		_ = os.Chmod(p, 0o600)
	}
}

// DefaultLibvirtURI is the local system libvirt daemon.
func DefaultLibvirtURI() string { return "qemu:///system" }

func ResolveLibvirtURI(configured string) string {
	if v := strings.TrimSpace(configured); v != "" {
		return v
	}
	return DefaultLibvirtURI()
}

func normalizeFileConfig(cfg FileConfig) FileConfig {
	if cfg.DiscordToken != nil && strings.TrimSpace(*cfg.DiscordToken) == "" {
		cfg.DiscordToken = nil
	}
	if cfg.VMName != nil && strings.TrimSpace(*cfg.VMName) == "" {
		cfg.VMName = nil
	}
	if cfg.OwnerID != nil && *cfg.OwnerID == 0 {
		cfg.OwnerID = nil
	}
	cfg.LibvirtURI = ResolveLibvirtURI(cfg.LibvirtURI)
	if cfg.Linux == nil {
		cfg.Linux = map[string]string{}
	}
	if cfg.Shells == nil {
		cfg.Shells = map[string]string{}
	}
	if cfg.BlockedIDs == nil {
		cfg.BlockedIDs = []uint64{}
	}
	if cfg.Managers == nil {
		cfg.Managers = []uint64{}
	}
	if cfg.AdminIDs == nil {
		cfg.AdminIDs = []uint64{}
	}
	if !ai.ValidModelName(cfg.AIModel) {
		cfg.AIModel = ai.DefaultModel()
	} else {
		cfg.AIModel = strings.TrimSpace(cfg.AIModel)
	}
	host := strings.TrimSpace(strings.TrimRight(strings.TrimSpace(cfg.OllamaHost), "/"))
	if host == "" {
		cfg.OllamaHost = ai.DefaultHost()
	} else {
		cfg.OllamaHost = host
	}
	cfg.AITemperature = ai.ClampTemperature(cfg.AITemperature)
	return cfg
}

// stripJSONC removes // line comments, /* */ block comments and trailing
// commas, honouring double-quoted strings (with backslash escapes).
func stripJSONC(src []byte) []byte {
	var out bytes.Buffer
	out.Grow(len(src))
	i := 0
	n := len(src)
	// pendingComma holds a ',' seen outside strings; flushed unless a
	// closing bracket follows (with only whitespace/comments between).
	pendingComma := -1 // index in out where the comma was written
	flushComma := func() { pendingComma = -1 }
	discardComma := func() {
		if pendingComma >= 0 {
			b := out.Bytes()
			out.Reset()
			out.Write(b[:pendingComma])
			pendingComma = -1
		}
	}
	for i < n {
		c := src[i]
		if c == '"' {
			flushComma()
			out.WriteByte(c)
			i++
			for i < n {
				out.WriteByte(src[i])
				if src[i] == '\\' && i+1 < n {
					out.WriteByte(src[i+1])
					i += 2
					continue
				}
				if src[i] == '"' {
					i++
					break
				}
				i++
			}
			continue
		}
		if c == '/' && i+1 < n && src[i+1] == '/' {
			i += 2
			for i < n && src[i] != '\n' {
				i++
			}
			continue
		}
		if c == '/' && i+1 < n && src[i+1] == '*' {
			i += 2
			for i < n {
				if src[i] == '*' && i+1 < n && src[i+1] == '/' {
					i += 2
					break
				}
				i++
			}
			continue
		}
		if c == ',' {
			out.WriteByte(c)
			pendingComma = out.Len() - 1
			i++
			continue
		}
		if c == '}' || c == ']' {
			discardComma()
			out.WriteByte(c)
			i++
			continue
		}
		if c == ' ' || c == '\t' || c == '\n' || c == '\r' {
			out.WriteByte(c)
			i++
			continue
		}
		flushComma()
		out.WriteByte(c)
		i++
	}
	return out.Bytes()
}

func TryLoadFileConfig() *FileConfig {
	raw, err := os.ReadFile(ConfigFilePath())
	if err != nil {
		return nil
	}
	var cfg FileConfig
	if err := json.Unmarshal(stripJSONC(raw), &cfg); err != nil {
		fmt.Fprintf(os.Stderr, "config: parse error in %s: %v — keeping current settings; fix the JSON (check commas and quotes)\n", ConfigFilePath(), err)
		return nil
	}
	n := normalizeFileConfig(cfg)
	return &n
}

func LoadFileConfig() FileConfig {
	if cfg := TryLoadFileConfig(); cfg != nil {
		LockConfigPrivate()
		return *cfg
	}
	return normalizeFileConfig(FileConfig{})
}

func configMtime() *time.Time {
	fi, err := os.Stat(ConfigFilePath())
	if err != nil {
		return nil
	}
	t := fi.ModTime()
	return &t
}

// ApplyFileConfig hot-applies cfg into d, returning changed key names.
func ApplyFileConfig(d *Data, cfg *FileConfig) []string {
	var changed []string
	d.Mu.Lock()
	defer d.Mu.Unlock()

	if cfg.OwnerID != nil && *cfg.OwnerID != 0 && *cfg.OwnerID != d.Allowed.Owner {
		d.Allowed.Owner = *cfg.OwnerID
		changed = append(changed, "owner")
	}
	if !equalU64(d.Allowed.Users, cfg.Managers) {
		d.Allowed.Users = append([]uint64(nil), cfg.Managers...)
		changed = append(changed, "managers")
	}
	if !equalStrMap(d.Allowed.Linux, cfg.Linux) {
		d.Allowed.Linux = cloneStrMap(cfg.Linux)
		changed = append(changed, "linux")
	}
	if !equalU64(d.Allowed.Blocked, cfg.BlockedIDs) {
		d.Allowed.Blocked = append([]uint64(nil), cfg.BlockedIDs...)
		changed = append(changed, "blocked_ids")
	}
	if !equalU64(d.Allowed.Admins, cfg.AdminIDs) {
		d.Allowed.Admins = append([]uint64(nil), cfg.AdminIDs...)
		changed = append(changed, "admin_ids")
	}
	s := &d.Settings
	if !equalOptU64(s.NotifyChannel, cfg.NotifyChannel) {
		s.NotifyChannel = cfg.NotifyChannel
		changed = append(changed, "notify_channel")
	}
	if s.WarMode != cfg.WarMode {
		s.WarMode = cfg.WarMode
		changed = append(changed, "war_mode")
	}
	if s.SayasEnabled != cfg.SayasEnabled {
		s.SayasEnabled = cfg.SayasEnabled
		changed = append(changed, "sayas_enabled")
	}
	if s.AIEnabled != cfg.AIEnabled {
		s.AIEnabled = cfg.AIEnabled
		changed = append(changed, "ai_enabled")
	}
	if s.AIModel != cfg.AIModel {
		s.AIModel = cfg.AIModel
		changed = append(changed, "ai_model")
	}
	if s.OllamaHost != cfg.OllamaHost {
		s.OllamaHost = cfg.OllamaHost
		changed = append(changed, "ollama_host")
	}
	if s.AIPrompt != cfg.AIPrompt {
		s.AIPrompt = cfg.AIPrompt
		ai.ClearAllHistory()
		changed = append(changed, "ai_prompt")
	}
	if s.AITemperature != cfg.AITemperature {
		s.AITemperature = ai.ClampTemperature(cfg.AITemperature)
		changed = append(changed, "ai_temperature")
	}
	if s.AIThink != cfg.AIThink {
		s.AIThink = cfg.AIThink
		changed = append(changed, "ai_think")
	}
	if !equalStrMap(d.Shells, cfg.Shells) {
		d.Shells = cloneStrMap(cfg.Shells)
		changed = append(changed, "shells")
	}
	vmName := ""
	if cfg.VMName != nil {
		vmName = strings.TrimSpace(*cfg.VMName)
	}
	if d.VM != vmName {
		d.VM = vmName
		changed = append(changed, "vm_name")
	}
	if d.LibvirtURI != cfg.LibvirtURI {
		d.LibvirtURI = cfg.LibvirtURI
		changed = append(changed, "libvirt_uri")
	}
	return changed
}

// WatchConfig polls the config file and hot-applies changes.
// onChange (may be nil) runs after every successful apply so callers can
// sync derived state such as the live libvirt connection URI.
func WatchConfig(d *Data, onChange func(*FileConfig)) {
	last := configMtime()
	for {
		time.Sleep(2 * time.Second)
		cur := configMtime()
		if mtimeEqual(cur, last) {
			continue
		}
		time.Sleep(500 * time.Millisecond)
		stable := configMtime()
		last = stable
		if stable == nil {
			fmt.Fprintln(os.Stderr, "config: file missing, recreating template")
			EnsureConfigTemplate()
			last = configMtime()
			continue
		}
		cfg := TryLoadFileConfig()
		if cfg == nil {
			continue
		}
		changed := ApplyFileConfig(d, cfg)
		if len(changed) == 0 {
			fmt.Fprintln(os.Stderr, "config: file changed, no effective updates")
		} else {
			fmt.Fprintf(os.Stderr, "config: hot-applied %s\n", strings.Join(changed, ", "))
		}
		if onChange != nil {
			onChange(cfg)
		}
	}
}

func mtimeEqual(a, b *time.Time) bool {
	if a == nil && b == nil {
		return true
	}
	if a == nil || b == nil {
		return false
	}
	return a.Equal(*b)
}

const persistHeader = `// artixy config — edits hot-apply within seconds, no restart needed.
// This is JSONC: plain JSON plus // and /* */ comments and trailing commas.
`

// PersistRuntime writes live state back to the config file (atomic, 0600).
// Refuses to overwrite a file with a JSON parse error.
func PersistRuntime(d *Data) error {
	if _, err := os.Stat(ConfigFilePath()); err == nil {
		if TryLoadFileConfig() == nil {
			return fmt.Errorf("config file has a JSON parse error — not overwriting it; fix the commas and quotes in %s", ConfigFilePath())
		}
	}
	cfg := TryLoadFileConfig()
	if cfg == nil {
		c := FileConfig{}
		cfg = &c
	}
	d.Mu.RLock()
	cfg.Managers = append([]uint64(nil), d.Allowed.Users...)
	cfg.Linux = cloneStrMap(d.Allowed.Linux)
	cfg.AdminIDs = append([]uint64(nil), d.Allowed.Admins...)
	cfg.NotifyChannel = d.Settings.NotifyChannel
	cfg.WarMode = d.Settings.WarMode
	cfg.SayasEnabled = d.Settings.SayasEnabled
	cfg.AIEnabled = d.Settings.AIEnabled
	cfg.AIModel = d.Settings.AIModel
	cfg.OllamaHost = d.Settings.OllamaHost
	cfg.AIPrompt = d.Settings.AIPrompt
	cfg.AITemperature = d.Settings.AITemperature
	cfg.AIThink = d.Settings.AIThink
	cfg.Shells = cloneStrMap(d.Shells)
	d.Mu.RUnlock()

	// Re-apply so normalization/clamping stays consistent.
	n := normalizeFileConfig(*cfg)
	raw, err := json.MarshalIndent(n, "", "  ")
	if err != nil {
		return err
	}
	return atomicWrite(ConfigFilePath(), persistHeader+string(raw)+"\n")
}

func atomicWrite(path, data string) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return err
	}
	tmp, err := os.CreateTemp(filepath.Dir(path), ".tmp-*")
	if err != nil {
		return err
	}
	tmpName := tmp.Name()
	_ = tmp.Chmod(0o600)
	if _, err := tmp.WriteString(data); err != nil {
		tmp.Close()
		os.Remove(tmpName)
		return err
	}
	if err := tmp.Sync(); err != nil {
		tmp.Close()
		os.Remove(tmpName)
		return err
	}
	tmp.Close()
	return os.Rename(tmpName, path)
}

func AccessAllowed(owner uint64, users, blocked []uint64, id uint64) bool {
	return !containsU64(blocked, id) && (id == owner || containsU64(users, id))
}

func ElevatedAllowed(owner uint64, admins, blocked []uint64, id uint64) bool {
	return !containsU64(blocked, id) && (id == owner || containsU64(admins, id))
}

func containsU64(s []uint64, v uint64) bool {
	for _, x := range s {
		if x == v {
			return true
		}
	}
	return false
}

func equalU64(a, b []uint64) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

func equalOptU64(a, b *uint64) bool {
	if a == nil && b == nil {
		return true
	}
	if a == nil || b == nil {
		return false
	}
	return *a == *b
}

func equalStrMap(a, b map[string]string) bool {
	if len(a) != len(b) {
		return false
	}
	for k, v := range a {
		if bv, ok := b[k]; !ok || bv != v {
			return false
		}
	}
	return true
}

func cloneStrMap(m map[string]string) map[string]string {
	out := make(map[string]string, len(m))
	for k, v := range m {
		out[k] = v
	}
	return out
}
