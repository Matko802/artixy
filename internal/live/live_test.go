package live

import "testing"

func TestExpandKeys(t *testing.T) {
	if got := ExpandTypedInput(";enter"); got != "\r" {
		t.Fatalf("enter got %q", got)
	}
	if got := ExpandTypedInput("hi ;space there"); got != "hi   there" {
		t.Fatalf("space got %q", got)
	}
	if got := ExpandTypedInput("a\\nb"); got != "a\nb" {
		t.Fatalf("nl got %q", got)
	}
}
