package ai

import "testing"

func TestValidModelName(t *testing.T) {
	for _, m := range []string{"llama3.1", "qwen2.5-coder:7b", "qwen3.5:4b"} {
		if !ValidModelName(m) {
			t.Fatalf("%s should be valid", m)
		}
	}
	for _, m := range []string{"", "bad name", "../evil", ".lead", "trail.", "a" + string(make([]byte, 200))} {
		if ValidModelName(m) {
			t.Fatalf("%q should be invalid", m)
		}
	}
}

func TestClamp(t *testing.T) {
	if ClampTemperature(0.7) != 0.7 {
		t.Fatal("0.7")
	}
	if ClampTemperature(-1) != 0 {
		t.Fatal("neg")
	}
	if ClampTemperature(9) != 2 {
		t.Fatal("high")
	}
}

func TestChunk(t *testing.T) {
	if len(ChunkReply("hi")) != 1 {
		t.Fatal("chunk")
	}
}
