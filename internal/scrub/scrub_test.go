package scrub

import "testing"

func TestPublicIPv4(t *testing.T) {
	if PublicIPv4([4]byte{8, 8, 8, 8}) != true {
		t.Fatal("8.8.8.8 should be public")
	}
	for _, ip := range [][4]byte{{10, 0, 0, 1}, {192, 168, 1, 1}, {127, 0, 0, 1}, {172, 20, 0, 1}} {
		if PublicIPv4(ip) {
			t.Fatalf("%v should be private", ip)
		}
	}
}

func TestScrub(t *testing.T) {
	got := ScrubPublicIP("my ip is 8.8.8.8 ok")
	if got != "my ip is [redacted] ok" {
		t.Fatalf("got %q", got)
	}
	got = ScrubPublicIP("local 192.168.1.5 stays")
	if got != "local 192.168.1.5 stays" {
		t.Fatalf("private should stay, got %q", got)
	}
}
