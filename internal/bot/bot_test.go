package bot

import "testing"

func TestGuestDir(t *testing.T) {
	if d, ok := NormalizeGuestDir("/tmp/artixy-uploads/a"); !ok || d != "/tmp/artixy-uploads/a" {
		t.Fatalf("got %q %v", d, ok)
	}
	if _, ok := NormalizeGuestDir("tmp/x"); ok {
		t.Fatal("relative should fail")
	}
	if _, ok := NormalizeGuestDir("/../etc"); ok {
		t.Fatal("escape should fail")
	}
}

func TestUploadAllow(t *testing.T) {
	if !AllowedUploadDir("/tmp/artixy-uploads", "") {
		t.Fatal("uploads dir")
	}
	if AllowedUploadDir("/etc/cron.d", "") {
		t.Fatal("etc should be denied")
	}
	if !AllowedUploadDir("/home/bob/docs", "bob") {
		t.Fatal("home should be allowed")
	}
	if AllowedUploadDir("/home/alice/docs", "bob") {
		t.Fatal("other home denied")
	}
}

func TestSensitive(t *testing.T) {
	for _, n := range []string{".env", "config.toml", "id_rsa", "api_token.txt"} {
		if !IsSensitiveSendName(n) {
			t.Fatalf("%s should be refused", n)
		}
	}
	if IsSensitiveSendName("report.pdf") {
		t.Fatal("report.pdf should be allowed")
	}
}

func TestParseRef(t *testing.T) {
	ch, msg, ok := ParseMessageRef("https://discord.com/channels/1/2/3", "9")
	if !ok || ch != "2" || msg != "3" {
		t.Fatalf("link parse got %q %q %v", ch, msg, ok)
	}
	ch, msg, ok = ParseMessageRef("12345", "9")
	if !ok || ch != "9" || msg != "12345" {
		t.Fatalf("id parse got %q %q %v", ch, msg, ok)
	}
}
