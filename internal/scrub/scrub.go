package scrub

import "strings"

func PublicIPv4(o [4]byte) bool {
	switch {
	case o[0] == 10:
		return false
	case o[0] == 172 && o[1] >= 16 && o[1] <= 31:
		return false
	case o[0] == 192 && o[1] == 168:
		return false
	case o[0] == 127:
		return false
	case o[0] == 169 && o[1] == 254:
		return false
	case o[0] == 100 && o[1] >= 64 && o[1] <= 127:
		return false
	case o[0] == 0:
		return false
	case o == [4]byte{255, 255, 255, 255}:
		return false
	default:
		return true
	}
}

func scanIPv4(b []byte, i int) ([4]byte, int, bool) {
	var o [4]byte
	if i > 0 {
		p := b[i-1]
		if isAlnum(p) || p == '.' {
			return o, 0, false
		}
	}
	j := i
	for k := 0; k < 4; k++ {
		start := j
		for j < len(b) && b[j] >= '0' && b[j] <= '9' {
			j++
		}
		l := j - start
		if l == 0 || l > 3 {
			return o, 0, false
		}
		v := 0
		for _, d := range b[start:j] {
			v = v*10 + int(d-'0')
		}
		if v > 255 {
			return o, 0, false
		}
		o[k] = byte(v)
		if k < 3 {
			if j >= len(b) || b[j] != '.' {
				return o, 0, false
			}
			j++
		}
	}
	if j < len(b) {
		if b[j] >= '0' && b[j] <= '9' {
			return o, 0, false
		}
		if b[j] == '.' && j+1 < len(b) && b[j+1] >= '0' && b[j+1] <= '9' {
			return o, 0, false
		}
	}
	return o, j, true
}

func isAlnum(c byte) bool {
	return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9')
}

func utf8Len(first byte) int {
	switch {
	case first <= 0x7F:
		return 1
	case first >= 0xC0 && first <= 0xDF:
		return 2
	case first >= 0xE0 && first <= 0xEF:
		return 3
	default:
		return 4
	}
}

func parseQuad(s string) ([4]byte, bool) {
	var o [4]byte
	p := strings.Split(s, ".")
	if len(p) != 4 {
		return o, false
	}
	for k, g := range p {
		if g == "" || len(g) > 3 {
			return o, false
		}
		v := 0
		for i := 0; i < len(g); i++ {
			if g[i] < '0' || g[i] > '9' {
				return o, false
			}
			v = v*10 + int(g[i]-'0')
		}
		if v > 255 {
			return o, false
		}
		o[k] = byte(v)
	}
	return o, true
}

func validGroups(parts []string) bool {
	for _, p := range parts {
		if p == "" || len(p) > 4 {
			return false
		}
		for i := 0; i < len(p); i++ {
			c := p[i]
			if !((c >= '0' && c <= '9') || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F')) {
				return false
			}
		}
	}
	return true
}

func classifyIPv6(tok string) (bool, bool) {
	head := tok
	var tail *[4]byte
	if strings.Contains(tok, ".") {
		cut := strings.LastIndex(tok, ":")
		if cut < 0 {
			return false, false
		}
		q, ok := parseQuad(tok[cut+1:])
		if !ok {
			return false, false
		}
		head = tok[:cut]
		tail = &q
	}
	if strings.Count(head, "::") > 1 {
		return false, false
	}
	hasDbl := strings.Contains(head, "::")
	need := 0
	if tail != nil {
		need = 2
	}
	var explicit []string
	if hasDbl {
		p := strings.Index(head, "::")
		var l, r []string
		if head[:p] != "" {
			l = strings.Split(head[:p], ":")
		}
		if head[p+2:] != "" {
			r = strings.Split(head[p+2:], ":")
		}
		if !validGroups(l) || !validGroups(r) {
			return false, false
		}
		if len(l)+len(r)+need > 7 {
			return false, false
		}
		explicit = append(append([]string{}, l...), r...)
	} else {
		g := strings.Split(head, ":")
		if !validGroups(g) || len(g)+need != 8 {
			return false, false
		}
		explicit = g
	}
	vals := make([]uint16, len(explicit))
	for i, g := range explicit {
		var v uint64
		for j := 0; j < len(g); j++ {
			c := g[j]
			var d uint64
			switch {
			case c >= '0' && c <= '9':
				d = uint64(c - '0')
			case c >= 'a' && c <= 'f':
				d = uint64(c-'a') + 10
			case c >= 'A' && c <= 'F':
				d = uint64(c-'A') + 10
			}
			v = v*16 + d
		}
		vals[i] = uint16(v)
	}
	allZero := true
	for _, v := range vals {
		if v != 0 {
			allZero = false
			break
		}
	}
	if allZero {
		if tail != nil {
			return true, PublicIPv4(*tail)
		}
		return true, false
	}
	// loopback ::1
	nonZero := vals
	for len(nonZero) > 0 && nonZero[0] == 0 {
		nonZero = nonZero[1:]
	}
	if len(nonZero) == 1 && nonZero[0] == 1 {
		return true, false
	}
	if len(vals) > 0 {
		g0 := vals[0]
		if (g0 >= 0xfe80 && g0 <= 0xfebf) || (g0 >= 0xfc00 && g0 <= 0xfdff) {
			return true, false
		}
	}
	return true, true
}

func scanIPv6(s string, i int) (int, bool, bool) {
	b := []byte(s)
	c := b[i]
	if !(isHex(c) || c == ':') {
		return 0, false, false
	}
	if i > 0 {
		p := b[i-1]
		if isHex(p) || p == ':' || p == '.' {
			return 0, false, false
		}
	}
	j := i
	colons := 0
	for j < len(b) && (isHex(b[j]) || b[j] == ':' || b[j] == '.') {
		if b[j] == ':' {
			colons++
		}
		j++
	}
	if colons < 2 {
		return 0, false, false
	}
	tok := s[i:j]
	for strings.HasSuffix(tok, ":") && !strings.HasSuffix(tok, "::") {
		tok = tok[:len(tok)-1]
		j--
	}
	ok, public := classifyIPv6(tok)
	if !ok {
		return 0, false, false
	}
	return j, public, true
}

func isHex(c byte) bool {
	return (c >= '0' && c <= '9') || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F')
}

// ScrubPublicIP replaces public IPv4/IPv6 literals with [redacted].
func ScrubPublicIP(s string) string {
	b := []byte(s)
	var out strings.Builder
	out.Grow(len(s))
	i := 0
	for i < len(b) {
		c := b[i]
		if isHex(c) || c == ':' {
			if end, public, ok := scanIPv6(s, i); ok {
				if public {
					out.WriteString("[redacted]")
				} else {
					out.WriteString(s[i:end])
				}
				i = end
				continue
			}
		}
		if c >= '0' && c <= '9' {
			if o, end, ok := scanIPv4(b, i); ok {
				if PublicIPv4(o) {
					out.WriteString("[redacted]")
				} else {
					out.WriteString(s[i:end])
				}
				i = end
				continue
			}
		}
		l := utf8Len(b[i])
		if i+l > len(b) {
			l = len(b) - i
		}
		out.WriteString(s[i : i+l])
		i += l
	}
	return out.String()
}
