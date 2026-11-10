"""Мини-плеер «чёлка» — сверху экрана по центру, как у MacBook / Dynamic Island.

Ничего не играет -> чёлка спрятана за верхним краем экрана.
Играет музыка   -> выезжает: обложка, название, кнопки ⏮ ⏯ ⏭. Цвет — из обложки.
Наведение мыши  -> плавно раскрывается: исполнитель, прогресс (клик — перемотка).

Данные берутся из системного медиа-API Windows (Apple Music, Spotify, браузер...).
Выход и автозапуск — через иконку в трее.

  python player.py                    запуск
  python player.py --enable-autostart / --disable-autostart
"""
import asyncio
import colorsys
import ctypes
import io
import math
import os
import sys
import threading
import time
from pathlib import Path

APP_NAME = "FloatingNotchPlayer"
RUN_KEY = r"Software\Microsoft\Windows\CurrentVersion\Run"


# --- автозапуск (HKCU\...\Run) --------------------------------------------------
def _launch_command():
    if getattr(sys, "frozen", False):                  # собранный .exe
        return f'"{sys.executable}"'
    exe = Path(sys.executable)
    pyw = exe.with_name("pythonw.exe")
    return f'"{pyw if pyw.exists() else exe}" "{Path(__file__).resolve()}"'


def autostart_enabled():
    import winreg
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, RUN_KEY) as k:
            winreg.QueryValueEx(k, APP_NAME)
            return True
    except OSError:
        return False


def set_autostart(on):
    import winreg
    with winreg.OpenKey(winreg.HKEY_CURRENT_USER, RUN_KEY, 0, winreg.KEY_SET_VALUE) as k:
        if on:
            winreg.SetValueEx(k, APP_NAME, 0, winreg.REG_SZ, _launch_command())
        else:
            try:
                winreg.DeleteValue(k, APP_NAME)
            except OSError:
                pass


if "--enable-autostart" in sys.argv or "--disable-autostart" in sys.argv:
    set_autostart("--enable-autostart" in sys.argv)
    print("autostart:", autostart_enabled())
    sys.exit(0)

# --- один экземпляр --------------------------------------------------------------
_mutex = ctypes.windll.kernel32.CreateMutexW(None, False, "Local\\FloatingNotchPlayerSingleInstance")
if ctypes.windll.kernel32.GetLastError() == 183:  # ERROR_ALREADY_EXISTS
    sys.exit(0)

# интерактивный рабочий стол (нужно до создания окон)
user32 = ctypes.windll.user32
try:
    hd = user32.OpenDesktopW("Default", 0, False, 0x01FF)
    if hd:
        user32.SetThreadDesktop(hd)
except Exception:
    pass

from PIL import Image
from PySide6.QtCore import QObject, QPointF, QRectF, Qt, QTimer, QVariantAnimation, Signal
from PySide6.QtGui import (QAction, QColor, QCursor, QFont, QFontMetrics, QGuiApplication,
                           QIcon, QImage, QLinearGradient, QPainter, QPainterPath, QPen,
                           QPixmap)
from PySide6.QtWidgets import QApplication, QMenu, QSystemTrayIcon, QWidget

# --- внешний вид ----------------------------------------------------------------
EAR = 0                 # вогнутые «ушки» по бокам чёлки (0 — нет, прямые углы сверху)
W0, H0 = 360, 40        # размер тела чёлки в компактном виде
W1, H1 = 420, 102       # ... при наведении
PAUSE_LINGER = 10.0     # сколько секунд чёлка остаётся после паузы
DARKEN = 0.78           # затемнение цвета обложки (меньше — темнее)
HOVER_OFF_DELAY = 0.18  # задержка сворачивания после ухода курсора, сек

PLAYING, PAUSED = 4, 5  # GlobalSystemMediaTransportControlsSessionPlaybackStatus
DEFAULT_COLOR = QColor(24, 24, 30)


def clamp(v, a=0.0, b=1.0):
    return max(a, min(b, v))


def lerp(a, b, t):
    return a + (b - a) * t


