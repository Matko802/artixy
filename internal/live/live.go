package live

import (
	"encoding/base64"
	"fmt"
	"hash/fnv"
	"os"
	"strings"
	"sync"
	"time"

	"github.com/Matko802/artixy/internal/config"
	"github.com/Matko802/artixy/internal/scrub"
	"github.com/Matko802/artixy/internal/termrender"
	"github.com/Matko802/artixy/internal/util"
	"github.com/Matko802/artixy/internal/vm"
	"github.com/bwmarrin/discordgo"
)

type Entry struct {
	Cancel    context_Cancel
	Tag       uint64
	OutF      string
	CodeF     string
	Pid       *int64
	MsgID     string
	InF       *string
	AuthorID  string
	ChannelID string
}

type context_Cancel = func()

type Map struct {
	mu sync.Mutex
	m  map[string]*Entry // key channel/user
}

func NewMap() *Map { return &Map{m: map[string]*Entry{}} }

func key(channel, user string) string { return channel + "/" + user }

func (l *Map) Remove(channel, user string) *Entry {
	l.mu.Lock()
	defer l.mu.Unlock()
	k := key(channel, user)
	old := l.m[k]
	delete(l.m, k)
	if old != nil && old.Cancel != nil {
		old.Cancel()
	}
	return old
}

func (l *Map) RemoveIfTag(channel, user string, tag uint64) {
	l.mu.Lock()
	defer l.mu.Unlock()
	k := key(channel, user)
	if e, ok := l.m[k]; ok && e.Tag == tag {
		delete(l.m, k)
	}
}

func (l *Map) Insert(channel, user string, e *Entry) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.m[key(channel, user)] = e
}

func (l *Map) FindByMsg(channel, msgID string) *Entry {
	l.mu.Lock()
	defer l.mu.Unlock()
	for k, e := range l.m {
		if strings.HasPrefix(k, channel+"/") && e.MsgID == msgID {
			return e
		}
	}
	return nil
}

func (l *Map) SetPid(channel, user string, tag uint64, pid int64) {
	l.mu.Lock()
	defer l.mu.Unlock()
	if e, ok := l.m[key(channel, user)]; ok && e.Tag == tag {
		e.Pid = &pid
	}
}

const (
	livePoll         = 180 * time.Millisecond
	liveEditMin      = 2 * time.Second
	liveEditMaxFails = 5
	liveGuestMaxFail = 15
	liveFrameBytes   = "200000"
	guestPath        = "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH"
)

func BuildRunner(shell, b64, outF, codeF, input, runas string) string {
	homeCd := "cd \"$HOME\" 2>/dev/null || cd /tmp; "
	if runas != "" {
		homeCd = fmt.Sprintf("cd ~%s 2>/dev/null || cd \"$HOME\" 2>/dev/null || cd /tmp; ", runas)
	}
	// 120x40 to match renderer
	return fmt.Sprintf("export CMD_DATA=\"$(echo %s | base64 -d)\"; if command -v script >/dev/null 2>&1; then script -qec \"export TERM=xterm-256color TERM_PROGRAM=rustyterm COLORTERM=truecolor; stty cols 120 rows 40; %s -c 'export PATH=%s; %seval \\\"$CMD_DATA\\\"'\" /dev/null <> %s; else %s -c 'export TERM_PROGRAM=rustyterm COLORTERM=truecolor; export PATH=%s; %seval \"$CMD_DATA\"' <> %s; fi > %s 2>&1; echo $? > %s",
		b64, shell, guestPath, homeCd, input, shell, guestPath, homeCd, input, outF, codeF)
}

func MkfifoScript(path, runas string) string {
	if runas != "" {
		return fmt.Sprintf("rm -f %s && mkfifo -m 600 %s && chown %s %s", path, path, runas, path)
	}
	return fmt.Sprintf("rm -f %s && mkfifo -m 600 %s", path, path)
}

