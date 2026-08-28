#!/usr/bin/env python3
"""Omacastnotes: the floating markdown notes window used by the Omarchycast launcher.

Run with no argument for the scratchpad; pass a file path to open that note.
Formerly known as shadow-notes."""

from __future__ import annotations

import base64
import hashlib
import json
import os
import re
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path

import gi

gi.require_version("Gtk", "3.0")
gi.require_version("Gdk", "3.0")
gi.require_version("GdkPixbuf", "2.0")
gi.require_version("WebKit2", "4.1")

from gi.repository import Gdk, GdkPixbuf, Gio, GLib, Gtk, WebKit2  # noqa: E402

APP_ID = "io.github.aditya-raj-tiwari.omacastnotes"
APP_DIR = Path.home() / ".local" / "share" / "omacastnotes"
LEGACY_DIR = Path.home() / ".local" / "share" / "shadow-notes"
UI_DIR = APP_DIR / "ui"
INDEX = UI_DIR / "index.html"
NOTES_PATH = APP_DIR / "data" / "notes.md"
GEO_PATH = APP_DIR / "data" / "geometry.json"
THEME_PATH = Path.home() / ".local" / "state" / "omarchy" / "current" / "theme" / "colors.toml"
DEFAULT_SIZE = (420, 510)
MIN_SIZE = (300, 360)
API_URL = "https://api.x.ai/v1/responses"
CHAT_URL = "https://api.x.ai/v1/chat/completions"
MODEL = "grok-4.6"

MATH_PROMPT = """Turn the input into clean lecture notes in GitHub-flavored Markdown.

This is a small notes editor. The result must be readable as a few flowing sentences.

Hard rules:
- NEVER put one symbol per line. NEVER output a column of single letters/braces.
- Reconstruct Wikipedia/MathML/OCR garbage (split glyphs, duplicated unicode+ascii) into proper maths.
- Use $...$ inline and $$...$$ for display. Subscripts as $\\Omega_1$, $\\Omega_2$. Empty set $\\emptyset$.
- Complements: $A^{c}$ with braces around the whole complement. $A^(c)$ becomes $A^{c}$.
- Sets on one line, e.g. $B=\\{\\emptyset,\\{a\\},\\{b\\},\\{a,b\\}\\}$.
- $\\sigma$-algebra, $\\mathcal{A}$, $\\mathbb{N}$, $\\cup$, $\\cap$, $\\setminus$, $\\in$.
- Do not repeat a formula twice (no unicode version then ascii version).
- Do not wrap the whole answer in a markdown fence. No commentary.
- Tables: one row per line, blank line before the table, separator | --- | --- |.

Example output:
Take $\\Omega_1=\\{1,2,3\\}$, $\\Omega_2=\\{a,b\\}$, with $f(1)=f(2)=a$ and $f(3)=b$. Let $B=\\{\\emptyset,\\{a\\},\\{b\\},\\{a,b\\}\\}$, which is a $\\sigma$-algebra on $\\Omega_2$.
"""


def ensure_dirs() -> None:
    NOTES_PATH.parent.mkdir(parents=True, exist_ok=True)
    # Migrate data written under the app's previous name, shadow-notes.
    for name in ("notes.md", "geometry.json"):
        new = NOTES_PATH.parent / name
        old = LEGACY_DIR / "data" / name
        if not new.exists() and old.is_file():
            try:
                new.write_bytes(old.read_bytes())
            except OSError:
                pass


def load_notes() -> str:
    try:
        return NOTES_PATH.read_text(encoding="utf-8")
    except FileNotFoundError:
        return ""


def save_notes(text: str) -> None:
    ensure_dirs()
    tmp = NOTES_PATH.with_suffix(".md.tmp")
    tmp.write_text(text, encoding="utf-8")
    tmp.replace(NOTES_PATH)


def load_geometry() -> tuple[int, int]:
    try:
        data = json.loads(GEO_PATH.read_text(encoding="utf-8"))
        width, height = int(data["width"]), int(data["height"])
        if width >= MIN_SIZE[0] and height >= MIN_SIZE[1]:
            return width, height
    except (OSError, ValueError, KeyError, TypeError, json.JSONDecodeError):
        pass
    return DEFAULT_SIZE