def make_font(family, size, weight, spacing=0.0):
    f = QFont(family, size)
    f.setFamilies([family, "Segoe UI", "Segoe UI Symbol"])   # запасной шрифт для Windows 10
    f.setWeight(weight)
    f.setLetterSpacing(QFont.SpacingType.AbsoluteSpacing, spacing)
    f.setHintingPreference(QFont.HintingPreference.PreferNoHinting)
    f.setStyleStrategy(QFont.StyleStrategy.PreferAntialias)
    return f


def fmt_time(sec):
    sec = int(max(0, sec))
    return f"{sec // 60}:{sec % 60:02d}"


def shade(c, k):
    h, s, v, _ = c.getHsvF()
    return QColor.fromHsvF(max(h, 0.0), s, clamp(v * k))


# --- переход в приложение, из которого играет трек -----------------------------
def _window_of_app(app_id):
    """Верхнее видимое окно приложения-источника (по Z-порядку) или None.

    Совпадение: для обычных программ — имя exe (Spotify.exe, Chrome...);
    для приложений Microsoft Store — AUMID процесса (в том числе у дочерних окон
    внутри ApplicationFrameHost)."""
    from ctypes import wintypes
    k32 = ctypes.windll.kernel32
    k32.OpenProcess.restype = wintypes.HANDLE
    k32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    k32.CloseHandle.argtypes = [wintypes.HANDLE]
    k32.QueryFullProcessImageNameW.argtypes = [wintypes.HANDLE, wintypes.DWORD,
                                               wintypes.LPWSTR, ctypes.POINTER(wintypes.DWORD)]
    k32.GetApplicationUserModelId.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.UINT),
                                              wintypes.LPWSTR]
    want_id = app_id.lower()
    want_stem = Path(app_id).stem.lower() if "!" not in app_id else None
    cache = {}

    def pid_matches(pid):
        if pid in cache:
            return cache[pid]
        ok = False
        h = k32.OpenProcess(0x1000, False, pid)               # QUERY_LIMITED_INFORMATION
        if h:
            if want_stem:
                buf = ctypes.create_unicode_buffer(520)
                size = wintypes.DWORD(520)
                if k32.QueryFullProcessImageNameW(h, 0, buf, ctypes.byref(size)):
                    ok = Path(buf.value).stem.lower() == want_stem
            if not ok:
                n = wintypes.UINT(256)
                buf = ctypes.create_unicode_buffer(256)
                if k32.GetApplicationUserModelId(h, ctypes.byref(n), buf) == 0:
                    ok = buf.value.lower() == want_id
            k32.CloseHandle(h)
        cache[pid] = ok
        return ok

    def pid_of(hwnd):
        pid = wintypes.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
        return pid.value

    found = []

    @ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
    def on_child(hwnd, _):
        if pid_matches(pid_of(hwnd)):
            found.append(hwnd)
            return False
        return True

    @ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
    def on_top(hwnd, _):
        if not user32.IsWindowVisible(hwnd) or user32.GetWindowTextLengthW(hwnd) == 0:
            return True
        if user32.GetWindowLongW(hwnd, -20) & 0x80:          # WS_EX_TOOLWINDOW
            return True
        if pid_matches(pid_of(hwnd)):
            found.append(hwnd)
            return False
        before = len(found)
        user32.EnumChildWindows(hwnd, on_child, 0)
        if len(found) > before:
            found[-1] = hwnd                                  # поднимаем окно верхнего уровня
            return False
        return True

    user32.EnumWindows(on_top, 0)
    return found[0] if found else None


def _bring_to_front(hwnd):
    if user32.IsIconic(hwnd):
        user32.ShowWindow(hwnd, 9)                            # SW_RESTORE
    user32.keybd_event(0x12, 0, 0, 0)                         # нажатие Alt снимает блокировку
    user32.keybd_event(0x12, 0, 2, 0)                         # переднего плана Windows
    user32.BringWindowToTop(hwnd)
    user32.SetForegroundWindow(hwnd)


def open_source_app(app_id):
    """Открыть/вывести на передний план приложение-источник (AUMID из медиа-сессии)."""
    if not app_id:
        return
    hwnd = _window_of_app(app_id)
    if hwnd:
        _bring_to_front(hwnd)
        return
    # окна нет (свёрнуто в трей / закрыто) — просим оболочку запустить приложение
    import subprocess
    subprocess.Popen(["explorer.exe", f"shell:AppsFolder\\{app_id}"],
                     creationflags=0x08000000)               # CREATE_NO_WINDOW


