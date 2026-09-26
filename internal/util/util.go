package util

import (
	"crypto/rand"
	"encoding/hex"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"
	"unicode/utf8"
)

func ValidRunas(name string) bool {
	if name == "" || len(name) > 32 || name == "root" {
		return false
	}
	for i, c := range name {
		if i == 0 {
			if !(c >= 'a' && c <= 'z') && c != '_' {
				return false
			}
		} else {
			if !(c >= 'a' && c <= 'z') && !(c >= '0' && c <= '9') && c != '_' && c != '-' {
				return false
			}
		}
	}
	return true
}

func RandomSuffix() string {
	b := make([]byte, 8)
	if _, err := rand.Read(b); err == nil {
		return hex.EncodeToString(b)
	}
	return fmt.Sprintf("%x-%d", time.Now().UnixNano(), os.Getpid())
}

func StripSGR(s string) string {
	b := []byte(s)
	var out strings.Builder
	i := 0
	for i < len(b) {
		if b[i] == 0x1b && i+1 < len(b) && b[i+1] == '[' {
			j := i + 2
			for j < len(b) && !isAlpha(b[j]) {
				j++
			}
			i = min(j+1, len(b))
			continue
		}
		if b[i] == 0x1b {
			if i+1 < len(b) && b[i+1] == ']' {
				j := i + 2
				for j < len(b) && b[j] != 0x07 {
					if b[j] == 0x1b && j+1 < len(b) && b[j+1] == '\\' {
						j += 2
						break
					}
					j++
				}
				if j < len(b) && b[j] == 0x07 {
					j++
				}
				i = j
				continue
			}
			i++
			if i < len(b) {
				i++
			}
			continue
		}
		if b[i] == '\r' {
			i++
			continue
		}
		_, size := utf8.DecodeRune(b[i:])
		if size <= 0 {
			size = 1
		}
		out.Write(b[i : i+size])
		i += size
	}
	return out.String()
}

func isAlpha(b byte) bool {
	return (b >= 'A' && b <= 'Z') || (b >= 'a' && b <= 'z')
}

func min(a, b int) int {
	if a < b {
		return a
	}
	return b
}

func Codeblock(s string) string {
	t := strings.TrimRight(s, "\n")
	// light sanitize: strip SGR
	t = StripSGR(t)
	if len(t) > 1800 {
		r := []rune(t)
		if len(r) > 1790 {
			t = string(r[:1790]) + "\n…truncated"
		}
	}
	if strings.TrimSpace(t) == "" {
		t = "(empty)"
	}
	return "```\n" + t + "\n```"
}

func FitBottomLines(body string) (string, bool) {
	const max = 1750
	lines := strings.Split(body, "\n")
	var kept []string
	length := 0
	truncated := false
	for i := len(lines) - 1; i >= 0; i-- {
		l := lines[i]
		if len([]rune(l))+1 > max {
			if len(kept) == 0 {
				v := []rune(l)
				start := len(v) - (max - 1)
				if start < 0 {
					start = 0
				}
				kept = append(kept, string(v[start:])+"\n")
			}
			truncated = true
			break
		}
		n := len([]rune(l)) + 1
		if length+n > max {
			truncated = true
			break
		}
		kept = append([]string{l}, kept...)
		length += n
	}
	return strings.Join(kept, "\n"), truncated
}

func NormalizeNL(s string) string {
	s = strings.ReplaceAll(s, "\r\n", "\n")
	return strings.ReplaceAll(s, "\r", "\n")
}

func AfterLastClear(s string) string {
	b := []byte(s)
	lastEnd := -1
	i := 0
	for i < len(b) {
		if b[i] == 0x1b && i+1 < len(b) {
			if b[i+1] == 'c' {
				lastEnd = i + 2
				i += 2
				continue
			}
			if b[i+1] == '[' {
				j := i + 2
				for j < len(b) && (isDigit(b[j]) || b[j] == ';' || b[j] == '?') {
					j++
				}
				if j < len(b) && (b[j] == 'J' || b[j] == 'H' || b[j] == 'f') {
					lastEnd = j + 1
					i = j + 1
					continue
				}
			}
		}
		_, size := utf8.DecodeRune(b[i:])
		if size <= 0 {
			size = 1
		}
		i += size
		if i > len(b) {
			i = len(b)
		}
	}
	if lastEnd >= 0 && lastEnd <= len(s) {
		return s[lastEnd:]
	}
	return s
}

func isDigit(b byte) bool { return b >= '0' && b <= '9' }

func PlainTail(body string) string {
	cr := NormalizeNL(strings.TrimRight(body, " \t\n\r"))
	clean := StripSGR(AfterLastClear(cr))
	fitted, truncated := FitBottomLines(clean)
	t := fitted
	if strings.TrimSpace(t) == "" {
		t = "(empty)"
	}
	if truncated {
		return "```\n…\n" + t + "\n```"
	}
	return "```\n" + t + "\n```"
}

func CapFileBody(clean string) string {
	const fileMax = 400000
	r := []rune(clean)
	if len(r) <= fileMax {
		return clean
	}
	return fmt.Sprintf("…[showing last %d chars]\n%s", fileMax, string(r[len(r)-fileMax:]))
}

func AttachName(cmd string) string {
	w := ""
	if f := strings.Fields(cmd); len(f) > 0 {
		w = f[0]
	} else {
		w = "output"
	}
	var sb strings.Builder
	for _, c := range w {
		if (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '-' || c == '_' {
			sb.WriteRune(c)
		}
	}
	if sb.String() == "" {
		return "output.txt"
	}
	return sb.String() + ".txt"
}

func ToolPath(name string) string {
	if name == "" || strings.Contains(name, "/") {
		return ""
	}
	dirs := filepath.SplitList(os.Getenv("PATH"))
	dirs = append(dirs, "/run/current-system/sw/bin", "/run/wrappers/bin", "/nix/profile/bin", "/usr/local/bin", "/usr/bin", "/bin")
	if home, err := os.UserHomeDir(); err == nil {
		dirs = append(dirs, filepath.Join(home, ".nix-profile", "bin"))
	}
	for _, d := range dirs {
		p := filepath.Join(d, name)
		if fi, err := os.Stat(p); err == nil && !fi.IsDir() && fi.Mode()&0o111 != 0 {
			return p
		}
	}
	return ""
}

func DeployedViaNix() bool {
	exe, err := os.Executable()
	if err != nil {
		return false
	}
	return strings.HasPrefix(exe, "/nix/store/")
}

func ProjectDir() string {
	exe, err := os.Executable()
	if err == nil {
		dir := filepath.Dir(exe)
		for i := 0; i < 6; i++ {
			if _, err := os.Stat(filepath.Join(dir, "go.mod")); err == nil {
				return dir
			}
			parent := filepath.Dir(dir)
			if parent == dir {
				break
			}
			dir = parent
		}
	}
	if wd, err := os.Getwd(); err == nil {
		return wd
	}
	return "."
}
