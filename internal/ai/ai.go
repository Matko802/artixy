package ai

import (
	"bytes"
	"container/list"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"strings"
	"sync"
	"time"
)

type HistoryItem struct {
	Role    string
	Content string
}

var (
	histMu sync.Mutex
	hist   = map[uint64]*list.List{}
)

func snapshot(channel uint64) []HistoryItem {
	histMu.Lock()
	defer histMu.Unlock()
	l, ok := hist[channel]
	if !ok {
		return nil
	}
	var out []HistoryItem
	for e := l.Front(); e != nil; e = e.Next() {
		out = append(out, e.Value.(HistoryItem))
	}
	// copy
	cp := make([]HistoryItem, len(out))
	copy(cp, out)
	return cp
}

func push(channel uint64, role, content string) {
	histMu.Lock()
	defer histMu.Unlock()
	if _, ok := hist[channel]; !ok {
		if len(hist) >= 200 {
			for k := range hist {
				delete(hist, k)
				break
			}
		}
		hist[channel] = list.New()
	}
	l := hist[channel]
	l.PushBack(HistoryItem{Role: role, Content: content})
	for l.Len() > 10 {
		l.Remove(l.Front())
	}
	for {
		total := 0
		for e := l.Front(); e != nil; e = e.Next() {
			total += len(e.Value.(HistoryItem).Content)
		}
		if total <= 3000 || l.Len() == 0 {
			break
		}
		l.Remove(l.Front())
	}
}

var httpClient = &http.Client{Timeout: 180 * time.Second}

func DefaultModel() string { return "llama3.1" }
func DefaultHost() string  { return "http://127.0.0.1:11434" }

func ResolveHost(configured string) string {
	for _, key := range []string{"OLLAMA_HOST", "OLLAMA_URL"} {
		if v := strings.TrimRight(strings.TrimSpace(os.Getenv(key)), "/"); v != "" {
			return v
		}
	}
	v := strings.TrimRight(strings.TrimSpace(configured), "/")
	if v == "" {
		return DefaultHost()
	}
	return v
}

func ValidModelName(s string) bool {
	s = strings.TrimSpace(s)
	if s == "" || len(s) > 128 {
		return false
	}
	if strings.HasPrefix(s, ".") || strings.HasPrefix(s, "-") || strings.HasPrefix(s, "/") || strings.HasPrefix(s, ":") {
		return false
	}
	if strings.HasSuffix(s, ".") || strings.HasSuffix(s, "-") || strings.HasSuffix(s, "/") || strings.HasSuffix(s, ":") {
		return false
	}
	if strings.Contains(s, "..") || strings.Contains(s, "//") || strings.Contains(s, " ") {
		return false
	}
	for _, c := range s {
		if (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') {
			continue
		}
		switch c {
		case '.', '_', '-', ':', '/':
		default:
			return false
		}
	}
	return true
}

func DefaultTemperature() float32 { return 0.8 }

func ClampTemperature(t float32) float32 {
	if t != t { // NaN
		return 0.8
	}
	if t < 0 {
		return 0
	}
	if t > 2 {
		return 2
	}
	return t
}

func CleanReply(text string) string {
	var sb strings.Builder
	blanks := 0
	for _, line := range strings.Split(text, "\n") {
		if strings.TrimSpace(line) == "" {
			blanks++
			if blanks <= 1 {
				sb.WriteString("\n")
			}
		} else {
			blanks = 0
			sb.WriteString(line)
			sb.WriteString("\n")
		}
	}
	return strings.TrimSpace(sb.String())
}

