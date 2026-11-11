use crate::{
    media::{MediaBackend, SEEK_ACK_TIMEOUT},
    model::{MediaCommand, MediaSnapshot, Playback, PlaybackPosition},
    platform::{self, Tray, TrayEvent},
    settings::{IdleView, NotchPosition, Settings},
    settings_ui,
    theme::Theme,
};
use eframe::egui::{self, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2, pos2, vec2};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

const BACKGROUND_HOLD: Duration = Duration::from_secs(1);
const BACKGROUND_FADE: f32 = 0.65;
const RESIZE_FRAME: Duration = Duration::from_millis(24);
const ANIMATION_FRAME: Duration = Duration::from_millis(42);
const PROGRESS_FRAME: Duration = Duration::from_millis(16);
const SEEK_THUMB_RADIUS: f32 = 5.5;
const FULLSCREEN_CHECK: Duration = Duration::from_millis(250);
pub const MINI_SIZE: [f32; 2] = [152.0, 12.0];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NotchView {
    Mini,
    Normal,
    Expanded,
}

struct Background {
    theme: Theme,
    previous: Option<(Theme, Instant)>,
    missing_since: Option<Instant>,
    black_mix: f32,
}

impl Background {
    fn new() -> Self {
        Self {
            theme: Theme::default(),
            previous: None,
            missing_since: None,
            black_mix: 1.0,
        }
    }

    fn set_artwork(&mut self, theme: Theme, now: Instant) {
        let previous = std::mem::replace(&mut self.theme, theme);
        self.previous = (self.black_mix < 1.0).then_some((previous, now));
    }

    fn update(&mut self, has_artwork: bool, now: Instant, dt: f32) {
        let step = dt / BACKGROUND_FADE;
        if has_artwork {
            self.missing_since = None;
            self.black_mix = (self.black_mix - step).max(0.0);
        } else {
            let since = *self.missing_since.get_or_insert(now);
            if now.duration_since(since) >= BACKGROUND_HOLD {
                self.black_mix = (self.black_mix + step).min(1.0);
                if self.black_mix == 1.0 {
                    self.theme = Theme::default();
                }
            }
        }
        if self
            .previous
            .as_ref()
            .is_some_and(|(_, since)| now.duration_since(*since).as_secs_f32() >= BACKGROUND_FADE)
        {
            self.previous = None;
        }
    }

    fn paint(&self, painter: &egui::Painter, rect: Rect, time: f32, now: Instant, radius: f32) {
        if self.black_mix < 1.0 {
            let mix = if let Some((previous, since)) = &self.previous {
                previous.paint_with_radius(painter, rect, time, 1.0, radius);
                (now.duration_since(*since).as_secs_f32() / BACKGROUND_FADE).clamp(0.0, 1.0)
            } else {
                1.0
            };
            self.theme
                .paint_with_radius(painter, rect, time, mix, radius);
        }
        if self.black_mix > 0.0 {
            Theme::default().paint_with_radius(painter, rect, time, self.black_mix, radius);
        }
    }
}

pub struct NotchApp {
    media: Option<MediaBackend>,
    tray: Option<Tray>,
    settings: Settings,
    autostart_mini: bool,
    autostart_enabled: bool,
    settings_open: bool,
    tray_error: Option<String>,
    autostart_error: Option<String>,
    save_error: Option<String>,
    snapshot: MediaSnapshot,
    has_media: bool,
    position: f64,
    playback: Playback,
    seek_hold: Option<(u64, Instant, PlaybackPosition)>,
    next_seek_id: u64,
    idle_since: Option<Instant>,
    cover: Option<egui::TextureHandle>,
    background: Background,
    last_frame: Instant,
    started: Instant,
    fluid_time: f32,
    clock: Option<(Instant, String, String)>,
    expand: f32,
    mini: f32,
    hover: bool,
    hover_since: Instant,
    entered_at: Option<Instant>,
    left_at: Option<Instant>,
    pinned: bool,
    dragging: bool,
    geometry: Option<[i32; 4]>,
    overlay_hwnd: Option<isize>,
    hidden: bool,
    fullscreen_sample: Option<(Instant, bool)>,
    source_focus_sample: Option<(Instant, String, bool)>,
    force_hover: bool,
    demo: bool,
    screenshot_requested: bool,
}

