/* Terminal-to-PNG renderer: libvterm grid + stb_truetype + stb_image_write. */
#include "termrender.h"
#include "util.h"

#include <pthread.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <vterm.h>

#include "stb_image_write.h"
#include "stb_truetype.h"

#define FONT_PX 16.0f
#define PAD 6
#define ATLAS_W 512
#define ATLAS_H 512
#define ATLAS_FIRST 32
#define ATLAS_COUNT 95

static const unsigned char BG[3] = { 0x0b, 0x0e, 0x14 };
static const unsigned char FG[3] = { 0xe6, 0xe6, 0xe6 };

typedef struct {
    stbtt_fontinfo info;
    unsigned char *file_data; /* owned */
    stbtt_bakedchar glyphs[ATLAS_COUNT];
    unsigned char *atlas; /* owned, ATLAS_W*ATLAS_H */
    int cell_w;
    int cell_h;
    int ascent;
    bool ok;
} term_font_t;

static term_font_t g_regular = { 0 };
static term_font_t g_bold = { 0 };
static pthread_once_t g_font_once = PTHREAD_ONCE_INIT;

/* 16-color table + 256-color cube + grayscale (ported 1:1 from termrender.rs). */
static void indexed_color(unsigned char i, unsigned char out[3]) {
    if (i < 16) {
        static const unsigned char tab[16][3] = {
            { 0x00, 0x00, 0x00 }, { 0xcd, 0x00, 0x00 },
            { 0x00, 0xcd, 0x00 }, { 0xcd, 0xcd, 0x00 },
            { 0x00, 0x00, 0xee }, { 0xcd, 0x00, 0xcd },
            { 0x00, 0xcd, 0xcd }, { 0xe5, 0xe5, 0xe5 },
            { 0x7f, 0x7f, 0x7f }, { 0xff, 0x00, 0x00 },
            { 0x00, 0xff, 0x00 }, { 0xff, 0xff, 0x00 },
            { 0x5c, 0x5c, 0xff }, { 0xff, 0x00, 0xff },
            { 0x00, 0xff, 0xff }, { 0xff, 0xff, 0xff },
        };
        memcpy(out, tab[i], 3);
        return;
    }
    if (i < 232) {
        static const unsigned char levels[6] = { 0x00, 0x5f, 0x87,
                                                 0xaf, 0xd7, 0xff };
        unsigned char k = (unsigned char)(i - 16);
        out[0] = levels[k / 36];
        out[1] = levels[(k / 6) % 6];
        out[2] = levels[k % 6];
        return;
    }
    {
        unsigned char v = (unsigned char)(8 + (i - 232) * 10);
        out[0] = out[1] = out[2] = v;
    }
}

static void resolve_color(const VTermColor *c, bool is_fg, bool bold,
                          unsigned char out[3]) {
    if (VTERM_COLOR_IS_INDEXED(c)) {
        unsigned idx = c->indexed.idx;
        /* bold brightens the low 8 (matches Rust brighten()) */
        if (bold && is_fg && idx < 8)
            idx += 8;
        indexed_color((unsigned char)idx, out);
        return;
    }
    if (VTERM_COLOR_IS_RGB(c)) {
        out[0] = c->rgb.red;
        out[1] = c->rgb.green;
        out[2] = c->rgb.blue;
        return;
    }
    memcpy(out, is_fg ? FG : BG, 3);
}

/* NOTE: libvterm 0.3 exposes no dim attribute, so SGR-dim text renders
 * at full brightness (documented simplification vs the Rust build). */

/* Locate a monospace font file for a fontconfig spec. */
static bool find_font_file(const char *spec, bool want_bold, char *out,
                           size_t n) {
    /* $ARTIXY_FONT(_BOLD) override wins */
    const char *env = getenv(want_bold ? "ARTIXY_FONT_BOLD" : "ARTIXY_FONT");
    if (env && *env) {
        snprintf(out, n, "%s", env);
        return true;
    }
    char cmd[256];
    snprintf(cmd, sizeof cmd, "fc-match '%s' --format=%%{file} 2>/dev/null", spec);
    FILE *p = popen(cmd, "r");
    if (p) {
        size_t len = 0;
        int ch;
        while ((ch = fgetc(p)) != EOF && ch != '\n' && len + 1 < n)
            out[len++] = (char)ch;
        out[len] = '\0';
        int rc = pclose(p);
        if (rc == 0 && len > 0)
            return true;
    }
    static const char *fallbacks[] = {
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/dejavu/DejaVuSansMono.ttf",
        NULL
    };
    for (size_t i = 0; fallbacks[i]; i++) {
        FILE *f = fopen(fallbacks[i], "rb");
        if (f) {
            fclose(f);
            snprintf(out, n, "%s", fallbacks[i]);
            return true;
        }
    }
    return false;
}

