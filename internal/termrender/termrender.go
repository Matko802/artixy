package termrender

import (
	"bytes"
	"image"
	"image/color"
	"image/png"
	"strings"

	"golang.org/x/image/font"
	"golang.org/x/image/font/basicfont"
	"golang.org/x/image/math/fixed"

	"github.com/Matko802/artixy/internal/util"
)

const (
	TermCols = 120
	TermRows = 40
)

var (
	bgColor = color.RGBA{0x0b, 0x0e, 0x14, 0xff}
	fgColor = color.RGBA{0xe6, 0xe6, 0xe6, 0xff}
)

// RenderTextPNG renders stripped terminal text to a PNG using a built-in
// bitmap font. No external fonts needed — ideal for Raspberry Pi.
// It approximates the Rust renderer's dark theme and bottom-truncation.
func RenderTextPNG(caption, output string) []byte {
	face := basicfont.Face7x13
	charW, charH := 7, 13
	pad := 8

	// Strip ANSI/SGR + kitty graphics sequences for the Go renderer.
	text := util.StripSGR(stripKitty(output))
	text = strings.TrimRight(text, "\n")
	lines := strings.Split(text, "\n")

	// Keep last N lines that fit.
	maxLines := TermRows
	if len(lines) > maxLines {
		lines = lines[len(lines)-maxLines:]
	}
	// Cap line width.
	for i, l := range lines {
		r := []rune(l)
		if len(r) > TermCols {
			lines[i] = string(r[len(r)-TermCols:])
		}
		// Expand tabs.
		lines[i] = strings.ReplaceAll(lines[i], "\t", "        ")
	}

	capLines := []string{}
	if strings.TrimSpace(caption) != "" {
		capLines = append(capLines, "$ "+strings.TrimSpace(caption))
	}
	all := append(capLines, lines...)

	// Measure width.
	maxW := 0
	for _, l := range all {
		w := len([]rune(l)) * charW
		if w > maxW {
			maxW = w
		}
	}
	if maxW < 60*charW {
		maxW = 60 * charW
	}
	if maxW > TermCols*charW {
		maxW = TermCols * charW
	}
	w := maxW + pad*2
	h := len(all)*charH + pad*2
	if h < 12*charH {
		h = 12*charH + pad*2
	}

	img := image.NewRGBA(image.Rect(0, 0, w, h))
	for y := 0; y < h; y++ {
		for x := 0; x < w; x++ {
			img.Set(x, y, bgColor)
		}
	}
	d := &font.Drawer{Dst: img, Src: image.NewUniform(fgColor), Face: face}
	for i, l := range all {
		d.Dot = fixed.Point26_6{
			X: fixed.I(pad),
			Y: fixed.I(pad + i*charH + 11),
		}
		d.DrawString(l)
	}
	var buf bytes.Buffer
	if err := png.Encode(&buf, img); err != nil {
		return nil
	}
	return buf.Bytes()
}

func stripKitty(s string) string {
	// Remove ESC _ G ... ST kitty graphics sequences (best-effort).
	var out strings.Builder
	b := []byte(s)
	i := 0
	for i < len(b) {
		if b[i] == 0x1b && i+1 < len(b) && b[i+1] == '_' {
			// skip until BEL or ESC \
			j := i + 2
			for j < len(b) {
				if b[j] == 0x07 {
					j++
					break
				}
				if b[j] == 0x1b && j+1 < len(b) && b[j+1] == '\\' {
					j += 2
					break
				}
				j++
			}
			i = j
			continue
		}
		out.WriteByte(b[i])
		i++
	}
	return out.String()
}