def save_geometry(width: int, height: int) -> None:
    if width < MIN_SIZE[0] or height < MIN_SIZE[1]:
        return
    ensure_dirs()
    GEO_PATH.write_text(
        json.dumps({"width": int(width), "height": int(height)}) + "\n",
        encoding="utf-8",
    )


def parse_colors_toml(path: Path) -> dict[str, str]:
    colors: dict[str, str] = {}
    if not path.is_file():
        return colors
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        value = value.strip().strip('"').strip("'")
        if value.startswith("#"):
            colors[key.strip()] = value
    return colors


def theme_payload() -> dict[str, str]:
    colors = parse_colors_toml(THEME_PATH)
    bg = colors.get("background", "#1a1b26")
    fg = colors.get("bright_foreground") or colors.get("foreground", "#c0caf5")
    muted = colors.get("dark_foreground") or colors.get("muted", "#565f89")
    accent = colors.get("blue") or colors.get("accent", "#7aa2f7")
    return {
        "bg": bg,
        "fg": fg,
        "muted": muted,
        "accent": accent,
        "card": bg,
    }


def load_token() -> str | None:
    env = os.environ.get("XAI_API_KEY")
    if env:
        return env.strip()
    auth_path = Path.home() / ".grok" / "auth.json"
    try:
        data = json.loads(auth_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    if not isinstance(data, dict):
        return None
    best = None
    for entry in data.values():
        if not isinstance(entry, dict):
            continue
        key = entry.get("key")
        if key:
            best = key
    return best


def http_json(url: str, token: str, payload: dict, timeout: int = 20) -> dict:
    req = urllib.request.Request(
        url,
        data=json.dumps(payload).encode("utf-8"),
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json",
            "User-Agent": "shadow-notes/1.0",
        },
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode("utf-8"))


def extract_text(data: dict) -> str:
    if not isinstance(data, dict):
        return ""
    if isinstance(data.get("output_text"), str) and data["output_text"].strip():
        return data["output_text"]
    chunks: list[str] = []
    output = data.get("output")
    if isinstance(output, list):
        for item in output:
            if not isinstance(item, dict):
                continue
            content = item.get("content")
            if isinstance(content, list):
                for part in content:
                    if isinstance(part, dict) and part.get("type") in {"output_text", "text"}:
                        text = part.get("text") or part.get("value")
                        if text:
                            chunks.append(str(text))
            elif isinstance(item.get("text"), str):
                chunks.append(item["text"])
    if chunks:
        return "\n".join(chunks)
    choices = data.get("choices")
    if isinstance(choices, list) and choices:
        msg = choices[0].get("message") if isinstance(choices[0], dict) else None
        if isinstance(msg, dict):
            content = msg.get("content")
            if isinstance(content, str):
                return content
    return ""


def strip_fence(text: str) -> str:
    text = text.strip()
    match = re.match(r"^```(?:markdown|md|latex)?\s*\n([\s\S]*?)\n```$", text)
    if match:
        return match.group(1).strip()
    return text


_TABLE_SEP = re.compile(r"^\s*\|?\s*:?-{3,}:?\s*(\|\s*:?-{3,}:?\s*)+\|?\s*$")


def _table_row(line: str) -> bool:
    t = line.strip()
    return "|" in t and (t.startswith("|") or t.count("|") >= 2)


def _tidy_row(line: str) -> str:
    t = re.sub(r"\s+", " ", line).strip()
    if not t.startswith("|"):
        t = "| " + t
    if not t.endswith("|"):
        t = t + " |"
    t = re.sub(r"\s*\|\s*", " | ", t)
    t = re.sub(r"^\s\|", "|", t)
    t = re.sub(r"\|\s$", "|", t)
    return t