func StripLeadingSpeaker(text string, names []string) string {
	out := strings.TrimLeft(text, " \t\n\r")
	for {
		t := strings.TrimLeft(out, " \t\n\r")
		if !strings.HasPrefix(t, "[") {
			break
		}
		end := strings.Index(t, "]")
		if end <= 1 || end > 65 {
			break
		}
		after := strings.TrimLeft(t[end+1:], " \t\n\r")
		if !strings.HasPrefix(after, ":") {
			break
		}
		out = strings.TrimLeft(after[1:], " \t\n\r")
	}
	ordered := append([]string(nil), names...)
	// longest first
	for i := 0; i < len(ordered); i++ {
		for j := i + 1; j < len(ordered); j++ {
			if len(ordered[j]) > len(ordered[i]) {
				ordered[i], ordered[j] = ordered[j], ordered[i]
			}
		}
	}
	for {
		t := strings.TrimLeft(out, " \t\n\r")
		hit := false
		for _, n := range ordered {
			n = strings.TrimSpace(n)
			if n == "" {
				continue
			}
			if len(t) > len(n) && t[:len(n)] == n && strings.HasPrefix(t[len(n):], ":") {
				out = strings.TrimLeft(t[len(n)+1:], " \t\n\r")
				hit = true
				break
			}
		}
		if !hit {
			break
		}
	}
	return out
}

var metaStarters = []string{
	"let me summarize",
	"just to sum it up",
	"just to summarize",
	"let's simplify",
	"let me simplify",
	"to simplify,",
	"too much info",
	"i got carried away",
	"i think i got",
	"here is a summary",
	"here's a summary",
	"to summarize,",
	"in summary,",
}

func metaPreambleLen(text string) int {
	t := strings.TrimLeft(text, " \t\n\r")
	low := strings.ToLower(t)
	for _, s := range metaStarters {
		if !strings.HasPrefix(low, s) {
			continue
		}
		rest := t[len(s):]
		if strings.HasSuffix(s, ",") || strings.HasPrefix(rest, ":") || strings.HasPrefix(rest, ",") {
			cut := len(s)
			if strings.HasPrefix(rest, ":") || strings.HasPrefix(rest, ",") {
				cut = len(s) + 1
			}
			if strings.TrimSpace(t[cut:]) == "" {
				return 0
			}
			return cut
		}
		idx := strings.IndexAny(rest, ".!?")
		if idx < 0 {
			return 0
		}
		cut := len(s) + idx + 1
		if strings.TrimSpace(t[cut:]) == "" {
			return 0
		}
		return cut
	}
	return 0
}

func StripMetaPreamble(text string) string {
	out := text
	for {
		cut := metaPreambleLen(out)
		if cut == 0 {
			break
		}
		out = strings.TrimLeft(strings.TrimLeft(out, " \t\n\r")[cut:], " \t\n\r")
	}
	return out
}

func SanitizeReply(raw string, names []string) string {
	return StripMetaPreamble(StripLeadingSpeaker(CleanReply(raw), names))
}

func FinalizeReply(channel uint64, tagged string, names []string, raw string) (string, error) {
	text := SanitizeReply(raw, names)
	if strings.TrimSpace(text) == "" {
		return "", fmt.Errorf("ollama returned an empty reply")
	}
	push(channel, "user", tagged)
	push(channel, "assistant", text)
	return text, nil
}

func IsRateLimitErr(s string) bool {
	t := strings.ToLower(s)
	return strings.Contains(t, "429") ||
		strings.Contains(t, "rate limit") ||
		strings.Contains(t, "rate_limit") ||
		strings.Contains(t, "rate-limited") ||
		strings.Contains(t, "too many requests") ||
		strings.Contains(t, "quota")
}

func IsAPIFullErr(s string) bool {
	if IsRateLimitErr(s) {
		return true
	}
	t := strings.ToLower(s)
	return strings.Contains(t, "503") ||
		strings.Contains(t, "529") ||
		strings.Contains(t, "overload") ||
		strings.Contains(t, "capacity") ||
		strings.Contains(t, "server is busy") ||
		strings.Contains(t, "try again in a bit") ||
		strings.Contains(t, "api full")
}

func APIFullMessage() string {
	return "Sorry, I'm running hot right now (API full/rate limited) — try again in a minute."
}

func BuildTranscript(past []HistoryItem, tagged string, names []string) string {
	var sb strings.Builder
	for _, e := range past {
		if e.Role == "assistant" {
			cleaned := SanitizeReply(e.Content, names)
			if strings.TrimSpace(cleaned) == "" || StaleHistoryLine(cleaned) {
				continue
			}
			sb.WriteString(cleaned)
		} else {
			sb.WriteString(e.Content)
		}
		sb.WriteString("\n")
	}
	sb.WriteString(tagged)
	return sb.String()
}

