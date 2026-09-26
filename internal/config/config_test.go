package config

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

func TestStripJSONC(t *testing.T) {
	src := `{
		// line comment
		"a": 1, /* block comment */
		"b": "x,}y // not a comment",
		"c": [1, 2,],
	}`
	var v struct {
		A int    `json:"a"`
		B string `json:"b"`
		C []int  `json:"c"`
	}
	if err := json.Unmarshal(stripJSONC([]byte(src)), &v); err != nil {
		t.Fatalf("parse: %v", err)
	}
	if v.A != 1 || v.B != "x,}y // not a comment" || len(v.C) != 2 {
		t.Fatalf("got %+v", v)
	}
}

func TestTemplateParses(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("XDG_CONFIG_HOME", dir)
	EnsureConfigTemplate()
	cfg := TryLoadFileConfig()
	if cfg == nil {
		t.Fatal("template should parse")
	}
	if cfg.AIModel != "llama3.1" || cfg.OllamaHost != "http://127.0.0.1:11434" {
		t.Fatalf("defaults wrong: %+v", cfg)
	}
	if cfg.LibvirtURI != "qemu:///system" {
		t.Fatalf("libvirt default wrong: %q", cfg.LibvirtURI)
	}
	// file must be private
	fi, err := os.Stat(filepath.Join(dir, "artixy", "config.jsonc"))
	if err != nil || fi.Mode().Perm() != 0o600 {
		t.Fatalf("perm: %v %v", fi, err)
	}
}

func TestPersistRoundTrip(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("XDG_CONFIG_HOME", dir)
	EnsureConfigTemplate()
	// Seed a vm_name in the file: persist must preserve it (it only writes
	// access lists, settings and shells back, never vm/owner/token).
	seed := `{"vm_name": "artix", "managers": [], "admin_ids": [], "linux": {}, "shells": {}}`
	if err := os.WriteFile(filepath.Join(dir, "artixy", "config.jsonc"), []byte(seed), 0o600); err != nil {
		t.Fatal(err)
	}
	d := &Data{
		Allowed: Allowed{Owner: 123, Users: []uint64{4}, Linux: map[string]string{"4": "bob"}, Blocked: []uint64{}, Admins: []uint64{}},
		VM:      "",
		Settings: BotSettings{
			AIModel: "qwen3.5:4b", OllamaHost: "http://x:11434", AIPrompt: "hi\nthere",
			AITemperature: 0.5, AIEnabled: true,
		},
		Shells: map[string]string{"4": "fish"},
	}
	if err := PersistRuntime(d); err != nil {
		t.Fatalf("persist: %v", err)
	}
	cfg := TryLoadFileConfig()
	if cfg == nil {
		t.Fatal("persisted file should parse")
	}
	if cfg.VMName == nil || *cfg.VMName != "artix" {
		t.Fatalf("vm: %+v", cfg.VMName)
	}
	if cfg.AIPrompt != "hi\nthere" || cfg.Shells["4"] != "fish" || cfg.Linux["4"] != "bob" {
		t.Fatalf("round trip mismatch: %+v", cfg)
	}
}