def normalize_markdown_tables(md: str) -> str:
    lines = md.split("\n")
    out: list[str] = []
    i = 0
    n = len(lines)
    while i < n:
        nxt = lines[i + 1].strip() if i + 1 < n else ""
        if _table_row(lines[i]) and _TABLE_SEP.match(nxt):
            if out and out[-1].strip() != "":
                out.append("")
            block = [lines[i].strip(), nxt]
            i += 2
            acc = ""
            while i < n:
                t = lines[i].strip()
                if t == "":
                    break
                if t.startswith("#") or (t[:2] in {"- ", "* ", "+ "} or re.match(r"^\d+\.\s", t)):
                    if not _table_row(t):
                        break
                if not acc:
                    acc = t
                elif t == "|":
                    acc = acc.rstrip() + " |"
                elif t.startswith("|") and (acc.endswith("|") or _TABLE_SEP.match(acc)):
                    block.append(acc)
                    acc = t
                else:
                    extra = t[1:].strip() if t.startswith("|") else t
                    acc = (acc.rstrip(" |") + " " + extra).strip()
                    if t.endswith("|") and not acc.endswith("|"):
                        acc += " |"
                if acc.endswith("|") and i + 1 < n:
                    peek = lines[i + 1].strip()
                    if peek.startswith("|") or peek == "" or _TABLE_SEP.match(peek):
                        block.append(acc)
                        acc = ""
                i += 1
            if acc:
                block.append(acc)
            out.extend(_tidy_row(row) for row in block)
            if i < n and lines[i].strip() != "":
                out.append("")
            continue
        out.append(lines[i])
        i += 1
    text = "\n".join(out)
    text = re.sub(r"[ \t]+\n", "\n", text)
    text = re.sub(r"\n{3,}", "\n\n", text)
    return text.strip() + ("\n" if md.endswith("\n") else "")


_BASE_SYM = r"([A-Za-z](?:_[A-Za-z0-9]+)?|\\[A-Za-z]+(?:\{[^}]+\})?)"
_SHORT_MATH = re.compile(r"^[\s\[\]{}().,;:=∅øΩωσΣ∪∩∈⊆⊂′'a-zA-Z0-9\\_-]{0,8}$")


def looks_broken_math(text: str) -> bool:
    lines = [ln.rstrip() for ln in text.splitlines()]
    nonempty = [ln.strip() for ln in lines if ln.strip()]
    if len(nonempty) < 4:
        return False
    avg = sum(len(ln) for ln in nonempty) / len(nonempty)
    short = sum(1 for ln in nonempty if len(ln) <= 2)
    glyphs = sum(1 for ln in nonempty if re.fullmatch(r"[ΩωσΣ∅ø∪∩∈⊆{}′']", ln))
    return (avg <= 10 and short / len(nonempty) >= 0.3) or glyphs >= 3


def collapse_broken_math(text: str) -> str:
    if not looks_broken_math(text):
        return text
    lines = text.replace("\r", "").split("\n")
    out: list[str] = []
    run = ""

    def flush() -> None:
        nonlocal run
        if run:
            out.append(run)
            run = ""

    for line in lines:
        s = line.strip()
        if s == "":
            flush()
            if out and out[-1] != "":
                out.append("")
            continue
        short = len(s) <= 2 or (len(s) <= 8 and bool(_SHORT_MATH.match(s)) and " " not in s)
        if short:
            run += s
            continue
        if run:
            if s[:1] in "={,;)]}":
                run += s
                flush()
            elif run.endswith("-"):
                run = run[:-1] + s
                flush()
            else:
                flush()
                out.append(s)
        elif out and out[-1].endswith("-"):
            prev = out[-1]
            keep = len(prev) >= 2 and not prev[-2].isascii()
            out[-1] = (prev if keep else prev[:-1]) + s
        else:
            out.append(s)
    flush()
    joined = re.sub(r"\n{3,}", "\n\n", "\n".join(out)).strip()
    joined = re.sub(r"(Ω_?[0-9]|Ω[₀-₉]|[A-Za-z]_?[0-9])\s*\1", r"\1", joined)
    return joined


def format_complements(md: str) -> str:
    def brace(s: str) -> str:
        s = re.sub(_BASE_SYM + r"\^\(([^)]+)\)", r"\1^{\2}", s)
        s = re.sub(_BASE_SYM + r"\^(c|C|circ|complement)\b", r"\1^{\2}", s)
        return s

    text = brace(md)
    text = re.sub(r"(^|[^$\\])(" + _BASE_SYM + r"\^\{[^}]+\})", r"\1$\2$", text)
    return text