def cover_color(data):
    """Цвет фона из обложки: доминантный (с учётом насыщенности) + среднее, затем затемнение."""
    try:
        im = Image.open(io.BytesIO(data)).convert("RGB")
        im.thumbnail((64, 64))
        avg = im.resize((1, 1), Image.BILINEAR).getpixel((0, 0))
        q = im.quantize(colors=8, method=Image.Quantize.MEDIANCUT)
        pal = q.getpalette()
        colors = q.getcolors() or []
        total = sum(c for c, _ in colors) or 1
        best, best_score = avg, -1.0
        for cnt, idx in colors:
            r, g, b = pal[idx * 3: idx * 3 + 3]
            h, s, v = colorsys.rgb_to_hsv(r / 255, g / 255, b / 255)
            score = (cnt / total) * (0.25 + s) * (1.0 if 0.15 < v < 0.95 else 0.35)
            if score > best_score:
                best, best_score = (r, g, b), score
        r, g, b = (int(best[i] * 0.7 + avg[i] * 0.3) for i in range(3))
        h, s, v = colorsys.rgb_to_hsv(r / 255, g / 255, b / 255)
        s = clamp(s * 1.25, 0.0, 0.85)
        if s > 0.06:
            s = max(s, 0.32)          # не даём цвету стать серым, если он у обложки есть
        v = clamp(v * DARKEN, 0.2, 0.46)
        r, g, b = colorsys.hsv_to_rgb(h, s, v)
        return QColor(int(r * 255), int(g * 255), int(b * 255))
    except Exception:
        return QColor(DEFAULT_COLOR)


# ==============================================================================
#  Медиа: фоновый поток, опрашивающий системные медиа-сессии Windows
# ==============================================================================
class Media(QObject):
    changed = Signal(object)

    def __init__(self):
        super().__init__()
        self.loop = None
        self.mgr = None
        self._key = None
        self._thumb = None
        self._attempts = 0
        threading.Thread(target=self._run, daemon=True).start()

    def command(self, name, arg=None):
        if self.loop:
            asyncio.run_coroutine_threadsafe(self._cmd(name, arg), self.loop)

    def _run(self):
        self.loop = asyncio.new_event_loop()
        asyncio.set_event_loop(self.loop)
        self.loop.run_until_complete(self._main())

    async def _main(self):
        from winrt.windows.media.control import \
            GlobalSystemMediaTransportControlsSessionManager as Manager
        self.mgr = await Manager.request_async()
        while True:
            try:
                await self._poll()
            except Exception as e:  # не даём потоку умереть
                print("media poll error:", e, file=sys.stderr)
            await asyncio.sleep(0.4)

    def _pick(self):
        for s in list(self.mgr.get_sessions()):
            if s.get_playback_info().playback_status == PLAYING:
                return s
        return self.mgr.get_current_session()

    async def _cmd(self, name, arg):
        s = self._pick()
        if not s:
            return
        if name == "toggle":
            await s.try_toggle_play_pause_async()
        elif name == "next":
            await s.try_skip_next_async()
        elif name == "prev":
            await s.try_skip_previous_async()
        elif name == "seek":
            await s.try_change_playback_position_async(int(arg * 10_000_000))

    @staticmethod
    async def _read_thumb(ref):
        from winrt.windows.storage.streams import DataReader
        stream = await ref.open_read_async()
        size = stream.size
        if not size:
            return None
        reader = DataReader(stream)
        await reader.load_async(size)
        buf = bytearray(size)
        reader.read_bytes(buf)
        return bytes(buf)

    async def _poll(self):
        s = self._pick()
        if s is None:
            self.changed.emit({"active": False})
            return
        pb = s.get_playback_info()
        status = int(pb.playback_status)
        info = await s.try_get_media_properties_async()
        key = (s.source_app_user_model_id, info.title, info.artist)
        if key != self._key:
            self._key, self._thumb, self._attempts = key, None, 0
        if self._thumb is None and self._attempts < 10 and info.thumbnail is not None:
            self._attempts += 1
            try:
                self._thumb = await self._read_thumb(info.thumbnail)
            except Exception:
                self._thumb = None

        pos = dur = 0.0
        try:
            tl = s.get_timeline_properties()
            dur = (tl.end_time - tl.start_time).total_seconds()
            pos = clamp((tl.position - tl.start_time).total_seconds(), 0, max(dur, 0))
        except Exception:
            pass

        self.changed.emit({
            "active": True, "status": status, "app": s.source_app_user_model_id or "",
            "title": info.title or "", "artist": info.artist or "",
            "key": key, "thumb": self._thumb,
            "pos": pos, "dur": dur,
            "can_prev": bool(pb.controls.is_previous_enabled),
            "can_next": bool(pb.controls.is_next_enabled),
        })