impl NotchApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        demo: bool,
        autostart: bool,
        open_settings: bool,
    ) -> Self {
        install_fonts(&cc.egui_ctx);
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let overlay_hwnd = cc
            .window_handle()
            .ok()
            .and_then(|handle| match handle.as_raw() {
                RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
                _ => None,
            });
        if let Some(hwnd) = overlay_hwnd {
            platform::configure_overlay(hwnd);
        }
        let settings = Settings::load();
        platform::set_language(settings.language);
        let (tray, error) = match Tray::new(cc.egui_ctx.clone()) {
            Ok(tray) => (Some(tray), None),
            Err(error) => (None, Some(error)),
        };
        let now = Instant::now();
        Self {
            media: (!demo)
                .then(|| MediaBackend::start(cc.egui_ctx.clone(), settings.priority_app.clone())),
            tray,
            settings_open: open_settings || error.is_some(),
            tray_error: error,
            autostart_error: None,
            save_error: None,
            autostart_mini: autostart,
            autostart_enabled: platform::autostart_enabled(),
            settings,
            snapshot: MediaSnapshot::default(),
            has_media: false,
            position: 0.0,
            playback: Playback::Stopped,
            seek_hold: None,
            next_seek_id: 0,
            idle_since: None,
            cover: None,
            background: Background::new(),
            last_frame: now,
            started: now,
            fluid_time: 2.4,
            clock: None,
            expand: 0.0,
            mini: 1.0,
            hover: false,
            hover_since: now,
            entered_at: None,
            left_at: None,
            pinned: false,
            dragging: false,
            geometry: None,
            overlay_hwnd,
            hidden: false,
            fullscreen_sample: None,
            source_focus_sample: None,
            force_hover: std::env::var_os("CLOCK_DEBUG_HOVER").is_some()
                || std::env::var_os("PLAYER_NOTCH_DEBUG_HOVER").is_some(),
            demo,
            screenshot_requested: false,
        }
    }

    fn receive_media(&mut self, ctx: &egui::Context, now: Instant, dt: f32) -> bool {
        let incoming = self
            .media
            .as_ref()
            .and_then(|media| media.snapshot_since(self.snapshot.observed_at))
            .or_else(|| (self.demo && !self.snapshot.active).then(demo_snapshot));
        let changed = incoming.as_ref().is_some_and(|incoming| {
            incoming.session != self.snapshot.session || incoming.track != self.snapshot.track
        });
        if changed {
            self.seek_hold = None;
            self.dragging = false;
        }
        if let Some(mut incoming) = incoming {
            self.has_media = has_media(&incoming);
            if !self.has_media {
                incoming.artwork = None;
            }
            let art_changed = match (&self.snapshot.artwork, &incoming.artwork) {
                (Some(a), Some(b)) => !Arc::ptr_eq(a, b),
                (None, None) => false,
                _ => true,
            };
            if art_changed {
                self.cover = incoming.artwork.as_ref().map(|art| {
                    self.background.set_artwork(art.theme.clone(), now);
                    ctx.load_texture("cover", art.image.clone(), egui::TextureOptions::LINEAR)
                });
            }
            self.snapshot = incoming;
        }

        let incoming = &self.snapshot;
        let playback = incoming.playback;
        let playing = playback == Playback::Playing;
        if changed || playing != (self.playback == Playback::Playing) {
            self.idle_since = (!playing).then_some(now);
        }
        if self
            .seek_hold
            .is_some_and(|(id, until, _)| incoming.handled_seek >= id || now >= until)
        {
            self.seek_hold = None;
        }
        if let Some((_, _, position)) = &mut self.seek_hold {
            *position = position.with_rate(incoming.position.rate(), now, incoming.duration);
        }
        self.playback = playback;
        if !self.dragging {
            self.position = self.seek_hold.map_or_else(
                || incoming.position_at(now),
                |(_, _, position)| position.value(now, incoming.duration),
            );
        }
        self.position = self.position.clamp(0.0, incoming.duration.max(0.0));
        self.background
            .update(self.cover.is_some(), now, dt.min(0.1));
        self.has_media
    }

    fn update_hover(&mut self, now: Instant, inside: bool) {
        let held_open = self.force_hover
            || self.pinned
            || self.dragging
            || (self.hover && platform::context_menu_open());
        let hovering = if held_open {
            self.entered_at = None;
            self.left_at = None;
            true
        } else if !self.settings.open_on_hover {
            self.entered_at = None;
            self.left_at = None;
            false
        } else if inside {
            let since = *self.entered_at.get_or_insert(now);
            self.left_at = None;
            self.hover
                || now.duration_since(since) >= Duration::from_millis(self.settings.hover_delay_ms)
        } else {
            self.entered_at = None;
            let since = *self.left_at.get_or_insert(now);
            self.hover
                && now.duration_since(since)
                    < Duration::from_millis(self.settings.collapse_delay_ms)
        };
        if hovering && !self.hover {
            self.hover_since = now;
        }
        self.hover = hovering;
    }

    fn should_hide(&mut self, ctx: &egui::Context, now: Instant, media: bool) -> bool {
        if self.tray.is_none() || self.force_hover {
            return false;
        }
        if !media && self.settings.idle_view == IdleView::Hidden {
            self.pinned = false;
            return true;
        }
        if !self.settings.hide_fullscreen {
            self.fullscreen_sample = None;
            return false;
        }
        if let Some((checked, fullscreen)) = self.fullscreen_sample {
            if let Some(remaining) = FULLSCREEN_CHECK.checked_sub(now.duration_since(checked)) {
                ctx.request_repaint_after(remaining);
                return fullscreen;
            }
        }
        let fullscreen = platform::primary_display_fullscreen();
        self.fullscreen_sample = Some((now, fullscreen));
        ctx.request_repaint_after(FULLSCREEN_CHECK);
        fullscreen
    }

    fn animate(&mut self, ctx: &egui::Context, now: Instant, dt: f32, media: bool) -> Vec2 {
        let should_hide = self.should_hide(ctx, now, media);
        if self.hidden != should_hide
            && let Some(hwnd) = self.overlay_hwnd
            && platform::set_overlay_hidden(hwnd, should_hide)
        {
            self.hidden = should_hide;
        }
        let hidden = self.hidden;
        let desktop = platform::desktop_info();
        let size = self.size();
        let origin = notch_origin(desktop, size, self.settings.position);
        if hidden {
            self.hover = false;
            self.entered_at = None;
            self.left_at = None;
            self.dragging = false;
        } else {
            let cursor = platform::cursor_pos(desktop.scale);
            self.update_hover(
                now,
                Rect::from_min_size(origin, size).contains(pos2(cursor[0], cursor[1])),
            );
            if let Some(since) = self.entered_at.filter(|_| !self.hover) {
                ctx.request_repaint_after(
                    (since + Duration::from_millis(self.settings.hover_delay_ms))
                        .saturating_duration_since(now),
                );
            }
            if let Some(since) = self.left_at.filter(|_| self.hover) {
                ctx.request_repaint_after(
                    (since + Duration::from_millis(self.settings.collapse_delay_ms))
                        .saturating_duration_since(now),
                );
            }
        }
        let pause_age = self
            .idle_since
            .map(|since| now.saturating_duration_since(since));
        let source_focused = if media && !self.demo {
            let id = &self.snapshot.app_id;
            let cached = self
                .source_focus_sample
                .as_ref()
                .filter(|(checked, source, _)| {
                    source == id && now.duration_since(*checked) < FULLSCREEN_CHECK
                });
            let focused = if let Some((_, _, focused)) = cached {
                *focused
            } else {
                let focused = platform::foreground_is_media_source(id);
                self.source_focus_sample = Some((now, id.clone(), focused));
                focused
            };
            ctx.request_repaint_after(FULLSCREEN_CHECK);
            focused
        } else {
            self.source_focus_sample = None;
            false
        };
        let view = notch_view(
            media,
            self.playback,
            pause_age,
            self.settings.mini_notch || self.autostart_mini || source_focused,
            self.hover,
            &self.settings,
        );
        let expand = f32::from(view == NotchView::Expanded);
        let mini = f32::from(view == NotchView::Mini);
        if self.settings.smooth_resize && !hidden {
            self.expand = approach(self.expand, expand, 13.0, dt);
            self.mini = approach(self.mini, mini, 13.0, dt);
            if self.expand != expand || self.mini != mini {
                ctx.request_repaint_after(RESIZE_FRAME);
            }
        } else {
            self.expand = expand;
            self.mini = mini;
        }
        let size = self.size();
        let origin = notch_origin(desktop, size, self.settings.position);
        let geometry = [
            origin.x.round() as i32,
            origin.y.round() as i32,
            size.x.round() as i32,
            size.y.ceil() as i32,
        ];
        if self.geometry != Some(geometry) {
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos2(
                geometry[0] as f32,
                geometry[1] as f32,
            )));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(
                geometry[2] as f32,
                geometry[3] as f32,
            )));
            self.geometry = Some(geometry);
        }
        if !hidden {
            if view == NotchView::Normal
                && media
                && !self.hover
                && self.playback != Playback::Playing
            {
                if let Some(age) = pause_age {
                    ctx.request_repaint_after(
                        Duration::from_secs(self.settings.pause_linger_secs) - age,
                    );
                }
            }
            if self.settings.artwork_bg {
                let moving = self.cover.is_some()
                    && media
                    && self.playback == Playback::Playing
                    && !self.settings.static_bg;
                if moving {
                    self.fluid_time += dt;
                }
                if moving
                    || self.background.previous.is_some()
                    || (self.cover.is_some() && self.background.black_mix > 0.0)
                {
                    ctx.request_repaint_after(ANIMATION_FRAME);
                } else if self.background.black_mix < 1.0 {
                    if let Some(since) = self.background.missing_since {
                        let hold = (since + BACKGROUND_HOLD).saturating_duration_since(now);
                        ctx.request_repaint_after(if hold.is_zero() {
                            ANIMATION_FRAME
                        } else {
                            hold
                        });
                    }
                }
            }
        }
        size
    }

    fn size(&self) -> Vec2 {
        let normal = vec2(360.0, 40.0);
        let mini = vec2(MINI_SIZE[0], MINI_SIZE[1]);
        let expanded = vec2(416.0, 136.0);
        normal + (mini - normal) * self.mini + (expanded - normal) * self.expand
    }

    fn render_notch(
        &mut self,
        ui: &mut egui::Ui,
        size: Vec2,
        now: Instant,
        media: bool,
        bands: [f32; 3],
    ) {
        let rect = Rect::from_min_size(Pos2::ZERO, size);
        let painter = ui.painter().clone();
        let mut pin_exclusions = Vec::new();
        let radius = 10.0 - 4.0 * self.mini + 6.0 * self.expand;
        if self.settings.artwork_bg {
            self.background
                .paint(&painter, rect, self.fluid_time, now, radius);
        } else {
            Theme::default().paint_with_radius(&painter, rect, 0.0, 1.0, radius);
        }
        if ui.input(|i| i.pointer.secondary_clicked()) {
            if let Some(tray) = &self.tray {
                tray.show_menu();
            } else {
                self.settings_open = true;
            }
        }
        let has_timeline = media && self.snapshot.duration > 0.0;
        let frac = if has_timeline {
            (self.position / self.snapshot.duration).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        let playing = media && self.playback == Playback::Playing;
        let h = self.expand;
        let mini_alpha = self.mini;
        if mini_alpha > 0.001 {
            let show_progress = self.settings.mini_progress && has_timeline;
            if playing && show_progress && self.position < self.snapshot.duration {
                ui.ctx().request_repaint_after(PROGRESS_FRAME);
            }
            let meter_x = if show_progress {
                let bar = Rect::from_min_max(pos2(16.0, 4.9), pos2(size.x - 22.0, 7.1));
                painter.rect_filled(bar, 1.1, white(0.22 * mini_alpha));
                if let Some(fill) = progress_fill(bar, frac) {
                    painter.rect_filled(fill, 1.1, white(0.9 * mini_alpha));
                }
                size.x - 16.5
            } else {
                size.x / 2.0 - 3.2
            };
            paint_spectrum(&painter, pos2(meter_x, 6.0), bands, 6.0, mini_alpha);
        }
        if !media {
            let alpha = ((1.0 - self.mini - 0.12) / 0.58).clamp(0.0, 1.0);
            if alpha > 0.001 {
                let center_y = size.y / 2.0;
                pin_exclusions.push(Rect::from_min_max(pos2(8.0, 0.0), pos2(104.0, size.y)));
                if self.settings.show_clock {
                    pin_exclusions.push(Rect::from_min_max(
                        pos2(size.x - 76.0, 0.0),
                        pos2(size.x, size.y),
                    ));
                    self.paint_clock(&painter, size.x - 14.0, center_y, alpha, now);
                }
                painter.text(
                    pos2(18.0, center_y),
                    egui::Align2::CENTER_CENTER,
                    "♪",
                    FontId::proportional(18.0),
                    white(0.76 * alpha),
                );
                painter.text(
                    pos2(34.0, center_y),
                    egui::Align2::LEFT_CENTER,
                    self.settings.language.text("Нет медиа", "No media"),
                    FontId::proportional(12.0),
                    white(0.84 * alpha),
                );
            }
            self.toggle_pin_on_background_click(ui, rect, &pin_exclusions, now);
            return;
        }
        let alpha = ((1.0 - self.mini - 0.25) / 0.55).clamp(0.0, 1.0);
        if alpha < 0.01 {
            self.toggle_pin_on_background_click(ui, rect, &pin_exclusions, now);
            return;
        }
        let cy = lerp(20.0, 31.0, h);
        let side = lerp(26.0, 42.0, h);
        let cover_rect = Rect::from_min_size(pos2(13.0, cy - side / 2.0), vec2(side, side));
        pin_exclusions.push(cover_rect);
        if self.settings.show_clock {
            pin_exclusions.push(Rect::from_min_max(
                pos2(size.x - 78.0, 0.0),
                pos2(size.x, 44.0),
            ));
            self.paint_clock(&painter, size.x - 14.0, cy, alpha, now);
        }
        if let Some(cover) = &self.cover {
            egui::Image::new((cover.id(), cover_rect.size()))
                .corner_radius(lerp(4.0, 7.0, h))
                .tint(white(alpha))
                .paint_at(ui, cover_rect);
        } else {
            painter.rect_filled(cover_rect, lerp(4.0, 7.0, h), white(0.09 * alpha));
            painter.text(
                cover_rect.center(),
                egui::Align2::CENTER_CENTER,
                "♪",
                FontId::proportional(19.0),
                white(0.5 * alpha),
            );
        }
        let right = size.x - 14.0;
        let text_right = if self.settings.show_clock {
            right - 75.0
        } else {
            right
        };
        let text_rect =
            Rect::from_min_max(pos2(cover_rect.right() + 11.0, 0.0), pos2(text_right, 57.0));
        let mut title_rect = text_rect;
        if playing {
            title_rect.max.x -= 20.0;
        }
        let title_y = cy - 8.0 * h;
        let title_hit_rect =
            text_hit_rect(&painter, &self.snapshot.title, title_rect, title_y, 12.6);
        pin_exclusions.push(title_hit_rect.expand2(vec2(2.0, 2.0)));
        let artist_rect = Rect::from_min_max(
            pos2(text_rect.left(), cy + 2.0),
            pos2(text_rect.right(), cy + 18.0),
        );
        let artist_hit_rect = text_hit_rect(
            &painter,
            &self.snapshot.artist,
            artist_rect,
            artist_rect.center().y,
            11.0,
        );
        if h > 0.02 {
            pin_exclusions.push(artist_hit_rect.expand2(vec2(2.0, 2.0)));
        }
        let source_enabled =
            self.settings.open_source && !self.demo && !self.snapshot.app_id.trim().is_empty();
        let source_id = egui::Id::new((self.snapshot.session, self.snapshot.track));
        let cover_response = source_enabled.then(|| {
            ui.interact(cover_rect, source_id.with("source-cover"), Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand)
        });
        let title_response =
            (source_enabled && !self.snapshot.title.trim().is_empty()).then(|| {
                ui.interact(
                    title_hit_rect,
                    source_id.with("source-title"),
                    Sense::click(),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand)
            });
        let artist_response =
            (source_enabled && h > 0.5 && !self.snapshot.artist.trim().is_empty()).then(|| {
                ui.interact(
                    artist_hit_rect,
                    source_id.with("source-artist"),
                    Sense::click(),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand)
            });
        if cover_response
            .as_ref()
            .is_some_and(|response| response.hovered())
        {
            painter.rect_filled(cover_rect, lerp(4.0, 7.0, h), white(0.10 * alpha));
        }
        if title_response
            .as_ref()
            .is_some_and(|response| response.hovered())
        {
            painter.rect_filled(title_hit_rect, 3.0, white(0.08 * alpha));
        }
        if artist_response
            .as_ref()
            .is_some_and(|response| response.hovered())
        {
            painter.rect_filled(artist_hit_rect, 3.0, white(0.08 * alpha));
        }
        draw_title(
            &painter,
            &self.snapshot.title,
            title_rect,
            title_y,
            alpha,
            self.settings.scroll_title && h > 0.9,
            now.duration_since(self.hover_since).as_secs_f32(),
        );
        if playing {
            paint_spectrum(
                &painter,
                pos2(title_hit_rect.right() + 8.0, title_y - 1.0),
                bands,
                11.0,
                alpha,
            );
        }
        if h > 0.02 {
            elided_text(
                &painter,
                &self.snapshot.artist,
                artist_rect,
                11.0,
                white(0.80 * alpha * h),
            );
        }
        let source_clicked = [&cover_response, &title_response, &artist_response]
            .into_iter()
            .flatten()
            .any(clicked_inside);
        if source_clicked {
            let id = self.snapshot.app_id.clone();
            std::thread::spawn(move || {
                if let Err(e) = platform::open_source_app(&id) {
                    crate::append_log(&e);
                }
            });
        }
        let mut scrubbed = false;
        if h > 0.02 {
            if playing && self.position < self.snapshot.duration {
                ui.ctx().request_repaint_after(PROGRESS_FRAME);
            }
            let bar = Rect::from_min_max(pos2(16.0, 69.5), pos2(size.x - 16.0, 73.5));
            let visible = rect.intersect(ui.clip_rect());
            let seek_hit = seek_hit_rect(bar, visible);
            pin_exclusions.push(seek_hit);
            pin_exclusions.push(Rect::from_min_max(pos2(12.0, 77.0), pos2(58.0, 91.0)));
            pin_exclusions.push(Rect::from_min_max(
                pos2(size.x - 62.0, 77.0),
                pos2(size.x - 12.0, 91.0),
            ));
            let mut over = false;
            if let Some((min, max)) = self
                .snapshot
                .seek_range
                .filter(|_| visible.contains(bar.center()))
            {
                let response =
                    ui.interact(seek_hit, source_id.with("seek"), Sense::click_and_drag());
                if let Some((x, commit)) =
                    scrub_input(ui, response.interact_rect, &mut self.dragging)
                {
                    scrubbed = true;
                    let target = (f64::from(((x - bar.left()) / bar.width()).clamp(0.0, 1.0))
                        * self.snapshot.duration)
                        .clamp(min, max);
                    self.position = target;
                    if commit {
                        self.next_seek_id += 1;
                        let id = self.next_seek_id;
                        if self.command(
                            MediaCommand::Seek {
                                seconds: target,
                                id,
                            },
                            now,
                        ) {
                            self.seek_hold = Some((
                                id,
                                now + SEEK_ACK_TIMEOUT,
                                PlaybackPosition::new(target, now, self.snapshot.position.rate()),
                            ));
                        }
                    }
                    ui.ctx().request_repaint();
                }
                over = self.dragging
                    || ui.input(|input| {
                        input
                            .pointer
                            .hover_pos()
                            .is_some_and(|pos| seek_hit.contains(pos))
                    });
                if over {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
            } else {
                self.dragging = false;
            }
            painter.rect_filled(bar, 3.0, white(0.19 * alpha * h));
            let seek_fraction = if self.snapshot.duration > 0.0 {
                (self.position / self.snapshot.duration).clamp(0.0, 1.0) as f32
            } else {
                0.0
            };
            let fill = progress_fill(bar, seek_fraction);
            if let Some(fill) = fill {
                painter.rect_filled(fill, 3.0, white(0.92 * alpha * h));
            }
            if over {
                let thumb_x = fill.map_or(bar.left(), |fill| fill.right());
                painter.circle_filled(
                    pos2(thumb_x, bar.center().y),
                    SEEK_THUMB_RADIUS,
                    white(alpha * h),
                );
            }
            painter.text(
                pos2(16.0, 84.0),
                egui::Align2::LEFT_CENTER,
                format_time(self.position),
                FontId::proportional(10.0),
                white(0.61 * alpha * h),
            );
            painter.text(
                pos2(size.x - 16.0, 84.0),
                egui::Align2::RIGHT_CENTER,
                format_time(self.snapshot.duration),
                FontId::proportional(10.0),
                white(0.61 * alpha * h),
            );
            let controls_alpha = alpha * ((h - 0.18) / 0.82).clamp(0.0, 1.0);
            let shuffle = self.snapshot.shuffle;
            let repeat = self.snapshot.repeat;
            for (index, icon) in ["shuffle", "previous", "play", "next", "repeat"]
                .iter()
                .enumerate()
            {
                let center = pos2(
                    size.x / 2.0 + (index as f32 - 2.0) * 40.0,
                    lerp(80.0, 110.0, h),
                );
                let hit = transport_hit_rect(center, seek_hit);
                pin_exclusions.push(hit);
                let enabled = match *icon {
                    "previous" => self.snapshot.can_previous,
                    "next" => self.snapshot.can_next,
                    "shuffle" => self.snapshot.can_shuffle,
                    "repeat" => self.snapshot.can_repeat,
                    _ if playing => self.snapshot.can_pause,
                    _ => self.snapshot.can_play,
                };
                let response = (h > 0.5 && enabled).then(|| {
                    ui.interact(
                        hit,
                        egui::Id::new((self.snapshot.session, *icon)),
                        Sense::click(),
                    )
                });
                let hover = response
                    .as_ref()
                    .is_some_and(|r| r.hovered() && r.contains_pointer());
                let active = (*icon == "shuffle" && shuffle) || (*icon == "repeat" && repeat > 0);
                if *icon == "play" {
                    painter.circle_filled(
                        center,
                        if hover { 16.8 } else { 16.0 },
                        white(0.95 * controls_alpha),
                    );
                } else if hover || active {
                    painter.circle_filled(
                        center,
                        13.5,
                        white(if active {
                            0.18 * controls_alpha
                        } else {
                            0.12 * controls_alpha
                        }),
                    );
                }
                let scale = if response
                    .as_ref()
                    .is_some_and(egui::Response::is_pointer_button_down_on)
                {
                    0.90
                } else {
                    1.0
                };
                draw_icon(
                    &painter,
                    if *icon == "play" {
                        if playing { "pause" } else { "play" }
                    } else {
                        icon
                    },
                    center,
                    scale,
                    if *icon == "play" {
                        Color32::from_rgba_unmultiplied(18, 18, 22, (255.0 * controls_alpha) as u8)
                    } else {
                        white(
                            controls_alpha
                                * if enabled {
                                    if active { 1.0 } else { 0.85 }
                                } else {
                                    0.33
                                },
                        )
                    },
                );
                if active {
                    painter.circle_filled(center + vec2(0.0, 10.0), 1.4, white(controls_alpha));
                }
                if *icon == "repeat" && repeat == 2 {
                    painter.text(
                        center,
                        egui::Align2::CENTER_CENTER,
                        "1",
                        FontId::proportional(7.0),
                        white(controls_alpha),
                    );
                }
                if !scrubbed && response.as_ref().is_some_and(clicked_inside) {
                    let command = match *icon {
                        "previous" => MediaCommand::Previous,
                        "next" => MediaCommand::Next,
                        "shuffle" => MediaCommand::Shuffle(!shuffle),
                        "repeat" => MediaCommand::Repeat((repeat + 1) % 3),
                        _ if playing => MediaCommand::Pause,
                        _ => MediaCommand::Play,
                    };
                    self.command(command, now);
                    ui.ctx().request_repaint();
                }
            }
        }
        if !scrubbed {
            self.toggle_pin_on_background_click(ui, rect, &pin_exclusions, now);
        }
    }

    fn paint_clock(
        &mut self,
        painter: &egui::Painter,
        right: f32,
        cy: f32,
        alpha: f32,
        now: Instant,
    ) {
        if self.clock.as_ref().is_none_or(|(next, _, _)| now >= *next) {
            let (clock, date, remaining) = platform::clock_strings(self.settings.language);
            self.clock = Some((now + remaining, clock, date));
        }
        let (next, clock, date) = self.clock.as_ref().unwrap();
        painter
            .ctx()
            .request_repaint_after(next.saturating_duration_since(now));
        let clock_y = if self.settings.show_date {
            cy - 6.8
        } else {
            cy
        };
        painter.text(
            pos2(right, clock_y),
            egui::Align2::RIGHT_CENTER,
            clock.as_str(),
            FontId::new(13.0, egui::FontFamily::Name("SF Pro Display".into())),
            white(0.97 * alpha),
        );
        if self.settings.show_date {
            painter.text(
                pos2(right, cy + 7.0),
                egui::Align2::RIGHT_CENTER,
                date.as_str(),
                FontId::proportional(9.6),
                white(0.70 * alpha),
            );
        }
    }

    fn toggle_pin_on_background_click(
        &mut self,
        ui: &egui::Ui,
        bounds: Rect,
        exclusions: &[Rect],
        now: Instant,
    ) {
        let click_pos = ui.input(|input| {
            input
                .pointer
                .primary_clicked()
                .then(|| input.pointer.interact_pos())
                .flatten()
        });
        if click_pos.is_some_and(|pos| {
            bounds.contains(pos) && !exclusions.iter().any(|rect| rect.contains(pos))
        }) {
            self.pinned = !self.pinned;
            if self.pinned {
                self.hover = true;
                self.hover_since = now;
                self.left_at = None;
            }
            ui.ctx().request_repaint();
        }
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut close = false;
        let mut retry_save = false;
        let previous = self.settings.clone();
        let mut auto = self.autostart_enabled;
        let media_apps = self
            .media
            .as_ref()
            .map(MediaBackend::apps)
            .unwrap_or_default();
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("settings"),
            egui::ViewportBuilder::default()
                .with_title(
                    self.settings
                        .language
                        .text("Настройки — PlayerNotch", "Settings — PlayerNotch"),
                )
                .with_inner_size([690.0, 670.0])
                .with_min_inner_size([520.0, 350.0]),
            |ctx, _| {
                if ctx.input(|i| i.viewport().close_requested()) {
                    close = true;
                    return;
                }
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::new()
                            .fill(settings_ui::PAGE_COLOR)
                            .inner_margin(20.0),
                    )
                    .show(ctx, |ui| {
                        settings_ui::column(ui, |ui| {
                            let warning = Color32::from_rgb(255, 160, 160);
                            if let Some(error) = &self.save_error {
                                ui.colored_label(
                                    warning,
                                    format!(
                                        "{} {error}",
                                        self.settings.language.text(
                                            "Настройки не сохранены.",
                                            "Settings were not saved."
                                        )
                                    ),
                                );
                                retry_save = ui
                                    .button(
                                        self.settings
                                            .language
                                            .text("Повторить сохранение", "Retry saving"),
                                    )
                                    .clicked();
                            }
                            if let Some(error) = &self.autostart_error {
                                ui.colored_label(
                                    warning,
                                    format!(
                                        "{} {error}",
                                        self.settings
                                            .language
                                            .text("Ошибка автозапуска.", "Autostart error.")
                                    ),
                                );
                            }
                            if let Some(error) = &self.tray_error {
                                ui.colored_label(
                                    warning,
                                    format!(
                                        "{} {error}",
                                        self.settings.language.text(
                                            "Значок в трее недоступен.",
                                            "System tray icon is unavailable."
                                        )
                                    ),
                                );
                            }
                            if let Some(error) = &self.snapshot.error {
                                let message = if self.settings.language
                                    == crate::settings::Language::English
                                {
                                    error
                                        .replace(
                                            "Не удалось запустить управление медиа:",
                                            "Could not start media controls:",
                                        )
                                        .replace(
                                            "Не удалось инициализировать управление медиа Windows",
                                            "Could not initialize Windows media controls",
                                        )
                                        .replace(
                                            "Медиасессии Windows недоступны:",
                                            "Windows media sessions are unavailable:",
                                        )
                                        .replace(
                                            "Список медиасессий Windows недоступен:",
                                            "Windows media session list is unavailable:",
                                        )
                                } else {
                                    error.clone()
                                };
                                ui.weak(message);
                            }
                            settings_ui::show(
                                ui,
                                &mut self.settings,
                                &mut self.autostart_mini,
                                &mut auto,
                                self.tray.is_some(),
                                &media_apps,
                            );
                        });
                    });
            },
        );
        if auto != self.autostart_enabled {
            self.autostart_error = platform::set_autostart(auto).err();
            if self.autostart_error.is_none() {
                self.autostart_enabled = auto;
            }
        }
        if self.settings != previous || retry_save {
            self.save_error = self.settings.save().err();
        }
        if self.settings != previous {
            if self.settings.priority_app != previous.priority_app {
                if let Some(media) = &self.media {
                    media.set_priority_app(self.settings.priority_app.clone());
                }
            }
            ctx.request_repaint();
        }
        if close {
            self.settings_open = false;
        }
    }

    fn command(&mut self, command: MediaCommand, now: Instant) -> bool {
        if let Some(media) = &self.media {
            media.command(&self.snapshot, command, now)
        } else if self.demo {
            match command {
                MediaCommand::Play => self.snapshot.playback = Playback::Playing,
                MediaCommand::Pause => self.snapshot.playback = Playback::Paused,
                MediaCommand::Seek { id, .. } => self.snapshot.handled_seek = id,
                MediaCommand::Shuffle(value) => self.snapshot.shuffle = value,
                MediaCommand::Repeat(value) => self.snapshot.repeat = value,
                MediaCommand::Previous | MediaCommand::Next => return false,
            }
            self.snapshot.position = PlaybackPosition::new(
                self.position,
                now,
                f64::from(self.snapshot.playback == Playback::Playing),
            );
            true
        } else {
            false
        }
    }
}