func CleanupLiveFiles(vmName, outF, codeF string, inF *string) {
	if inF != nil {
		_, _, _, _ = vm.GuestExec(vmName, "/bin/rm", []string{"-f", outF, codeF, *inF}, false, 10)
	} else {
		_, _, _, _ = vm.GuestExec(vmName, "/bin/rm", []string{"-f", outF, codeF}, false, 10)
	}
}

func CleanupStaleLiveFiles(vmName string) {
	_, _, _, _ = vm.GuestExec(vmName, "/bin/bash", []string{"-c", "rm -f /tmp/podbot-live-*; pkill -f 'podbot-live-' 2>/dev/null; true"}, false, 15)
}

const maxKeyRepeat = 100

func keyBase(word string) (string, bool) {
	lower := strings.ToLower(word)
	if strings.HasPrefix(lower, ";ctrl+") {
		suffix := lower[6:]
		if len(suffix) == 1 {
			ch := suffix[0]
			if ch < 'a' || ch > 'z' {
				return "", false
			}
			return string([]byte{ch - 'a' + 1}), true
		}
		switch suffix {
		case "return":
			return "\x7f", true
		case "space":
			return "\x00", true
		case "enter":
			return "\n", true
		case "esc":
			return "\x1b", true
		case "up":
			return "\x1b[1;5A", true
		case "down":
			return "\x1b[1;5B", true
		case "right":
			return "\x1b[1;5C", true
		case "left":
			return "\x1b[1;5D", true
		}
		return "", false
	}
	switch lower {
	case ";return":
		return "\x7f", true
	case ";space":
		return " ", true
	case ";enter":
		return "\r", true
	case ";esc":
		return "\x1b", true
	case ";up":
		return "\x1b[A", true
	case ";down":
		return "\x1b[B", true
	case ";right":
		return "\x1b[C", true
	case ";left":
		return "\x1b[D", true
	}
	return "", false
}

func expandLine(line string) string {
	words := strings.Fields(line)
	// We need to preserve spacing: simple approach — scan words with repeat support.
	var out strings.Builder
	rest := line
	i := 0
	for i < len(words) {
		w := words[i]
		// find w in rest
		idx := strings.Index(rest, w)
		if idx > 0 {
			out.WriteString(rest[:idx])
			rest = rest[idx:]
		}
		if k, ok := keyBase(w); ok {
			count := 1
			if i+1 < len(words) {
				var n int
				if _, err := fmt.Sscanf(words[i+1], "%d", &n); err == nil && n >= 1 && n <= maxKeyRepeat {
					// ensure the next token is purely numeric
					isNum := true
					for _, c := range words[i+1] {
						if c < '0' || c > '9' {
							isNum = false
							break
						}
					}
					if isNum {
						count = n
						rest = strings.TrimPrefix(rest, w)
						rest = strings.TrimPrefix(rest, " ")
						rest = strings.TrimPrefix(rest, words[i+1])
						i += 2
						for c := 0; c < count; c++ {
							out.WriteString(k)
						}
						continue
					}
				}
			}
			_ = count
			rest = strings.TrimPrefix(rest, w)
			out.WriteString(k)
			i++
			continue
		}
		out.WriteString(w)
		rest = strings.TrimPrefix(rest, w)
		i++
	}
	out.WriteString(rest)
	return out.String()
}

// ExpandTypedInput expands ;key sequences and \n escapes.
func ExpandTypedInput(text string) string {
	text = strings.ReplaceAll(text, "\\n", "\n")
	lines := strings.Split(text, "\n")
	for i, l := range lines {
		lines[i] = expandLine(l)
	}
	return strings.Join(lines, "\n")
}