def format_with_api(token: str, user_content: list[dict]) -> str:
    chat_content: list[dict] = []
    for part in user_content:
        kind = part.get("type")
        if kind == "input_text":
            chat_content.append({"type": "text", "text": part.get("text", "")})
        elif kind == "input_image":
            chat_content.append(
                {
                    "type": "image_url",
                    "image_url": {"url": part.get("image_url"), "detail": "high"},
                }
            )
        else:
            chat_content.append(part)
    data = http_json(
        CHAT_URL,
        token,
        {
            "model": MODEL,
            "messages": [
                {"role": "system", "content": MATH_PROMPT},
                {"role": "user", "content": chat_content},
            ],
        },
        timeout=20,
    )
    text = collapse_broken_math(strip_fence(extract_text(data)))
    text = format_complements(normalize_markdown_tables(text))
    if not text.strip():
        raise RuntimeError("empty response")
    return text


def format_math_text(text: str) -> str:
    token = load_token()
    if not token:
        raise RuntimeError("no xAI credentials")
    cleaned = collapse_broken_math(text)
    return format_with_api(
        token,
        [
            {
                "type": "input_text",
                "text": "Rewrite this copied maths as clean Markdown with LaTeX. "
                "Flowing sentences, no one-symbol-per-line.\n\n" + cleaned,
            }
        ],
    )


def format_math_image(data_url: str) -> str:
    token = load_token()
    if not token:
        raise RuntimeError("no xAI credentials")
    if not data_url.startswith("data:image/"):
        raise RuntimeError("unsupported image")
    header, _, b64 = data_url.partition(",")
    mime = "image/png"
    if "image/jpeg" in header or "image/jpg" in header:
        mime = "image/jpeg"
    elif "image/png" in header:
        mime = "image/png"
    elif "image/webp" in header:
        data_url = convert_to_png_data_url(base64.b64decode(b64))
        mime = "image/png"
    if mime not in {"image/png", "image/jpeg"}:
        raw = base64.b64decode(b64)
        data_url = convert_to_png_data_url(raw)
    return format_with_api(
        token,
        [
            {"type": "input_image", "image_url": data_url, "detail": "high"},
            {
                "type": "input_text",
                "text": "Transcribe this image of mathematics or lecture notes into Markdown with LaTeX.",
            },
        ],
    )


def convert_to_png_data_url(raw: bytes) -> str:
    loader = GdkPixbuf.PixbufLoader()
    loader.write(raw)
    loader.close()
    pixbuf = loader.get_pixbuf()
    ok, blob = pixbuf.save_to_bufferv("png", [], [])
    if not ok or not blob:
        raise RuntimeError("could not convert image")
    return "data:image/png;base64," + base64.b64encode(bytes(blob)).decode("ascii")


def read_wayland_image() -> str | None:
    for mime in ("image/png", "image/jpeg"):
        try:
            proc = subprocess.run(
                ["wl-paste", "--type", mime],
                capture_output=True,
                check=False,
            )
        except FileNotFoundError:
            return None
        if proc.returncode == 0 and proc.stdout:
            if mime == "image/png" and proc.stdout[:8] == b"\x89PNG\r\n\x1a\n":
                return "data:image/png;base64," + base64.b64encode(proc.stdout).decode("ascii")
            if mime == "image/jpeg" and proc.stdout[:2] == b"\xff\xd8":
                return "data:image/jpeg;base64," + base64.b64encode(proc.stdout).decode("ascii")
    return None


def copy_markdown(text: str) -> None:
    try:
        subprocess.run(["wl-copy", "--type", "text/plain"], input=text.encode("utf-8"), check=False)
    except FileNotFoundError:
        clipboard = Gtk.Clipboard.get(Gdk.SELECTION_CLIPBOARD)
        clipboard.set_text(text, -1)
        clipboard.store()


def js_call(web: WebKit2.WebView, expr: str) -> None:
    web.run_javascript(expr, None, None, None)