impl eframe::App for NotchApp {
    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f32();
        self.last_frame = now;
        if let Some(tray) = &self.tray {
            for event in tray.events() {
                match event {
                    TrayEvent::Settings => {
                        self.autostart_enabled = platform::autostart_enabled();
                        self.settings_open = true;
                        ctx.send_viewport_cmd_to(
                            egui::ViewportId::from_hash_of("settings"),
                            egui::ViewportCommand::Focus,
                        );
                    }
                    TrayEvent::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                }
            }
        }
        let media = self.receive_media(ctx, now, dt);
        let size = self.animate(ctx, now, dt.min(0.1), media);
        let bands = if !self.hidden && media && self.playback == Playback::Playing {
            ctx.request_repaint_after(Duration::from_millis(32));
            let t = now.duration_since(self.started).as_secs_f32();
            std::array::from_fn(|i| {
                0.25 + 0.65 * (t * (3.0 + i as f32 * 0.7) + i as f32).sin().abs()
            })
        } else {
            [0.0; 3]
        };
        if !self.hidden {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ctx, |ui| self.render_notch(ui, size, now, media, bands));
        }
        self.settings_window(ctx);
        if let Some(path) = std::env::var_os("CLOCK_DEBUG_SHOT") {
            if !self.screenshot_requested {
                let deadline = self.started + Duration::from_millis(3500);
                if now >= deadline {
                    self.screenshot_requested = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
                } else {
                    ctx.request_repaint_after(deadline - now);
                }
            }
            for event in ctx.input(|i| i.events.clone()) {
                if let egui::Event::Screenshot { image, .. } = event {
                    let bytes: Vec<u8> = image
                        .pixels
                        .iter()
                        .flat_map(|c| c.to_srgba_unmultiplied())
                        .collect();
                    if let Err(e) = image::save_buffer(
                        std::path::PathBuf::from(&path),
                        &bytes,
                        image.size[0] as u32,
                        image.size[1] as u32,
                        image::ColorType::Rgba8,
                    ) {
                        crate::append_log(&e.to_string());
                    }
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        if let Some((_, until, _)) = self.seek_hold {
            ctx.request_repaint_after(until.saturating_duration_since(now));
        }
    }
}

fn notch_origin(desktop: platform::DesktopInfo, size: Vec2, position: NotchPosition) -> Pos2 {
    let free_width = (desktop.size[0] - size.x).max(0.0);
    let margin = 12.0_f32.min(free_width / 2.0);
    let x = match position {
        NotchPosition::Left => margin,
        NotchPosition::Center => free_width / 2.0,
        NotchPosition::Right => free_width - margin,
    };
    pos2(desktop.origin[0] + x, desktop.origin[1])
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "SF Pro Text".into(),
        egui::FontData::from_static(include_bytes!("../fonts/SF-Pro-Text-Regular.otf")).into(),
    );
    fonts.font_data.insert(
        "SF Pro Display".into(),
        egui::FontData::from_static(include_bytes!("../fonts/SF-Pro-Display-Semibold.otf")).into(),
    );
    fonts.font_data.insert(
        "SF Pro Text Medium".into(),
        egui::FontData::from_static(include_bytes!("../fonts/SF-Pro-Text-Medium.otf")).into(),
    );
    fonts.font_data.insert(
        "SF Pro Display Medium".into(),
        egui::FontData::from_static(include_bytes!("../fonts/SF-Pro-Display-Medium.otf")).into(),
    );
    fonts.families.insert(
        egui::FontFamily::Name("SF Pro Display".into()),
        vec!["SF Pro Display".into(), "SF Pro Text".into()],
    );
    fonts.families.insert(
        egui::FontFamily::Name("SF Pro Text Medium".into()),
        vec!["SF Pro Text Medium".into(), "SF Pro Text".into()],
    );
    fonts.families.insert(
        egui::FontFamily::Name("SF Pro Display Medium".into()),
        vec!["SF Pro Display Medium".into(), "SF Pro Text".into()],
    );
    fonts
        .families
        .get_mut(&egui::FontFamily::Proportional)
        .unwrap()
        .insert(0, "SF Pro Text".into());
    ctx.set_fonts(fonts);
}