type generateResponse struct {
	Response *string `json:"response"`
	Message  *struct {
		Content string `json:"content"`
	} `json:"message"`
}

func generateOnce(url, model, system, prompt string, temperature float32, think bool) (string, error) {
	req := map[string]interface{}{
		"model":      model,
		"prompt":     prompt,
		"stream":     false,
		"think":      think,
		"keep_alive": "10m",
		"options":    map[string]interface{}{"temperature": ClampTemperature(temperature)},
	}
	if strings.TrimSpace(system) != "" {
		req["system"] = system
	}
	body, _ := json.Marshal(req)
	resp, err := httpClient.Post(url, "application/json", bytes.NewReader(body))
	if err != nil {
		return "", err
	}
	defer resp.Body.Close()
	raw, _ := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		s := strings.TrimSpace(string(raw))
		if len(s) > 300 {
			s = string([]rune(s)[:300])
		}
		return "", fmt.Errorf("ollama %s: %s", resp.Status, s)
	}
	var parsed generateResponse
	if err := json.Unmarshal(raw, &parsed); err != nil {
		return "", err
	}
	if parsed.Response != nil && strings.TrimSpace(*parsed.Response) != "" {
		return *parsed.Response, nil
	}
	if parsed.Message != nil && strings.TrimSpace(parsed.Message.Content) != "" {
		return parsed.Message.Content, nil
	}
	return "", fmt.Errorf("ollama returned an empty reply")
}

func StaleHistoryLine(s string) bool {
	return strings.Contains(strings.ToLower(strings.TrimSpace(s)), "running hot right now")
}

func GlitchText(host, model, systemPrompt string, temperature float32, think bool) string {
	url := strings.TrimRight(host, "/") + "/api/generate"
	raw, err := generateOnce(url, model, systemPrompt, "You just glitched out. Tell the user in one short sentence, no details.", temperature, think)
	if err != nil {
		if IsAPIFullErr(err.Error()) {
			return APIFullMessage()
		}
		return "sorry, glitched out — try again in a sec"
	}
	text := StripMetaPreamble(StripLeadingSpeaker(CleanReply(raw), nil))
	if strings.TrimSpace(text) == "" {
		return "sorry, glitched out — try again in a sec"
	}
	if IsAPIFullErr(text) {
		return APIFullMessage()
	}
	r := []rune(text)
	if len(r) > 300 {
		return string(r[:300])
	}
	return text
}

func OllamaChat(host, model string, channel uint64, speaker, prompt, systemPrompt string, temperature float32, think bool) (string, error) {
	host = strings.TrimRight(host, "/")
	url := host + "/api/generate"
	tagged := fmt.Sprintf("[%s]: %s", SpeakerTag(speaker), prompt)
	past := snapshot(channel)
	fmt.Fprintf(os.Stderr, "ai chat: model=%s sys_chars=%d hist_msgs=%d temp=%v\n", model, len([]rune(systemPrompt)), len(past), ClampTemperature(temperature))
	names := []string{SpeakerTag(speaker)}
	for _, e := range past {
		if e.Role == "assistant" {
			continue
		}
		t := strings.TrimLeft(e.Content, " \t")
		if strings.HasPrefix(t, "[") {
			if end := strings.Index(t, "]"); end > 1 && end <= 64 {
				n := strings.TrimSpace(t[1:end])
				found := false
				for _, x := range names {
					if x == n {
						found = true
						break
					}
				}
				if n != "" && !found {
					names = append(names, n)
				}
			}
		}
	}
	transcript := BuildTranscript(past, tagged, names)
	first, err := generateOnce(url, model, systemPrompt, transcript, temperature, think)
	if err != nil {
		if IsAPIFullErr(err.Error()) {
			fmt.Fprintln(os.Stderr, "ollama_chat: api full on first call, offline fallback")
			fb := APIFullMessage()
			push(channel, "user", tagged)
			push(channel, "assistant", fb)
			return fb, nil
		}
		return "", err
	}
	return FinalizeReply(channel, tagged, names, first)
}