func ForwardTerminalInput(vmName, fifo, runas, text string) bool {
	b64 := base64.StdEncoding.EncodeToString([]byte(text))
	inner := fmt.Sprintf("echo %s | base64 -d >> %s", b64, fifo)
	var script string
	if runas != "" {
		script = fmt.Sprintf("timeout 8 su %s -s /bin/bash -c '%s'", runas, inner)
	} else {
		script = fmt.Sprintf("timeout 8 bash -c '%s'", inner)
	}
	code, _, _, _ := vm.GuestExec(vmName, "/bin/bash", []string{"-c", script}, false, 15)
	if code != 0 {
		fmt.Fprintf(os.Stderr, "live: terminal input not delivered (rc %d)\n", code)
		return false
	}
	return true
}

// LiveDeps abstracts Discord I/O for testability.
type LiveDeps struct {
	Session   *discordgo.Session
	Data      *config.Data
	Live      *Map
	ScrubIP   bool
	ChannelID string
	MsgID     string
	AuthorID  string
	VM        string
	Cmd       string
	Runas     string
}

func editPosted(s *discordgo.Session, channelID, msgID, content string, files []*discordgo.File) bool {
	edit := &discordgo.MessageEdit{ID: msgID, Channel: channelID, Content: &content}
	if len(files) > 0 {
		// discordgo edit with files: re-send via channel message edit API supports files
		edit.Files = files
	} else {
		empty := []*discordgo.File{}
		_ = empty
	}
	_, err := s.ChannelMessageEditComplex(edit)
	return err == nil
}

func editCleared(s *discordgo.Session, channelID, msgID, content string) bool {
	_, err := s.ChannelMessageEditComplex(&discordgo.MessageEdit{
		ID: msgID, Channel: channelID, Content: &content,
	})
	return err == nil
}

func BeginRun(s *discordgo.Session, d *config.Data, lm *Map, channelID, msgID, authorID, authorName, vmName, cmd, runas string, scrubIP bool) {
	if runas != "" && !util.ValidRunas(runas) {
		s.ChannelMessageSend(channelID, util.PlainTail("linked linux account is invalid; ask the owner to re-add you."))
		return
	}
	tag := msgID // use discord msg id as tag
	rand := util.RandomSuffix()
	outF := fmt.Sprintf("/tmp/podbot-live-%s-%s.out", shortTag(tag), rand)
	codeF := fmt.Sprintf("/tmp/podbot-live-%s-%s.code", shortTag(tag), rand)
	inF := fmt.Sprintf("/tmp/podbot-live-%s-%s.in", shortTag(tag), rand)

	var inOpt *string
	if code, _, _, _ := vm.GuestExec(vmName, "/bin/bash", []string{"-c", MkfifoScript(inF, runas)}, false, 10); code == 0 {
		inOpt = &inF
	}
	if old := lm.Remove(channelID, authorID); old != nil {
		vmName2 := vmName
		go func() {
			if old.Pid != nil {
				vm.GuestKillTree(vmName2, *old.Pid)
			}
			CleanupLiveFiles(vmName2, old.OutF, old.CodeF, old.InF)
			editCleared(s, old.ChannelID, old.MsgID, util.Codeblock("This live session has been closed."))
		}()
	}
	done := make(chan struct{})
	var cancel func()
	// simple cancel via channel close flag
	cancelled := false
	cancel = func() { cancelled = true }
	_ = cancelled
	entry := &Entry{Cancel: cancel, Tag: hashTag(tag), OutF: outF, CodeF: codeF, MsgID: msgID, InF: inOpt, AuthorID: authorID, ChannelID: channelID}
	lm.Insert(channelID, authorID, entry)
	fmt.Fprintf(os.Stderr, "run started for %s (id %s)\n", authorName, authorID)
	go func() {
		liveRun(s, d, lm, done, channelID, msgID, authorID, hashTag(tag), vmName, cmd, runas, outF, codeF, inOpt, scrubIP)
	}()
}

func shortTag(s string) string {
	if len(s) > 12 {
		return s[len(s)-12:]
	}
	return s
}

func hashTag(s string) uint64 {
	h := fnv.New64a()
	h.Write([]byte(s))
	return h.Sum64()
}