fn elided_text(painter: &egui::Painter, text: &str, rect: Rect, size: f32, color: Color32) {
    let mut job = egui::text::LayoutJob::simple_singleline(
        text.to_owned(),
        FontId::proportional(size),
        color,
    );
    job.wrap.max_width = rect.width();
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let galley = painter.layout_job(job);
    painter.with_clip_rect(rect).galley(
        pos2(rect.left(), rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
}

fn clicked_inside(response: &egui::Response) -> bool {
    // egui's hit-test radius can assign a click just outside the widget to it.
    response.clicked_by(egui::PointerButton::Primary)
        && response
            .interact_pointer_pos()
            .is_some_and(|pos| response.interact_rect.contains(pos))
}

fn transport_hit_rect(center: Pos2, seek_hit: Rect) -> Rect {
    let mut hit = Rect::from_center_size(center, vec2(40.0, 36.0));
    hit.min.y = hit.min.y.max(seek_hit.bottom() + 1.0);
    hit
}

fn seek_hit_rect(bar: Rect, visible: Rect) -> Rect {
    // Use the thumb's diameter for the target height and include both end positions.
    let diameter = 2.0 * SEEK_THUMB_RADIUS;
    Rect::from_center_size(
        bar.center(),
        vec2(bar.width() + diameter, bar.height().max(diameter)),
    )
    .intersect(visible)
}

fn progress_fill(bar: Rect, fraction: f32) -> Option<Rect> {
    let end = bar.left() + fraction * bar.width();
    (end > bar.left()).then(|| Rect::from_min_max(bar.min, pos2(end, bar.bottom())))
}

fn paint_spectrum(painter: &egui::Painter, first: Pos2, bands: [f32; 3], height: f32, alpha: f32) {
    for (index, level) in bands.into_iter().enumerate() {
        let x = first.x + index as f32 * 3.2;
        let bar_height = lerp(2.0, height, level);
        painter.rect_filled(
            Rect::from_center_size(pos2(x, first.y), vec2(2.0, bar_height)),
            1.0,
            white(0.88 * alpha),
        );
    }
}

fn scrub_input(ui: &egui::Ui, hit: Rect, dragging: &mut bool) -> Option<(f32, bool)> {
    use egui::PointerButton::Primary;
    ui.input(|input| {
        let mut release = None;
        for event in &input.events {
            match *event {
                egui::Event::WindowFocused(false) => {
                    *dragging = false;
                    return None;
                }
                egui::Event::PointerButton {
                    pos,
                    button: Primary,
                    pressed: true,
                    ..
                } => {
                    // Hit-test the current layout even while the notch is resizing.
                    *dragging = hit.contains(pos);
                }
                egui::Event::PointerButton {
                    pos,
                    button: Primary,
                    pressed: false,
                    ..
                } if *dragging => {
                    *dragging = false;
                    release = Some(pos.x);
                }
                _ => {}
            }
        }
        *dragging &= input.pointer.primary_down();
        if let Some(x) = release {
            return Some((x, true));
        }
        if *dragging {
            return input.pointer.interact_pos().map(|pos| (pos.x, false));
        }
        None
    })
}

fn text_hit_rect(
    painter: &egui::Painter,
    text: &str,
    bounds: Rect,
    center_y: f32,
    size: f32,
) -> Rect {
    let trimmed = text.trim();
    let leading_bytes = text.len() - text.trim_start().len();
    let leading_width = if leading_bytes > 0 {
        painter
            .layout_no_wrap(
                text[..leading_bytes].to_owned(),
                FontId::proportional(size),
                Color32::WHITE,
            )
            .size()
            .x
    } else {
        0.0
    };
    let galley = painter.layout_no_wrap(
        trimmed.to_owned(),
        FontId::proportional(size),
        Color32::WHITE,
    );
    let left = bounds.left() + leading_width;
    let width = galley.size().x.min((bounds.right() - left).max(0.0));
    Rect::from_center_size(
        pos2(left + width / 2.0, center_y),
        vec2(width, galley.size().y.min(bounds.height())),
    )
}

fn draw_title(
    painter: &egui::Painter,
    title: &str,
    rect: Rect,
    y: f32,
    alpha: f32,
    expanded: bool,
    elapsed: f32,
) {
    let galley = painter.layout_no_wrap(title.to_owned(), FontId::proportional(12.6), white(alpha));
    let extra = galley.size().x - rect.width();
    let clip = Rect::from_center_size(pos2(rect.center().x, y), vec2(rect.width(), 20.0));
    if expanded && extra > 0.0 {
        let run = extra / 32.0;
        let u = elapsed % (2.0 * (run + 0.9));
        let (offset, pause) = if u < 0.9 {
            (0.0, 0.9 - u)
        } else if u < 0.9 + run {
            ((u - 0.9) * 32.0, 0.0)
        } else if u < 1.8 + run {
            (extra, 1.8 + run - u)
        } else {
            ((extra - (u - 1.8 - run) * 32.0).max(0.0), 0.0)
        };
        painter.ctx().request_repaint_after(if pause > 0.0 {
            Duration::from_secs_f32(pause)
        } else {
            ANIMATION_FRAME
        });
        painter.with_clip_rect(clip).galley(
            pos2(rect.left() - offset, y - galley.size().y / 2.0),
            galley,
            white(alpha),
        );
    } else {
        elided_text(painter, title, clip, 12.6, white(alpha));
    }
}

fn draw_icon(p: &egui::Painter, name: &str, c: Pos2, scale: f32, color: Color32) {
    let at = |x: f32, y: f32| c + vec2(x, y) * scale;
    let line = |points: &[[f32; 2]]| {
        p.add(egui::Shape::line(
            points.iter().map(|v| at(v[0], v[1])).collect(),
            Stroke::new(1.8_f32, color),
        ))
    };
    let triangle = |points: &[[f32; 2]]| {
        p.add(egui::Shape::convex_polygon(
            points.iter().map(|v| at(v[0], v[1])).collect(),
            color,
            Stroke::NONE,
        ))
    };
    match name {
        "play" => {
            triangle(&[[-3.5, -7.0], [6.5, 0.0], [-3.5, 7.0]]);
        }
        "pause" => {
            for x in [-4.0, 3.5] {
                p.rect_filled(
                    Rect::from_center_size(at(x, 0.0), vec2(3.0, 13.0) * scale),
                    0.7,
                    color,
                );
            }
        }
        "previous" | "next" => {
            let sign = if name == "next" { 1.0 } else { -1.0 };
            for x in [-4.0, 3.0] {
                triangle(&[
                    [(x - 4.0) * sign, -6.0],
                    [(x + 3.0) * sign, 0.0],
                    [(x - 4.0) * sign, 6.0],
                ]);
            }
            line(&[[8.0 * sign, -6.0], [8.0 * sign, 6.0]]);
        }
        "shuffle" => {
            line(&[[-8.0, -5.0], [-5.0, -5.0], [4.0, 5.0], [8.0, 5.0]]);
            line(&[[-8.0, 5.0], [-5.0, 5.0], [-2.0, 2.0]]);
            line(&[[2.0, -2.0], [5.0, -5.0], [8.0, -5.0]]);
            line(&[[5.0, -8.0], [8.0, -5.0], [5.0, -2.0]]);
            line(&[[5.0, 2.0], [8.0, 5.0], [5.0, 8.0]]);
        }
        "repeat" => {
            line(&[[-7.0, 1.0], [-7.0, -3.0], [-5.0, -5.0], [7.0, -5.0]]);
            line(&[[4.0, -8.0], [7.0, -5.0], [4.0, -2.0]]);
            line(&[[7.0, -1.0], [7.0, 3.0], [5.0, 5.0], [-7.0, 5.0]]);
            line(&[[-4.0, 8.0], [-7.0, 5.0], [-4.0, 2.0]]);
        }
        _ => {}
    }
}

fn approach(current: f32, target: f32, rate: f32, dt: f32) -> f32 {
    let value = current + (target - current) * (1.0 - (-dt * rate).exp());
    if (value - target).abs() < 0.002 {
        target
    } else {
        value
    }
}
fn notch_view(
    media: bool,
    playback: Playback,
    pause_age: Option<Duration>,
    mini: bool,
    hover: bool,
    settings: &Settings,
) -> NotchView {
    let playing_or_recent_pause = playback == Playback::Playing
        || pause_age.is_some_and(|age| age < Duration::from_secs(settings.pause_linger_secs));
    match (media, hover) {
        (true, true) => NotchView::Expanded,
        (false, _) if hover || settings.idle_view == IdleView::Clock => NotchView::Normal,
        (true, false) if !mini && playing_or_recent_pause => NotchView::Normal,
        _ => NotchView::Mini,
    }
}
fn has_media(snapshot: &MediaSnapshot) -> bool {
    let title = snapshot.title.trim();
    snapshot.active
        && !title.is_empty()
        && !(matches!(
            title.to_lowercase().as_str(),
            "not playing" | "не исполняется"
        ) && snapshot.artist.trim().is_empty())
}
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
fn white(alpha: f32) -> Color32 {
    Color32::from_white_alpha((alpha.clamp(0.0, 1.0) * 255.0) as u8)
}
fn format_time(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}
fn demo_snapshot() -> MediaSnapshot {
    MediaSnapshot {
        active: true,
        session: 1,
        playback: Playback::Playing,
        title: "PlayerNotch — Rust".into(),
        artist: "Предпросмотр интерфейса".into(),
        app_id: "demo".into(),
        position: PlaybackPosition::new(42.0, Instant::now(), 1.0),
        duration: 240.0,
        can_play: true,
        can_pause: true,
        can_shuffle: true,
        can_repeat: true,
        seek_range: Some((0.0, 240.0)),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn background_holds_its_color_before_fading_to_black() {
        let mut background = Background::new();
        let now = Instant::now();
        background.update(false, now, BACKGROUND_FADE);
        assert_eq!(background.black_mix, 1.0);

        background.update(true, now, BACKGROUND_FADE);
        background.update(false, now, 0.0);
        background.update(false, now + Duration::from_millis(999), 0.016);
        assert_eq!(background.black_mix, 0.0);

        let fade_started = now + BACKGROUND_HOLD;
        background.update(false, fade_started, BACKGROUND_FADE / 2.0);
        assert_eq!(background.black_mix, 0.5);
        background.update(
            false,
            fade_started + Duration::from_millis(325),
            BACKGROUND_FADE / 2.0,
        );
        assert_eq!(background.black_mix, 1.0);
    }

    #[test]
    fn artwork_resets_the_hold_and_reverses_an_active_black_fade() {
        let mut background = Background::new();
        let now = Instant::now();
        background.update(true, now, BACKGROUND_FADE);
        background.update(false, now, 0.0);
        background.update(false, now + BACKGROUND_HOLD, BACKGROUND_FADE / 2.0);
        assert_eq!(background.black_mix, 0.5);

        let artwork_returned = now + Duration::from_millis(1250);
        background.update(true, artwork_returned, BACKGROUND_FADE / 4.0);
        assert_eq!(background.black_mix, 0.25);
        background.update(false, artwork_returned + Duration::from_millis(16), 0.016);
        background.update(false, artwork_returned + Duration::from_millis(999), 0.016);
        assert_eq!(background.black_mix, 0.25);
    }

    #[test]
    fn animation_converges_without_overshoot() {
        let mut value = 0.0;
        for _ in 0..120 {
            value = approach(value, 1.0, 13.0, 1.0 / 60.0);
            assert!((0.0..=1.0).contains(&value));
        }
        assert_eq!(value, 1.0);
    }
    #[test]
    fn time_format_handles_negative_and_long_tracks() {
        assert_eq!(format_time(-1.0), "0:00");
        assert_eq!(format_time(3671.0), "61:11");
    }

    #[test]
    fn source_click_requires_pointer_inside_widget() {
        let rect = Rect::from_min_max(pos2(20.0, 20.0), pos2(60.0, 40.0));
        for (pos, expected) in [(pos2(30.0, 30.0), true), (pos2(61.0, 30.0), false)] {
            let ctx = egui::Context::default();
            for pressed in [None, Some(true), Some(false)] {
                let mut input = egui::RawInput::default();
                if let Some(pressed) = pressed {
                    input.events.push(egui::Event::PointerMoved(pos));
                    input.events.push(egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
                let _ = ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let response = ui.interact(rect, ui.id().with("source"), Sense::click());
                        if pressed == Some(false) {
                            assert!(response.clicked());
                            assert_eq!(clicked_inside(&response), expected);
                        }
                    });
                });
            }
        }
    }

    #[test]
    fn seek_hitbox_clicks_and_drags_only_with_a_primary_press_inside() {
        let bar = Rect::from_min_max(pos2(16.0, 69.5), pos2(400.0, 73.5));
        let rect = seek_hit_rect(bar, Rect::from_min_size(Pos2::ZERO, vec2(416.0, 136.0)));
        let start = pos2(210.0, 72.0);
        let left_edge = pos2(11.0, 72.0);
        let outside = pos2(430.0, 72.0);
        let nearby = pos2(210.0, rect.top() - 0.5);
        let button = |pos, button, pressed| egui::Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        use egui::PointerButton::{Primary, Secondary};
        let ctx = egui::Context::default();
        let mut dragging = false;
        let mut frame = |events, expected, expected_dragging| {
            let _ = ctx.run(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        assert_eq!(scrub_input(ui, rect, &mut dragging), expected);
                        assert_eq!(dragging, expected_dragging);
                    });
                },
            );
        };
        frame(
            vec![
                button(left_edge, Primary, true),
                button(left_edge, Primary, false),
            ],
            Some((left_edge.x, true)),
            false,
        );
        frame(
            vec![
                egui::Event::PointerMoved(start),
                button(start, Primary, true),
            ],
            Some((start.x, false)),
            true,
        );
        frame(
            vec![button(start, Primary, false)],
            Some((start.x, true)),
            false,
        );
        frame(
            vec![button(start, Primary, true)],
            Some((start.x, false)),
            true,
        );
        frame(
            vec![egui::Event::PointerMoved(outside)],
            Some((outside.x, false)),
            true,
        );
        frame(
            vec![button(outside, Primary, false)],
            Some((outside.x, true)),
            false,
        );
        frame(
            vec![
                egui::Event::PointerMoved(nearby),
                button(nearby, Primary, true),
            ],
            None,
            false,
        );
        frame(
            vec![
                egui::Event::PointerMoved(start),
                button(start, Primary, false),
            ],
            None,
            false,
        );
        frame(vec![button(start, Secondary, true)], None, false);
        frame(vec![button(start, Secondary, false)], None, false);
    }

    #[test]
    fn transport_hitboxes_stay_below_seek_during_expansion() {
        let bar = Rect::from_min_max(pos2(16.0, 69.5), pos2(400.0, 73.5));
        let seek = seek_hit_rect(bar, Rect::from_min_size(Pos2::ZERO, vec2(416.0, 136.0)));
        assert_eq!(
            seek,
            Rect::from_min_max(pos2(10.5, 66.0), pos2(405.5, 77.0))
        );
        for step in 51..=100 {
            let center = pos2(208.0, lerp(80.0, 110.0, step as f32 / 100.0));
            let hit = transport_hit_rect(center, seek);
            assert!(hit.contains(center));
            assert!(!hit.intersects(seek));
        }
    }

    #[test]
    fn progress_starts_empty_and_grows_without_a_minimum_fill() {
        let bar = Rect::from_min_size(pos2(16.0, 69.5), vec2(384.0, 4.0));
        assert_eq!(progress_fill(bar, 0.0), None);
        let fill = progress_fill(bar, 0.005).unwrap();
        assert!(fill.width() > 0.0 && fill.width() < bar.height());
        assert_eq!(progress_fill(bar, 1.0), Some(bar));
    }

    #[test]
    fn embedded_font_renders_cyrillic() {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.label("Настройки — PlayerNotch");
            });
        });
        assert!(!ctx.tessellate(output.shapes, 1.0).is_empty());
    }
    #[test]
    fn notch_view_follows_media_hover_and_mini_setting() {
        use NotchView::{Expanded, Mini, Normal};

        let settings = Settings::default();
        for mini in [false, true] {
            assert_eq!(
                notch_view(false, Playback::Stopped, None, mini, false, &settings),
                Mini
            );
            assert_eq!(
                notch_view(false, Playback::Stopped, None, mini, true, &settings),
                Normal
            );

            let cases = [
                (Playback::Playing, None, Normal),
                (Playback::Paused, Some(Duration::from_secs(9)), Normal),
                (Playback::Paused, Some(Duration::from_secs(10)), Mini),
                (Playback::Stopped, Some(Duration::from_secs(60)), Mini),
            ];
            for (playback, pause_age, resting_view) in cases {
                assert_eq!(
                    notch_view(true, playback, pause_age, mini, false, &settings),
                    if mini { Mini } else { resting_view },
                );
                assert_eq!(
                    notch_view(true, playback, pause_age, mini, true, &settings),
                    Expanded
                );
            }
        }
    }

    #[test]
    fn idle_clock_and_custom_pause_linger_control_the_resting_view() {
        let settings = Settings {
            idle_view: IdleView::Clock,
            pause_linger_secs: 20,
            ..Settings::default()
        };
        assert_eq!(
            notch_view(false, Playback::Stopped, None, true, false, &settings),
            NotchView::Normal
        );
        for (age, view) in [(19, NotchView::Normal), (20, NotchView::Mini)] {
            assert_eq!(
                notch_view(
                    true,
                    Playback::Paused,
                    Some(Duration::from_secs(age)),
                    false,
                    false,
                    &settings
                ),
                view
            );
        }
    }
    #[test]
    fn idle_placeholder_and_missing_session_have_no_media() {
        let mut sample = demo_snapshot();
        sample.title = "Not Playing".into();
        sample.artist.clear();
        assert!(!has_media(&sample));
        assert!(!has_media(&MediaSnapshot::default()));

        sample.title = "Browser video".into();
        sample.duration = 0.0;
        assert!(has_media(&sample));
    }
}