func ClearHistory(channel uint64) {
	histMu.Lock()
	defer histMu.Unlock()
	delete(hist, channel)
}

func ClearAllHistory() {
	histMu.Lock()
	defer histMu.Unlock()
	hist = map[uint64]*list.List{}
}

func RecordArtixy(channel uint64, text string) {
	t := strings.TrimSpace(text)
	if t == "" {
		return
	}
	r := []rune(t)
	if len(r) > 1500 {
		t = string(r[:1500])
	}
	push(channel, "assistant", t)
}

func ModelPresent(host, model string) *bool {
	url := strings.TrimRight(host, "/") + "/api/tags"
	resp, err := httpClient.Get(url)
	if err != nil {
		return nil
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return nil
	}
	var tags struct {
		Models []struct {
			Name string `json:"name"`
		} `json:"models"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&tags); err != nil {
		return nil
	}
	want := strings.ToLower(strings.TrimSpace(model))
	base := strings.SplitN(want, ":", 2)[0]
	for _, m := range tags.Models {
		n := strings.ToLower(m.Name)
		nbase := strings.SplitN(n, ":", 2)[0]
		if n == want || nbase == base {
			b := true
			return &b
		}
	}
	b := false
	return &b
}

func SpeakerTag(raw string) string {
	flat := strings.Join(strings.Fields(raw), " ")
	var sb strings.Builder
	for _, c := range flat {
		switch c {
		case '[', ']':
			sb.WriteRune(' ')
		default:
			if c < 32 || c == 127 {
				sb.WriteRune(' ')
			} else {
				sb.WriteRune(c)
			}
		}
	}
	clean := strings.Join(strings.Fields(sb.String()), " ")
	r := []rune(clean)
	if len(r) > 64 {
		clean = string(r[:64])
	}
	if strings.TrimSpace(clean) == "" {
		return "someone"
	}
	return clean
}

func StripMention(content string, botID uint64) string {
	s := strings.ReplaceAll(content, fmt.Sprintf("<@%d>", botID), "")
	s = strings.ReplaceAll(s, fmt.Sprintf("<@!%d>", botID), "")
	return strings.TrimSpace(s)
}

func MentionsName(content string) bool {
	return strings.Contains(strings.ToLower(content), "artixy")
}

func StripName(content string) string {
	var out []string
	for _, w := range strings.Fields(content) {
		if !strings.Contains(strings.ToLower(w), "artixy") {
			out = append(out, w)
		}
	}
	return strings.Join(out, " ")
}

func ChunkReply(s string) []string {
	const max = 1900
	const maxChunks = 4
	s = strings.TrimSpace(s)
	if len([]rune(s)) <= max {
		return []string{s}
	}
	var chunks []string
	var cur strings.Builder
	curLen := 0
	flush := func() {
		t := strings.TrimRight(cur.String(), "\n")
		if strings.TrimSpace(t) != "" {
			chunks = append(chunks, t)
		}
		cur.Reset()
		curLen = 0
	}
	for _, line := range strings.Split(s, "\n") {
		lineLen := len([]rune(line)) + 1
		if lineLen > max {
			if strings.TrimSpace(cur.String()) != "" {
				flush()
			}
			r := []rune(line)
			for i := 0; i < len(r); i += max {
				end := i + max
				if end > len(r) {
					end = len(r)
				}
				chunks = append(chunks, string(r[i:end]))
				if len(chunks) >= maxChunks {
					break
				}
			}
			if len(chunks) >= maxChunks {
				break
			}
			continue
		}
		if curLen+lineLen > max {
			flush()
			if len(chunks) >= maxChunks {
				break
			}
		}
		cur.WriteString(line)
		cur.WriteString("\n")
		curLen += lineLen
	}
	if strings.TrimSpace(cur.String()) != "" && len(chunks) < maxChunks {
		flush()
	}
	if len(chunks) == 0 {
		r := []rune(s)
		if len(r) > max {
			r = r[:max]
		}
		return []string{string(r)}
	}
	return chunks
}