static bool font_load(term_font_t *f, const char *path) {
    memset(f, 0, sizeof *f);
    char *data = NULL;
    size_t len = 0;
    if (read_file(path, &data, &len) != 0 || !len)
        return false;
    if (!stbtt_InitFont(&f->info, (unsigned char *)data, 0)) {
        free(data);
        return false;
    }
    f->file_data = (unsigned char *)data;
    f->atlas = xmalloc(ATLAS_W * ATLAS_H);
    if (!f->atlas) {
        free(data);
        memset(f, 0, sizeof *f);
        return false;
    }
    int baked = stbtt_BakeFontBitmap(f->file_data, 0, FONT_PX, f->atlas,
                                     ATLAS_W, ATLAS_H, ATLAS_FIRST,
                                     ATLAS_COUNT, f->glyphs);
    if (baked <= 0) {
        free(f->atlas);
        free(f->file_data);
        memset(f, 0, sizeof *f);
        return false;
    }
    float scale = stbtt_ScaleForPixelHeight(&f->info, FONT_PX);
    int ascent, descent, gap;
    stbtt_GetFontVMetrics(&f->info, &ascent, &descent, &gap);
    f->ascent = (int)(ascent * scale + 0.5f);
    int desc = (int)(-descent * scale + 0.5f);
    f->cell_h = f->ascent + desc + 4;
    if (f->cell_h < 1)
        f->cell_h = 1;
    int adv, lsb;
    stbtt_GetCodepointHMetrics(&f->info, 'M', &adv, &lsb);
    f->cell_w = (int)(adv * scale + 0.5f);
    if (f->cell_w < 1)
        f->cell_w = 1;
    f->ok = true;
    return true;
}

static void fonts_init_once(void) {
    char reg[1024], bold[1024];
    if (find_font_file("DejaVu Sans Mono", false, reg, sizeof reg))
        font_load(&g_regular, reg);
    if (find_font_file("DejaVu Sans Mono:weight=bold", true, bold,
                       sizeof bold)) {
        if (!font_load(&g_bold, bold) && g_regular.ok)
            g_bold = g_regular; /* share buffers; fonts live forever */
    } else if (g_regular.ok) {
        g_bold = g_regular; /* share buffers; fonts live forever */
    }
}

unsigned char *termrender_strip_kitty(const unsigned char *in, size_t len,
                                      size_t *out_len) {
    size_t cap = len + 1;
    unsigned char *out = xmalloc(cap);
    if (!out)
        return NULL;
    size_t w = 0, i = 0;
    while (i < len) {
        if (in[i] == 0x1b && i + 1 < len && in[i + 1] == '_') {
            size_t j = i + 2;
            while (j < len) {
                if (in[j] == 0x07) {
                    j++;
                    break;
                }
                if (in[j] == 0x1b && j + 1 < len && in[j + 1] == '\\') {
                    j += 2;
                    break;
                }
                j++;
            }
            i = j;
            continue;
        }
        out[w++] = in[i++];
    }
    out[w] = '\0';
    if (out_len)
        *out_len = w;
    return out;
}

typedef struct {
    unsigned char *p;
    size_t n, cap;
} png_buf_t;

static void png_write_cb(void *ud, void *data, int len) {
    png_buf_t *b = ud;
    if (len <= 0)
        return;
    if (b->n + (size_t)len + 1 > b->cap) {
        size_t nc = (b->n + (size_t)len + 1) * 2;
        unsigned char *np = xrealloc(b->p, nc);
        if (!np)
            return;
        b->p = np;
        b->cap = nc;
    }
    memcpy(b->p + b->n, data, (size_t)len);
    b->n += (size_t)len;
}

static void blit_glyph(unsigned char *img, int w, int h, int x0, int y0,
                       int bw, int bh, const unsigned char *atlas,
                       int stride, int ax, int ay, const unsigned char fg[3]) {
    if (bw <= 0 || bh <= 0 || !atlas)
        return;
    for (int y = 0; y < bh; y++) {
        for (int x = 0; x < bw; x++) {
            unsigned a = atlas[(ay + y) * stride + (ax + x)];
            if (!a)
                continue;
            int bx = x0 + x, by = y0 + y;
            if (bx < 0 || by < 0 || bx >= w || by >= h)
                continue;
            unsigned char *p = img + ((size_t)by * (size_t)w + (size_t)bx) * 3;
            for (int c = 0; c < 3; c++)
                p[c] = (unsigned char)((fg[c] * a + p[c] * (255 - a)) / 255);
        }
    }
}