# ==============================================================================
#  Окно-«чёлка»
# ==============================================================================
class Notch(QWidget):
    def __init__(self):
        super().__init__()
        self.setWindowFlags(Qt.WindowType.FramelessWindowHint
                            | Qt.WindowType.WindowStaysOnTopHint
                            | Qt.WindowType.Tool
                            | Qt.WindowType.WindowDoesNotAcceptFocus)
        self.setAttribute(Qt.WidgetAttribute.WA_TranslucentBackground)
        self.setAttribute(Qt.WidgetAttribute.WA_ShowWithoutActivating)
        self.setMouseTracking(True)

        D, T = "Segoe UI Variable Display", "Segoe UI Variable Text"
        self.title_font = make_font(D, 9.5, QFont.Weight.DemiBold, 0.1)
        self.artist_font = make_font(T, 8, QFont.Weight.Medium, 0.1)
        self.time_font = make_font(T, 7, QFont.Weight.Medium, 0.2)
        self.title_fm = QFontMetrics(self.title_font)
        self.artist_fm = QFontMetrics(self.artist_font)

        # состояние данных
        self.state = {"active": False}
        self.cover_key = None
        self.cover = None
        self.col = QColor(DEFAULT_COLOR)
        self.stamp = time.monotonic()
        self.paused_since = None
        self.last_status = None
        # состояние анимации
        self.show_t = 0.0       # 0 = спрятана, 1 = выехала
        self.hov_t = 0.0        # 0 = компактно, 1 = раскрыто
        self.want_show = False
        self.hover = False
        self.hover_since = 0.0
        self.left_at = None
        self.hover_btn = None
        self.hit = {}
        self.force_hover = bool(os.environ.get("CLOCK_DEBUG_HOVER"))
        self._last_tick = time.monotonic()
        self._geom = None

        self.color_anim = QVariantAnimation(self)
        self.color_anim.setDuration(650)
        self.color_anim.valueChanged.connect(self._on_color)

        self.media = Media()
        self.media.changed.connect(self._on_media)

        self.timer = QTimer(self)
        self.timer.timeout.connect(self._tick)
        self.timer.start(16)

    # --- данные ---
    def _on_color(self, v):
        self.col = QColor(v)

    def _on_media(self, d):
        self.state = d
        self.stamp = time.monotonic()
        if d.get("active"):
            st = d["status"]
            if st != self.last_status:
                if st == PAUSED:
                    self.paused_since = time.monotonic()
                self.last_status = st
            if d["key"] != self.cover_key or (self.cover is None and d.get("thumb")):
                self.cover_key = d["key"]
                self._set_cover(d.get("thumb"))
        else:
            self.last_status = None

    def _set_cover(self, data):
        self.cover = None
        target = QColor(DEFAULT_COLOR)
        if data:
            img = QImage.fromData(data)
            if not img.isNull():
                side = min(img.width(), img.height())
                img = img.copy((img.width() - side) // 2, (img.height() - side) // 2, side, side)
                dpr = self.devicePixelRatioF()
                px = int(44 * dpr)
                pm = QPixmap.fromImage(img.scaled(
                    px, px, Qt.AspectRatioMode.IgnoreAspectRatio,
                    Qt.TransformationMode.SmoothTransformation))
                pm.setDevicePixelRatio(dpr)
                self.cover = pm
                target = cover_color(data)
        self.color_anim.stop()
        self.color_anim.setStartValue(QColor(self.col))
        self.color_anim.setEndValue(target)
        self.color_anim.start()

    def _wants_show(self):
        d = self.state
        if not (d.get("active") and d.get("title")):
            return False
        if d["status"] == PLAYING:
            return True
        if d["status"] == PAUSED:
            if not d.get("artist") and d.get("dur", 0.0) <= 0:   # пустая заглушка («Not Playing»)
                return False
            recent = (self.paused_since is not None
                      and time.monotonic() - self.paused_since < PAUSE_LINGER)
            return recent or self.hover
        return False

    def _pos(self):
        d = self.state
        pos = d.get("pos", 0.0)
        if d.get("status") == PLAYING:
            pos += time.monotonic() - self.stamp
        dur = d.get("dur", 0.0)
        return clamp(pos, 0, dur) if dur > 0 else 0.0

    # --- основной цикл анимации ---
    def _tick(self):
        now = time.monotonic()
        dt = clamp(now - self._last_tick, 0.0, 0.1)
        self._last_tick = now

        # наведение курсора (по положению мыши, а не по событиям — стабильнее при ресайзе)
        inside = False
        if self.isVisible() and self.show_t > 0.3:
            inside = self.frameGeometry().contains(QCursor.pos())
        if self.force_hover:
            inside = True
        if inside:
            if not self.hover:
                self.hover_since = now
            self.hover, self.left_at = True, None
        elif self.hover:
            if self.left_at is None:
                self.left_at = now
            if now - self.left_at > HOVER_OFF_DELAY:
                self.hover, self.left_at = False, None
                if self.last_status == PAUSED:
                    self.paused_since = now

        self.want_show = self._wants_show()
        k = lambda rate: 1 - math.exp(-dt * rate)
        self.show_t += ((1.0 if self.want_show else 0.0) - self.show_t) * k(9)
        self.hov_t += ((1.0 if self.hover else 0.0) - self.hov_t) * k(13)
        if self.show_t > 0.999 and self.want_show:
            self.show_t = 1.0
        if self.hov_t < 0.002 and not self.hover:
            self.hov_t = 0.0
        if self.hov_t > 0.998 and self.hover:
            self.hov_t = 1.0

        if self.show_t < 0.004 and not self.want_show:
            self.show_t = 0.0
            if self.isVisible():
                self.hide()
            self.timer.setInterval(100)
            return
        self.timer.setInterval(16)
        if not self.isVisible():
            self._place()
            self.show()
        self._place()
        self.raise_()

        # ховер над кнопками
        btn = None
        if inside and self.hov_t > 0.0:
            lp = self.mapFromGlobal(QCursor.pos())
            lp = QPointF(lp.x(), lp.y())
            for name, rect in self.hit.items():
                if rect.contains(lp):
                    btn = name
                    break
        if btn != self.hover_btn:
            self.hover_btn = btn
        self.setCursor(Qt.CursorShape.PointingHandCursor if btn else Qt.CursorShape.ArrowCursor)
        self.update()

    def _body_size(self):
        h = self.hov_t
        return lerp(W0, W1, h), lerp(H0, H1, h)

    def _place(self):
        # чёлка выезжает целиком (жёстко), а не «набирается» по высоте
        bw, bh = self._body_size()
        w, hgt = int(round(bw)) + EAR * 2, int(math.ceil(bh))
        scr = QGuiApplication.primaryScreen().geometry()
        x = scr.x() + (scr.width() - w) // 2
        y = scr.y() - int(round((1.0 - self.show_t) * hgt))
        g = (x, y, w, hgt)
        if g != self._geom:
            self._geom = g
            self.setGeometry(*g)

    # --- иконки ---
    @staticmethod
    def _tri(p, pts, color):
        path = QPainterPath()
        path.moveTo(*pts[0])
        for q in pts[1:]:
            path.lineTo(*q)
        path.closeSubpath()
        p.setBrush(color)
        p.setPen(QPen(color, 1.5, Qt.PenStyle.SolidLine, Qt.PenCapStyle.RoundCap,
                      Qt.PenJoinStyle.RoundJoin))
        p.drawPath(path)

    def _glyph(self, p, kind, cx, cy, s, color):
        p.setPen(Qt.PenStyle.NoPen)
        p.setBrush(color)
        if kind in ("prev", "next"):
            m = -1 if kind == "prev" else 1
            p.drawRoundedRect(QRectF(cx + m * s * 0.52 - s * 0.09, cy - s * 0.46, s * 0.18, s * 0.92), 1, 1)
            self._tri(p, [(cx - m * s * 0.48, cy - s * 0.46), (cx - m * s * 0.48, cy + s * 0.46),
                          (cx + m * s * 0.34, cy)], color)
        elif kind == "play":
            self._tri(p, [(cx - s * 0.30, cy - s * 0.52), (cx - s * 0.30, cy + s * 0.52),
                          (cx + s * 0.58, cy)], color)
        elif kind == "pause":
            w = s * 0.30
            p.drawRoundedRect(QRectF(cx - s * 0.46, cy - s * 0.5, w, s), 1.6, 1.6)
            p.drawRoundedRect(QRectF(cx + s * 0.16, cy - s * 0.5, w, s), 1.6, 1.6)

    # --- отрисовка ---
    def paintEvent(self, _):
        p = QPainter(self)
        p.setRenderHints(QPainter.RenderHint.Antialiasing
                         | QPainter.RenderHint.TextAntialiasing
                         | QPainter.RenderHint.SmoothPixmapTransform)
        h = self.hov_t
        bw, bh = self._body_size()
        W = bw + EAR * 2
        ear = min(float(EAR), bh * 0.8)
        R = min(lerp(10, 14, h), bh / 2)
        L, Rt = float(EAR), float(EAR) + bw     # левая/правая границы тела

        # --- форма: плоский верх, вогнутые «ушки», скруглённый низ ---
        path = QPainterPath()
        path.moveTo(L - ear, 0)
        path.lineTo(Rt + ear, 0)
        path.quadTo(Rt, 0, Rt, ear)
        path.lineTo(Rt, bh - R)
        path.quadTo(Rt, bh, Rt - R, bh)
        path.lineTo(L + R, bh)
        path.quadTo(L, bh, L, bh - R)
        path.lineTo(L, ear)
        path.quadTo(L, 0, L - ear, 0)
        path.closeSubpath()

        g = QLinearGradient(0, 0, 0, bh)
        g.setColorAt(0.0, shade(self.col, 1.0))
        g.setColorAt(1.0, shade(self.col, 0.72))
        p.setPen(Qt.PenStyle.NoPen)
        p.setBrush(g)
        p.drawPath(path)

        p.save()
        p.setClipPath(path)
        a = 1.0
        self.hit = {}
        if a > 0:
            p.setOpacity(a)
            d = self.state
            cy = lerp(H0 / 2, 31.0, h)

            # --- обложка ---
            cs = lerp(26, 40, h)
            cr = QRectF(L + 13, cy - cs / 2, cs, cs)
            rad = lerp(4, 6, h)
            cp = QPainterPath()
            cp.addRoundedRect(cr, rad, rad)
            p.save()
            p.setClipPath(cp)
            if self.cover is not None:
                p.drawPixmap(cr.toRect(), self.cover)
            else:
                p.fillRect(cr, QColor(255, 255, 255, 28))
                p.setPen(QColor(255, 255, 255, 120))
                p.setFont(make_font("Segoe UI Symbol", 12, QFont.Weight.Normal))
                p.drawText(cr, Qt.AlignmentFlag.AlignCenter, "♪")
            p.restore()
            p.setPen(QPen(QColor(255, 255, 255, 30), 1))
            p.setBrush(Qt.BrushStyle.NoBrush)
            p.drawRoundedRect(cr.adjusted(0.5, 0.5, -0.5, -0.5), rad, rad)
            if self.hover_btn == "open":                    # подсветка обложки: «кликабельно»
                p.setPen(Qt.PenStyle.NoPen)
                p.setBrush(QColor(255, 255, 255, 38))
                p.drawRoundedRect(cr, rad, rad)

            # --- кнопки ---
            sp = lerp(27, 35, h)
            nx = Rt - 12 - sp / 2
            centers = {"next": nx, "play": nx - sp, "prev": nx - 2 * sp}
            playing = d.get("status") == PLAYING
            for name in ("prev", "play", "next"):
                bx = centers[name]
                enabled = d.get("can_prev", True) if name == "prev" else \
                    d.get("can_next", True) if name == "next" else True
                self.hit[name] = QRectF(bx - sp / 2, cy - 17, sp, 34)
                hov = self.hover_btn == name and enabled
                if name == "play":
                    rr = lerp(0, 15, h) * (1.08 if hov else 1.0)
                    if rr > 0.5:
                        p.setPen(Qt.PenStyle.NoPen)
                        p.setBrush(QColor(255, 255, 255, 255 if hov else 238))
                        p.drawEllipse(QPointF(bx, cy), rr, rr)
                    # глиф: белый в компакте -> тёмный на белом круге
                    gcol = QColor(int(lerp(255, 18, h)), int(lerp(255, 18, h)), int(lerp(255, 22, h)))
                    self._glyph(p, "pause" if playing else "play", bx, cy, lerp(9, 11, h), gcol)
                else:
                    if hov:
                        p.setPen(Qt.PenStyle.NoPen)
                        p.setBrush(QColor(255, 255, 255, 34))
                        p.drawEllipse(QPointF(bx, cy), 14.5, 14.5)
                    self._glyph(p, name, bx, cy, lerp(8, 10, h),
                                QColor(255, 255, 255, 240 if enabled else 80))

            # --- текст ---
            tx = cr.right() + 11
            area = QRectF(tx, 0, centers["prev"] - sp / 2 - 8 - tx, bh)
            if h > 0.9:                                     # клик по обложке/названию -> приложение
                self.hit["open"] = QRectF(cr.left(), 0, area.right() - cr.left(), 58)
            title = d.get("title", "")
            artist = d.get("artist", "")
            tw = self.title_fm.horizontalAdvance(title)
            ty = cy - 8 * h
            trect = QRectF(area.left(), ty - self.title_fm.height() / 2, area.width(), self.title_fm.height())
            p.setFont(self.title_font)
            p.setPen(QColor(255, 255, 255, 250))
            if tw > area.width() and h > 0.9:                # бегущая строка при наведении
                extra = tw - area.width()
                pause, speed = 0.9, 32.0
                run = extra / speed
                u = (time.monotonic() - self.hover_since) % (2 * (run + pause))
                if u < pause:
                    off = 0.0
                elif u < pause + run:
                    off = (u - pause) * speed
                elif u < 2 * pause + run:
                    off = extra
                else:
                    off = extra - (u - 2 * pause - run) * speed
                p.save()
                p.setClipRect(trect)
                p.drawText(QRectF(trect.left() - off, trect.top(), tw + 4, trect.height()),
                           Qt.AlignmentFlag.AlignVCenter | Qt.AlignmentFlag.AlignLeft, title)
                p.restore()
            else:
                p.drawText(trect, Qt.AlignmentFlag.AlignVCenter | Qt.AlignmentFlag.AlignLeft,
                           self.title_fm.elidedText(title, Qt.TextElideMode.ElideRight, int(area.width())))
            if h > 0.02:                                       # исполнитель — только при наведении
                p.setOpacity(a * h)
                p.setFont(self.artist_font)
                p.setPen(QColor(255, 255, 255, 205))
                arect = QRectF(area.left(), cy + 9 - self.artist_fm.height() / 2,
                               area.width(), self.artist_fm.height())
                p.drawText(arect, Qt.AlignmentFlag.AlignVCenter | Qt.AlignmentFlag.AlignLeft,
                           self.artist_fm.elidedText(artist, Qt.TextElideMode.ElideRight, int(area.width())))

                # --- прогресс ---
                bx0, bx1 = L + 16, Rt - 16
                by = 70.0
                dur = d.get("dur", 0.0)
                frac = self._pos() / dur if dur > 0 else 0.0
                over = self.hover_btn == "bar"
                th = 6.0 if over else 4.0
                track = QRectF(bx0, by - th / 2, bx1 - bx0, th)
                p.setPen(Qt.PenStyle.NoPen)
                p.setBrush(QColor(255, 255, 255, 48))
                p.drawRoundedRect(track, th / 2, th / 2)
                p.setBrush(QColor(255, 255, 255, 235))
                fw = max(th, track.width() * frac)
                p.drawRoundedRect(QRectF(track.left(), track.top(), fw, th), th / 2, th / 2)
                if over:
                    p.drawEllipse(QPointF(track.left() + fw, by), 5.5, 5.5)
                self.hit["bar"] = QRectF(bx0 - 4, by - 9, bx1 - bx0 + 8, 18)
                p.setFont(self.time_font)
                p.setPen(QColor(255, 255, 255, 150))
                p.drawText(QRectF(bx0, by + 8, 60, 12), Qt.AlignmentFlag.AlignLeft | Qt.AlignmentFlag.AlignVCenter,
                           fmt_time(self._pos()))
                p.drawText(QRectF(bx1 - 60, by + 8, 60, 12), Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter,
                           fmt_time(dur) if dur > 0 else "")
        p.restore()
        p.end()

    # --- мышь ---
    def mousePressEvent(self, e):
        if e.button() != Qt.MouseButton.LeftButton:
            return
        pos = e.position()
        for name in ("prev", "play", "next"):
            r = self.hit.get(name)
            if r and r.contains(pos):
                self.media.command({"prev": "prev", "play": "toggle", "next": "next"}[name])
                return
        r = self.hit.get("bar")
        dur = self.state.get("dur", 0.0)
        if r and r.contains(pos) and dur > 0:
            frac = clamp((pos.x() - (r.left() + 4)) / (r.width() - 8))
            self.media.command("seek", frac * dur)
            self.state["pos"] = frac * dur       # сразу двигаем полоску
            self.stamp = time.monotonic()
            return
        r = self.hit.get("open")
        if r and r.contains(pos) and self.hov_t > 0.8:
            try:
                open_source_app(self.state.get("app", ""))
            except Exception as e:
                print("open app error:", e, file=sys.stderr)


def tray_icon():
    pm = QPixmap(64, 64)
    pm.fill(Qt.GlobalColor.transparent)
    p = QPainter(pm)
    p.setRenderHint(QPainter.RenderHint.Antialiasing)
    p.setPen(Qt.PenStyle.NoPen)
    p.setBrush(QColor(12, 12, 16))
    p.drawRoundedRect(QRectF(2, 2, 60, 60), 16, 16)
    p.setBrush(QColor(255, 255, 255))
    p.drawEllipse(QPointF(24, 44), 8, 6)
    p.drawRect(QRectF(29.5, 14, 4, 30))
    path = QPainterPath()
    path.moveTo(33.5, 14)
    path.quadTo(46, 18, 44, 30)
    path.quadTo(40, 22, 33.5, 22)
    path.closeSubpath()
    p.drawPath(path)
    p.end()
    return QIcon(pm)


def main():
    try:
        ctypes.windll.shcore.SetProcessDpiAwareness(2)
    except Exception:
        pass
    app = QApplication(sys.argv)
    app.setQuitOnLastWindowClosed(False)
    w = Notch()

    tray = QSystemTrayIcon(tray_icon(), app)
    tray.setToolTip("Плеер-чёлка")
    menu = QMenu()
    act_auto = QAction("Запускать с Windows", menu)
    act_auto.setCheckable(True)
    act_auto.setChecked(autostart_enabled())
    act_quit = QAction("Выход", menu)
    menu.addAction(act_auto)
    menu.addSeparator()
    menu.addAction(act_quit)
    act_auto.toggled.connect(set_autostart)
    act_quit.triggered.connect(app.quit)
    tray.setContextMenu(menu)
    tray.show()

    shot = os.environ.get("CLOCK_DEBUG_SHOT")
    if shot:
        def save():
            w.grab().save(shot)
            print("state:", {k: v for k, v in w.state.items() if k not in ("thumb", "key")},
                  "visible:", w.isVisible(), "show_t:", round(w.show_t, 2), "hov_t:", round(w.hov_t, 2),
                  "hover:", w.hover, "cursor:", QCursor.pos().toTuple(),
                  "geom:", w.geometry().getRect(), "color:", w.col.name())
            app.quit()
        QTimer.singleShot(3500, save)
    sys.exit(app.exec())


if __name__ == "__main__":
    main()
