package util

import "testing"

func TestValidRunas(t *testing.T) {
	for _, ok := range []string{"matko802", "_tallis_", "cootshk", "bob-1"} {
		if !ValidRunas(ok) {
			t.Fatalf("%s should be valid", ok)
		}
	}
	for _, bad := range []string{"", "root", "Root", "9bob", "bob!", "a_very_long_username_over_32_chars_xx"} {
		if ValidRunas(bad) {
			t.Fatalf("%s should be invalid", bad)
		}
	}
}

func TestPlainTail(t *testing.T) {
	got := PlainTail("$ ls\nfoo\n")
	if got == "" || len(got) < 5 {
		t.Fatalf("plain tail empty: %q", got)
	}
}

func TestStripSGR(t *testing.T) {
	got := StripSGR("\x1b[31mhi\x1b[0m")
	if got != "hi" {
		t.Fatalf("got %q", got)
	}
}