unsigned char *termrender_png(const unsigned char *output, size_t len,
                              size_t *png_len) {
    *png_len = 0;
    pthread_once(&g_font_once, fonts_init_once);
    if (!g_regular.ok || !g_regular.atlas)
        return NULL;

    size_t clean_len = 0;
    unsigned char *clean =
        termrender_strip_kitty(output ? output : (unsigned char *)"", len,
                               &clean_len);
    if (!clean)
        return NULL;

    VTerm *vt = vterm_new(TERM_ROWS, TERM_COLS);
    if (!vt) {
        free(clean);
        return NULL;
    }
    vterm_set_utf8(vt, 1);
    VTermScreen *screen = vterm_obtain_screen(vt);
    vterm_screen_reset(screen, 1);
    if (clean_len)
        vterm_input_write(vt, (const char *)clean, clean_len);
    free(clean);

    int cw = g_regular.cell_w, chh = g_regular.cell_h;
    int W = TERM_COLS * cw + PAD * 2;
    int H = TERM_ROWS * chh + PAD * 2;
    unsigned char *img = xmalloc((size_t)W * (size_t)H * 3);
    if (!img) {
        vterm_free(vt);
        return NULL;
    }
    for (size_t i = 0; i < (size_t)W * (size_t)H; i++) {
        img[i * 3] = BG[0];
        img[i * 3 + 1] = BG[1];
        img[i * 3 + 2] = BG[2];
    }

    for (int r = 0; r < TERM_ROWS; r++) {
        for (int col = 0; col < TERM_COLS; col++) {
            VTermPos pos = { r, col };
            VTermScreenCell cell;
            if (!vterm_screen_get_cell(screen, pos, &cell))
                continue;
            if (cell.width == 0)
                continue; /* wide-char spacer */
            bool bold = cell.attrs.bold;
            term_font_t *font = (bold && g_bold.ok && g_bold.atlas) ? &g_bold
                                                                   : &g_regular;
            unsigned char fg[3], bg[3];
            resolve_color(&cell.fg, true, bold, fg);
            resolve_color(&cell.bg, false, false, bg);
            if (cell.attrs.reverse) {
                unsigned char t[3];
                memcpy(t, fg, 3);
                memcpy(fg, bg, 3);
                memcpy(bg, t, 3);
            }
            if (cell.attrs.conceal)
                memcpy(fg, bg, 3);
            int cx = col * cw + PAD;
            int cy = r * chh + PAD;
            if (memcmp(bg, BG, 3) != 0) {
                for (int y = 0; y < chh; y++) {
                    for (int x = 0; x < cw; x++) {
                        int bx = cx + x, by = cy + y;
                        if (bx < 0 || by < 0 || bx >= W || by >= H)
                            continue;
                        unsigned char *p =
                            img + ((size_t)by * (size_t)W + (size_t)bx) * 3;
                        p[0] = bg[0];
                        p[1] = bg[1];
                        p[2] = bg[2];
                    }
                }
            }
            uint32_t ch = cell.chars[0];
            if (ch == ' ' || ch < 32 || ch == 127)
                continue;
            if (ch >= 127)
                continue; /* outside baked atlas; skip (blank) */
            stbtt_bakedchar *g = &font->glyphs[ch - ATLAS_FIRST];
            int gw = g->x1 - g->x0, gh = g->y1 - g->y0;
            int gx = cx + g->xoff;
            int gy = cy + font->ascent + g->yoff;
            blit_glyph(img, W, H, gx, gy, gw, gh, font->atlas, ATLAS_W, g->x0,
                       g->y0, fg);
        }
    }
    /* cursor bar */
    {
        VTermState *state = vterm_obtain_state(vt);
        VTermPos cur = { 0, 0 };
        vterm_state_get_cursorpos(state, &cur);
        if (cur.row >= 0 && cur.row < TERM_ROWS && cur.col >= 0 &&
            cur.col < TERM_COLS) {
            int cx = cur.col * cw + PAD + 1;
            int cy = cur.row * chh + PAD + 2;
            for (int y = 0; y < chh - 4; y++) {
                for (int x = 0; x < 2; x++) {
                    int bx = cx + x, by = cy + y;
                    if (bx < 0 || by < 0 || bx >= W || by >= H)
                        continue;
                    unsigned char *p =
                        img + ((size_t)by * (size_t)W + (size_t)bx) * 3;
                    p[0] = FG[0];
                    p[1] = FG[1];
                    p[2] = FG[2];
                }
            }
        }
    }
    vterm_free(vt);

    png_buf_t pb = { NULL, 0, 0 };
    stbi_write_png_to_func(png_write_cb, &pb, W, H, 3, img,
                           (int)((size_t)W * 3));
    free(img);
    if (!pb.p || !pb.n) {
        free(pb.p);
        return NULL;
    }
    *png_len = pb.n;
    return pb.p;
}