func liveRun(s *discordgo.Session, d *config.Data, lm *Map, done chan struct{}, channelID, msgID, authorID string, tag uint64, vmName, cmd, runas, outF, codeF string, inF *string, scrubIP bool) {
	if runas != "" && !util.ValidRunas(runas) {
		editPosted(s, channelID, msgID, util.PlainTail("linked linux account is invalid; ask the owner to re-add you."), nil)
		lm.RemoveIfTag(channelID, authorID, tag)
		return
	}
	b64 := base64.StdEncoding.EncodeToString([]byte(cmd))
	input := "/dev/null"
	if inF != nil {
		input = *inF
	}
	script := BuildRunner("bash", b64, outF, codeF, input, runas)
	scriptSh := BuildRunner("sh", b64, outF, codeF, input, runas)
	var lpath string
	var largs []string
	if runas != "" {
		lpath = "su"
		largs = []string{"-", runas, "-s", "/bin/bash", "-c", script}
	} else {
		lpath = "/bin/bash"
		largs = []string{"-c", script}
	}
	pid, err := vm.GuestLaunchRaw(vmName, lpath, largs, false)
	if err != nil && strings.Contains(err.Error(), "No such file") {
		if runas != "" {
			pid, err = vm.GuestLaunchRaw(vmName, "su", []string{"-", runas, "-s", "/bin/sh", "-c", scriptSh}, false)
		} else {
			pid, err = vm.GuestLaunchRaw(vmName, "/bin/sh", []string{"-c", scriptSh}, false)
		}
	}
	if err != nil {
		editPosted(s, channelID, msgID, util.PlainTail(err.Error()), nil)
		CleanupLiveFiles(vmName, outF, codeF, inF)
		lm.RemoveIfTag(channelID, authorID, tag)
		return
	}
	lm.SetPid(channelID, authorID, tag, pid)

	first := true
	var lastEdit *time.Time
	now := time.Now()
	lastEdit = &now
	editFails := 0
	guestFails := 0
	var postedHash *uint64

	for {
		time.Sleep(livePoll)
		fetched := ""
		if _, out, _, err := vm.GuestExec(vmName, "/usr/bin/tail", []string{"-c", liveFrameBytes, outF}, true, 15); err != nil {
			guestFails++
			fmt.Fprintf(os.Stderr, "live: guest fetch failed (%d/%d): %v\n", guestFails, liveGuestMaxFail, err)
			if guestFails >= liveGuestMaxFail {
				s.ChannelMessageSend(channelID, util.PlainTail(fmt.Sprintf("$ %s\n…live updates stopped: %s", cmd, "guest agent stopped answering")))
				editCleared(s, channelID, msgID, util.Codeblock("This live session has been closed."))
				CleanupLiveFiles(vmName, outF, codeF, inF)
				break
			}
			first = false
			time.Sleep(2 * time.Second)
			continue
		} else {
			fetched = out
		}
		// GuestExec returns error on failure; detect via empty + retry counter is best-effort.
		if scrubIP {
			fetched = scrub.ScrubPublicIP(fetched)
		}
		doneCode, err := vm.GuestStatus(vmName, pid)
		if err != nil {
			guestFails++
			fmt.Fprintf(os.Stderr, "live: guest status failed (%d/%d): %v\n", guestFails, liveGuestMaxFail, err)
			if guestFails >= liveGuestMaxFail {
				s.ChannelMessageSend(channelID, util.PlainTail(fmt.Sprintf("$ %s\n…live updates stopped: %s", cmd, "guest agent stopped answering")))
				editCleared(s, channelID, msgID, util.Codeblock("This live session has been closed."))
				CleanupLiveFiles(vmName, outF, codeF, inF)
				break
			}
			first = false
			time.Sleep(2 * time.Second)
			continue
		}
		guestFails = 0
		if doneCode != nil {
			code := *doneCode
			_, full, _, _ := vm.GuestExec(vmName, "/usr/bin/tail", []string{"-c", "500000", outF}, true, 30)
			if full == "" {
				full = fetched
			}
			if scrubIP {
				full = scrub.ScrubPublicIP(full)
			}
			header := fmt.Sprintf("$ %s", cmd)
			output := strings.TrimRight(full, "\n")
			if code != 0 {
				if output != "" {
					output += fmt.Sprintf("\nexit %d", code)
				} else {
					output = fmt.Sprintf("exit %d", code)
				}
			}
			if first {
				combined := header
				if strings.TrimSpace(output) != "" {
					combined += "\n" + strings.TrimRight(output, "\n")
				}
				isLong := len([]rune(combined)) > 1800
				var posted bool
				if isLong {
					if png := termrender.RenderTextPNG(cmd, output); png != nil {
						posted = editPosted(s, channelID, msgID, header, []*discordgo.File{{Name: "live.png", Reader: bytesReader(png)}})
					} else {
						posted = editPosted(s, channelID, msgID, util.PlainTail(combined), nil)
					}
				} else {
					posted = editPosted(s, channelID, msgID, util.PlainTail(combined), nil)
				}
				if !posted {
					s.ChannelMessageSend(channelID, util.PlainTail(fmt.Sprintf("$ %s\n…live updates stopped: %s", cmd, "Discord kept rejecting message edits")))
				}
			} else if code == 0 && postedHash != nil {
				editCleared(s, channelID, msgID, util.Codeblock("This live session has been closed."))
			} else {
				var posted bool
				if png := termrender.RenderTextPNG(cmd, output); png != nil && len([]rune(output)) > 800 {
					posted = editPosted(s, channelID, msgID, header, []*discordgo.File{{Name: "live.png", Reader: bytesReader(png)}})
				} else {
					combined := header
					if strings.TrimSpace(output) != "" {
						combined += "\n" + strings.TrimRight(output, "\n")
					}
					posted = editPosted(s, channelID, msgID, util.PlainTail(combined), nil)
				}
				if !posted {
					s.ChannelMessageSend(channelID, util.PlainTail(fmt.Sprintf("$ %s\n…live updates stopped: %s", cmd, "Discord kept rejecting message edits")))
				}
			}
			CleanupLiveFiles(vmName, outF, codeF, inF)
			break
		}
		// still running: throttle edits
		h := fnv.New64a()
		h.Write([]byte(fetched))
		digest := h.Sum64()
		if postedHash != nil && *postedHash == digest {
			first = false
			continue
		}
		if lastEdit != nil && time.Since(*lastEdit) < liveEditMin {
			first = false
			continue
		}
		var text string
		var files []*discordgo.File
		if png := termrender.RenderTextPNG(cmd, fetched); png != nil && len([]rune(fetched)) > 800 {
			text = fmt.Sprintf("$ %s", cmd)
			files = []*discordgo.File{{Name: "live.png", Reader: bytesReader(png)}}
		} else {
			combined := fmt.Sprintf("$ %s", cmd)
			if strings.TrimSpace(fetched) != "" {
				combined += "\n" + strings.TrimRight(fetched, "\n")
			}
			text = util.PlainTail(combined)
		}
		if editPosted(s, channelID, msgID, text, files) {
			t := time.Now()
			lastEdit = &t
			editFails = 0
			postedHash = &digest
		} else {
			editFails++
			t := time.Now()
			lastEdit = &t
			fmt.Fprintf(os.Stderr, "live: edit %d/%d failed\n", editFails, liveEditMaxFails)
			if editFails >= liveEditMaxFails {
				s.ChannelMessageSend(channelID, util.PlainTail(fmt.Sprintf("$ %s\n…live updates stopped: %s", cmd, "Discord kept rejecting message edits")))
				editCleared(s, channelID, msgID, util.Codeblock("This live session has been closed."))
				CleanupLiveFiles(vmName, outF, codeF, inF)
				break
			}
		}
		first = false
	}
	lm.RemoveIfTag(channelID, authorID, tag)
}