def js_string(value: str) -> str:
    return json.dumps(value)


class NotesApp(Gtk.Application):
    def __init__(self, app_id: str = APP_ID) -> None:
        super().__init__(application_id=app_id, flags=Gio.ApplicationFlags.FLAGS_NONE)
        self.window: Gtk.Window | None = None
        self.web: WebKit2.WebView | None = None
        self._req = 0
        self._geo_save = 0

    def do_activate(self) -> None:  # noqa: N802
        if self.window is None:
            self._build()
            self.window.show_all()
            self.window.present()
            GLib.timeout_add(80, self._apply_saved_size)
            return
        if self.window.get_visible() and self.window.is_active():
            self._capture_size()
            self.window.hide()
            return
        self.window.show()
        self.window.present()
        GLib.timeout_add(80, self._apply_saved_size)
        if self.web:
            js_call(self.web, "document.getElementById('src') && document.getElementById('src').focus()")

    def _build(self) -> None:
        theme = theme_payload()
        bg = theme.get("bg", "#1a1b26")
        screen = Gdk.Screen.get_default()
        css = Gtk.CssProvider()
        css.load_from_data(
            f"window {{ background-color: {bg}; }}\n".encode("utf-8")
        )
        Gtk.StyleContext.add_provider_for_screen(
            screen, css, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION
        )

        width, height = load_geometry()
        win = Gtk.ApplicationWindow(application=self, title="Notes")
        win.set_name("shadow-notes")
        win.set_decorated(False)
        win.set_resizable(True)
        win.set_keep_above(True)
        win.set_default_size(width, height)

        ucm = WebKit2.UserContentManager()
        ucm.register_script_message_handler("native")
        ucm.connect("script-message-received::native", self._on_message)
        try:
            sheet = WebKit2.UserStyleSheet(
                "html,body,*{cursor:default!important;}textarea,input,[contenteditable]{cursor:text!important;}button,a{cursor:pointer!important;}",
                WebKit2.UserContentInjectedFrames.ALL_FRAMES,
                WebKit2.UserStyleLevel.USER,
                None,
                None,
            )
            ucm.add_style_sheet(sheet)
        except Exception:
            pass

        settings = WebKit2.Settings()
        settings.set_enable_developer_extras(False)
        settings.set_enable_javascript(True)
        settings.set_allow_file_access_from_file_urls(True)
        try:
            settings.set_allow_universal_access_from_file_urls(True)
        except Exception:
            pass
        settings.set_enable_write_console_messages_to_stdout(False)

        web = WebKit2.WebView.new_with_user_content_manager(ucm)
        web.set_settings(settings)
        web.set_background_color(self._rgba(bg))
        web.connect("context-menu", lambda *args: True)
        web.connect("load-changed", self._on_load)
        web.load_uri(INDEX.resolve().as_uri() + f"?v={int(time.time())}")

        web.set_hexpand(True)
        web.set_vexpand(True)
        win.add(web)
        win.connect("delete-event", self._on_delete)
        win.connect("configure-event", self._on_configure)
        win.connect("realize", self._on_realize)

        self.window = win
        self.web = web

    @staticmethod
    def _rgba(color: str) -> Gdk.RGBA:
        rgba = Gdk.RGBA()
        if not rgba.parse(color):
            rgba.parse("#1a1b26")
        rgba.alpha = 1.0
        return rgba

    def _on_delete(self, *_args):
        self._capture_size()
        if self.window:
            self.window.hide()
        return True

    def _on_realize(self, widget) -> None:
        gdk_win = widget.get_window()
        if gdk_win is None:
            return
        cursor = Gdk.Cursor.new_from_name(widget.get_display(), "default")
        if cursor is not None:
            gdk_win.set_cursor(cursor)

    def _on_configure(self, _win, event) -> bool:
        if event.width >= MIN_SIZE[0] and event.height >= MIN_SIZE[1]:
            if self._geo_save:
                GLib.source_remove(self._geo_save)
            self._geo_save = GLib.timeout_add(200, self._flush_geo, event.width, event.height)
        return False

    def _flush_geo(self, width: int, height: int) -> bool:
        self._geo_save = 0
        save_geometry(width, height)
        return False

    def _capture_size(self) -> None:
        if self.window is None:
            return
        width, height = self.window.get_size()
        save_geometry(width, height)

    def _apply_saved_size(self) -> bool:
        width, height = load_geometry()
        if self.window is not None:
            self.window.resize(width, height)
        try:
            subprocess.run(
                [
                    "hyprctl",
                    "dispatch",
                    (
                        "hl.dsp.window.resize({"
                        f" x = {width}, y = {height}, relative = false,"
                        ' window = "class:shadow-notes" })'
                    ),
                ],
                check=False,
                capture_output=True,
            )
        except OSError:
            pass
        return False

    def _on_load(self, web: WebKit2.WebView, event: WebKit2.LoadEvent) -> None:
        if event != WebKit2.LoadEvent.FINISHED:
            return
        payload = {"theme": theme_payload(), "markdown": load_notes()}
        js_call(web, f"window.ShadowNotes && window.ShadowNotes.boot({json.dumps(payload)})")

    def _on_message(self, _manager, result: WebKit2.JavascriptResult) -> None:
        try:
            raw = result.get_js_value().to_string()
            msg = json.loads(raw)
        except Exception:
            return
        kind = msg.get("type")
        if kind == "save":
            save_notes(msg.get("markdown") or "")
        elif kind == "hide":
            if self.window:
                self.window.hide()
        elif kind == "copy":
            copy_markdown(msg.get("markdown") or "")
            if self.web:
                js_call(self.web, "window.ShadowNotes && window.ShadowNotes.copied()")
        elif kind == "format-text":
            self._start_format("text", msg.get("text") or "")
        elif kind == "format-image":
            self._start_format("image", msg.get("dataUrl") or "")
        elif kind == "format-clipboard-image":
            self._start_format("clipboard-image", "")

    def _start_format(self, kind: str, payload: str) -> None:
        self._req += 1
        req_id = self._req
        thread = threading.Thread(target=self._format_worker, args=(req_id, kind, payload), daemon=True)
        thread.start()

    def _format_worker(self, req_id: int, kind: str, payload: str) -> None:
        try:
            if kind == "text":
                text = format_math_text(payload)
            else:
                data_url = payload if kind == "image" else read_wayland_image()
                if not data_url:
                    raise RuntimeError("no image on clipboard")
                text = format_math_image(data_url)
            if not text.strip():
                raise RuntimeError("empty response")
            GLib.idle_add(self._format_done, req_id, text, None)
        except Exception as exc:
            GLib.idle_add(self._format_done, req_id, None, str(exc))

    def _format_done(self, req_id: int, text: str | None, error: str | None) -> bool:
        if req_id != self._req or self.web is None:
            return False
        if error or not text:
            js_call(self.web, f"window.ShadowNotes && window.ShadowNotes.fail({js_string(error or 'failed')})")
            return False
        js_call(self.web, f"window.ShadowNotes && window.ShadowNotes.insert({js_string(text)})")
        copy_markdown(text)
        return False


def instance_id_for(path: Path) -> str:
    """A distinct application id per note file.

    The app is single-instance, so without this a second note would only
    re-focus the first one's window. Keying the id to the path keeps that
    focus-instead-of-duplicate behaviour per note, which is what you want when
    the same note is opened twice.
    """
    digest = hashlib.sha1(os.path.abspath(path).encode("utf-8")).hexdigest()[:12]
    # D-Bus name elements may not start with a digit.
    return f"{APP_ID}.n{digest}"


def main() -> int:
    global NOTES_PATH
    GLib.set_prgname("omacastnotes")
    try:
        Gdk.set_program_class("omacastnotes")
    except Exception:
        pass

    app_id = APP_ID
    args = [a for a in sys.argv[1:] if not a.startswith("-")]
    if args:
        NOTES_PATH = Path(args[0]).expanduser()
        app_id = instance_id_for(NOTES_PATH)

    ensure_dirs()
    app = NotesApp(app_id)
    return app.run([sys.argv[0]])


if __name__ == "__main__":
    sys.exit(main())
