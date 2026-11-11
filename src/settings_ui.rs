use crate::settings::{IdleView, Language, NotchPosition, Settings};
use eframe::egui::{
    self, Align, Color32, FontFamily, FontId, Layout, Pos2, Rect, RichText, Sense, Stroke,
    StrokeKind, Vec2, pos2, vec2,
};

pub const PAGE_COLOR: Color32 = Color32::from_gray(28);
const CARD_COLOR: Color32 = Color32::from_gray(38);
const TEXT_COLOR: Color32 = Color32::from_gray(233);
const MUTED_COLOR: Color32 = Color32::from_gray(168);
const ACCENT: Color32 = Color32::from_rgb(71, 145, 246);
const MAX_COLUMN_WIDTH: f32 = 840.0;
const CONTROL_WIDTH: f32 = 168.0;
const SWITCH_WIDTH: f32 = 40.0;
const ICON_WIDTH: f32 = 24.0;
const ICON_GAP: f32 = 14.0;
const CONTROL_GAP: f32 = 20.0;
const SCROLLBAR_WIDTH: f32 = 6.0;
const SCROLLBAR_GAP: f32 = 6.0;

#[derive(Clone, Copy)]
enum Icon {
    Notch,
    Screen,
    Pointer,
    Timer,
    Clock,
    Artwork,
    Media,
    Power,
}

pub fn column(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    let available = ui.available_rect_before_wrap();
    let width = available.width().min(MAX_COLUMN_WIDTH);
    // The scrollbar occupies space on the right; center the cards themselves when there is room.
    let center_shift =
        ((available.width() - width) / 2.0).min((SCROLLBAR_WIDTH + SCROLLBAR_GAP) / 2.0);
    let bounds = Rect::from_min_size(
        pos2(
            available.center().x - width / 2.0 + center_shift,
            available.top(),
        ),
        vec2(width, available.height()),
    );
    ui.scope_builder(egui::UiBuilder::new().max_rect(bounds), |ui| {
        configure_style(ui.style_mut());
        contents(ui);
    });
}

fn configure_style(style: &mut egui::Style) {
    style.visuals.override_text_color = Some(TEXT_COLOR);
    style.visuals.weak_text_color = Some(MUTED_COLOR);
    style.visuals.extreme_bg_color = PAGE_COLOR;
    style.visuals.disabled_alpha = 0.65;
    let widgets = &mut style.visuals.widgets;
    for (visuals, fill, border) in [
        (&mut widgets.inactive, 49, Color32::from_gray(66)),
        (&mut widgets.hovered, 58, Color32::from_gray(84)),
        (&mut widgets.active, 56, ACCENT),
        (&mut widgets.open, 55, Color32::from_gray(94)),
    ] {
        visuals.weak_bg_fill = Color32::from_gray(fill);
        visuals.bg_fill = visuals.weak_bg_fill;
        visuals.bg_stroke = Stroke::new(1.0_f32, border);
        visuals.fg_stroke = Stroke::new(1.0_f32, TEXT_COLOR);
        visuals.corner_radius = 6.into();
        visuals.expansion = 0.0;
    }
    style.spacing.button_padding = vec2(10.0, 5.0);
    style.spacing.interact_size.y = 32.0;
    style.spacing.item_spacing.y = 8.0;
    let scroll = &mut style.spacing.scroll;
    scroll.floating = false;
    scroll.foreground_color = false;
    scroll.bar_width = SCROLLBAR_WIDTH;
    scroll.bar_inner_margin = SCROLLBAR_GAP;
}

pub fn show(
    ui: &mut egui::Ui,
    settings: &mut Settings,
    autostart_mini: &mut bool,
    autostart: &mut bool,
    can_hide: bool,
    media_apps: &[String],
) {
    let lang = settings.language;
    ui.label(RichText::new(lang.text("Настройки", "Settings")).font(header_font(30.0)));
    ui.add_space(12.0);
    egui::ScrollArea::vertical()
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            choice_card(
                ui, Icon::Screen, lang.text("Язык", "Language"),
                lang.text("Язык интерфейса приложения.", "Application interface language."),
                &mut settings.language,
                &[(Language::Russian, "Русский"), (Language::English, "English")],
                |_| true,
            );
            let mut mini = settings.mini_notch || *autostart_mini;
            if toggle_card(
                ui,
                Icon::Notch,
                lang.text("Мини-чёлка", "Mini notch"),
                lang.text("В покое чёлка остаётся тонкой даже во время воспроизведения.", "Keep the notch thin while idle, even during playback."),
                &mut mini,
            ) {
                settings.mini_notch = mini;
                *autostart_mini = false;
            }
            choice_card(
                ui,
                Icon::Notch,
                lang.text("Без активного медиа", "No active media"),
                if can_hide {
                    lang.text("Что показывать, когда нет активного трека или видео.", "Choose what to show when no track or video is active.")
                } else {
                    lang.text("Скрытие доступно при работающем значке в трее.", "Hiding is available when the tray icon is running.")
                },
                &mut settings.idle_view,
                &[
                    (IdleView::Mini, lang.text("Мини-чёлка", "Mini notch")),
                    (IdleView::Clock, lang.text("Обычный вид", "Standard view")),
                    (IdleView::Hidden, lang.text("Скрывать", "Hide")),
                ],
                |value| value != IdleView::Hidden || can_hide,
            );
            ui.add_enabled_ui(can_hide, |ui| {
                toggle_card(
                    ui,
                    Icon::Screen,
                    lang.text("Скрывать в полноэкранном режиме", "Hide in fullscreen"),
                    lang.text("Работает для игр и видео на основном мониторе.", "Works for games and videos on the primary monitor."),
                    &mut settings.hide_fullscreen,
                );
            });
            choice_card(
                ui,
                Icon::Screen,
                lang.text("Положение на экране", "Screen position"),
                lang.text("Расположение чёлки у верхнего края основного монитора.", "Position the notch along the top of the primary monitor."),
                &mut settings.position,
                &[
                    (NotchPosition::Left, lang.text("Слева", "Left")),
                    (NotchPosition::Center, lang.text("По центру", "Center")),
                    (NotchPosition::Right, lang.text("Справа", "Right")),
                ],
                |_| true,
            );
            toggle_card(
                ui,
                Icon::Pointer,
                lang.text("Раскрывать при наведении", "Expand on hover"),
                lang.text("Щелчком по фону можно закрепить чёлку или снять закрепление.", "Click the background to pin or unpin the notch."),
                &mut settings.open_on_hover,
            );
            ui.add_enabled_ui(settings.open_on_hover, |ui| {
                delay_card(
                    ui,
                    lang.text("Задержка раскрытия", "Expand delay"),
                    lang.text("Помогает избежать раскрытия при случайном наведении.", "Prevent expansion from accidental hovering."),
                    &mut settings.hover_delay_ms,
                    &[(0, lang.text("Сразу", "Immediately")), (120, lang.text("120 мс", "120 ms")), (250, lang.text("250 мс", "250 ms")), (500, lang.text("500 мс", "500 ms")), (750, lang.text("750 мс", "750 ms"))],
                    lang.text("мс", "ms"),
                );
                delay_card(
                    ui,
                    lang.text("Задержка сворачивания", "Collapse delay"),
                    lang.text("Сколько чёлка остаётся открытой после ухода курсора.", "How long the notch stays open after the pointer leaves."),
                    &mut settings.collapse_delay_ms,
                    &[(0, lang.text("Сразу", "Immediately")), (180, lang.text("180 мс", "180 ms")), (350, lang.text("350 мс", "350 ms")), (600, lang.text("600 мс", "600 ms")), (1000, lang.text("1 с", "1 s"))],
                    lang.text("мс", "ms"),
                );
            });
            ui.add_enabled_ui(!mini, |ui| {
                delay_card(
                    ui,
                    lang.text("Сворачивание после паузы", "Collapse after pausing"),
                    lang.text("Сколько обычный вид остаётся на экране после паузы. Работает при выключенной мини-чёлке.", "How long the standard view remains after pausing when the mini notch is off."),
                    &mut settings.pause_linger_secs,
                    &[(0, lang.text("Сразу", "Immediately")), (5, lang.text("5 с", "5 s")), (10, lang.text("10 с", "10 s")), (20, lang.text("20 с", "20 s")), (30, lang.text("30 с", "30 s")), (60, lang.text("1 мин", "1 min"))],
                    lang.text("с", "s"),
                );
            });
            ui.add_space(5.0);
            toggle_card(
                ui,
                Icon::Notch,
                lang.text("Плавное раскрытие", "Smooth resizing"),
                lang.text("Анимировать переход между размерами чёлки.", "Animate transitions between notch sizes."),
                &mut settings.smooth_resize,
            );
            toggle_card(
                ui,
                Icon::Clock,
                lang.text("Часы", "Clock"),
                lang.text("Показывать текущее время в обычном и раскрытом виде.", "Show the current time in standard and expanded views."),
                &mut settings.show_clock,
            );
            ui.add_enabled_ui(settings.show_clock, |ui| {
                toggle_card(
                    ui,
                    Icon::Clock,
                    lang.text("Дата под часами", "Date below clock"),
                    lang.text("Показывать день недели, число и месяц.", "Show the day of the week, day and month."),
                    &mut settings.show_date,
                );
            });
            choice_card(
                ui,
                Icon::Artwork,
                lang.text("Фон чёлки", "Notch background"),
                lang.text("Выберите фон для всех размеров чёлки.", "Choose the background for all notch sizes."),
                &mut settings.artwork_bg,
                &[(true, lang.text("Из обложки", "From artwork")), (false, lang.text("Сплошной чёрный", "Solid black"))],
                |_| true,
            );
            ui.add_enabled_ui(settings.artwork_bg, |ui| {
                let mut animate_gradient = !settings.static_bg;
                if toggle_card(
                    ui,
                    Icon::Artwork,
                    lang.text("Анимация градиента", "Animate gradient"),
                    lang.text("Плавно переливать цвета на фоне.", "Animate the background colors."),
                    &mut animate_gradient,
                ) {
                    settings.static_bg = !animate_gradient;
                }
            });
            card(ui, Icon::Media, lang.text("Приоритетное приложение", "Preferred app"),
                lang.text("При нескольких медиасессиях показывать выбранное приложение. В списке — приложения с активными медиасессиями.", "Prefer this app when several media sessions are active. The list contains apps with active sessions."),
                CONTROL_WIDTH, |ui| {
                    let selected_id = settings.priority_app.clone();
                    let selected = selected_id.as_deref().map(|id| {
                        app_choice_label(id, media_apps.iter().any(|app| app == id), lang)
                    }).unwrap_or_else(|| lang.text("Нет", "None").to_owned());
                    let response = combo_box(ui, &selected, |ui| {
                        if combo_item(ui, settings.priority_app.is_none(), lang.text("Нет", "None"), true).clicked() {
                            settings.priority_app = None;
                        }
                        if let Some(id) = selected_id.as_ref().filter(|id| !media_apps.contains(id)) {
                            let label = app_choice_label(id, false, lang);
                            if combo_item(ui, true, &label, true).on_hover_text(id).clicked() {
                                settings.priority_app = Some(id.clone());
                            }
                        }
                        for id in media_apps {
                            if combo_item(ui, settings.priority_app.as_ref() == Some(id), app_label(id), true)
                                .on_hover_text(id).clicked() {
                                settings.priority_app = Some(id.clone());
                            }
                        }
                    });
                    if let Some(id) = selected_id {
                        response.on_hover_text(id);
                    }
                });
            toggle_card(
                ui,
                Icon::Media,
                lang.text("Прогресс в мини-чёлке", "Progress in mini notch"),
                lang.text("Показывать полоску воспроизведения в тонкой чёлке.", "Show playback progress in the thin notch."),
                &mut settings.mini_progress,
            );
            toggle_card(
                ui,
                Icon::Media,
                lang.text("Прокрутка длинных названий", "Scroll long titles"),
                lang.text("Показывать название трека целиком в раскрытом плеере.", "Show the full track title in the expanded player."),
                &mut settings.scroll_title,
            );
            toggle_card(
                ui,
                Icon::Pointer,
                lang.text("Открывать приложение по щелчку", "Open app on click"),
                lang.text("Щелчок по обложке, названию или исполнителю открывает источник медиа.", "Click artwork, title or artist to open the media source."),
                &mut settings.open_source,
            );
            ui.add_space(5.0);
            toggle_card(
                ui,
                Icon::Power,
                lang.text("Запускать с Windows", "Start with Windows"),
                lang.text("Показывать чёлку автоматически при входе в систему.", "Show the notch automatically when you sign in."),
                autostart,
            );
            ui.add_space(14.0);
            if ui
                .button(RichText::new(lang.text("Сбросить настройки чёлки", "Reset notch settings")).size(13.0))
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                *settings = Settings::default();
                *autostart_mini = false;
            }
            ui.add_space(6.0);
        });
}

fn app_label(id: &str) -> &str {
    let name = id.rsplit(['\\', '/']).next().unwrap_or(id);
    name.split('!').next().unwrap_or(name)
}

fn app_choice_label(id: &str, available: bool, lang: Language) -> String {
    if available {
        app_label(id).to_owned()
    } else {
        format!(
            "{}: {}",
            lang.text("Недоступно", "Unavailable"),
            app_label(id)
        )
    }
}

fn header_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("SF Pro Display Medium".into()))
}

fn title_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("SF Pro Text Medium".into()))
}

fn card<T>(
    ui: &mut egui::Ui,
    icon: Icon,
    title: &str,
    description: &str,
    control_width: f32,
    control: impl FnOnce(&mut egui::Ui) -> T,
) -> T {
    ui.push_id(title, |ui| {
        egui::Frame::new()
            .fill(CARD_COLOR)
            .stroke(Stroke::new(1.0_f32, Color32::from_gray(42)))
            .corner_radius(8)
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
                let width = ui.available_width();
                let text_width =
                    (width - ICON_WIDTH - ICON_GAP - CONTROL_GAP - control_width).max(0.0);
                let title =
                    ui.painter()
                        .layout(title.to_owned(), title_font(15.0), TEXT_COLOR, text_width);
                let description = ui.painter().layout(
                    description.to_owned(),
                    FontId::proportional(13.0),
                    MUTED_COLOR,
                    text_width,
                );
                let text_height = title.size().y + description.size().y + 3.0;
                let row_height = text_height.max(40.0);
                let (row, _) = ui.allocate_exact_size(vec2(width, row_height), Sense::hover());
                draw_icon(
                    ui.painter(),
                    icon,
                    pos2(row.left() + ICON_WIDTH / 2.0, row.center().y),
                );
                let text_rect = Rect::from_min_size(
                    pos2(
                        row.left() + ICON_WIDTH + ICON_GAP,
                        row.center().y - text_height / 2.0,
                    ),
                    vec2(text_width, text_height),
                );
                let mut text_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt("text")
                        .max_rect(text_rect)
                        .layout(Layout::top_down(Align::Min)),
                );
                text_ui.spacing_mut().item_spacing.y = 3.0;
                text_ui.label(title);
                text_ui.label(description);

                let control_rect = Rect::from_min_size(
                    pos2(row.right() - control_width, row.top()),
                    vec2(control_width, row_height),
                );
                let mut control_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt("control")
                        .max_rect(control_rect)
                        .layout(Layout::right_to_left(Align::Center)),
                );
                control(&mut control_ui)
            })
            .inner
    })
    .inner
}

fn toggle_card(
    ui: &mut egui::Ui,
    icon: Icon,
    title: &str,
    description: &str,
    value: &mut bool,
) -> bool {
    card(ui, icon, title, description, SWITCH_WIDTH, |ui| {
        let (rect, mut response) = ui.allocate_exact_size(vec2(SWITCH_WIDTH, 22.0), Sense::click());
        if response.clicked() {
            *value = !*value;
            response.mark_changed();
        }
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *value, title)
        });
        let t = ui.ctx().animate_bool(response.id, *value);
        let (fill, border) = if *value {
            (ACCENT, ACCENT)
        } else if response.hovered() {
            (Color32::from_gray(88), Color32::from_gray(116))
        } else {
            (Color32::from_gray(75), Color32::from_gray(100))
        };
        ui.painter().rect(
            rect,
            11,
            fill,
            Stroke::new(1.0_f32, border),
            StrokeKind::Inside,
        );
        let x = egui::lerp(rect.left() + 11.0..=rect.right() - 11.0, t);
        ui.painter()
            .circle_filled(pos2(x, rect.center().y), 8.0, Color32::WHITE);
        if response.has_focus() {
            ui.painter().rect_stroke(
                rect.expand(3.0),
                14,
                Stroke::new(1.0_f32, ACCENT),
                StrokeKind::Outside,
            );
        }
        response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .changed()
    })
}

fn delay_card(
    ui: &mut egui::Ui,
    title: &str,
    description: &str,
    value: &mut u64,
    choices: &[(u64, &str)],
    unit: &str,
) {
    let fallback = format!("{} {unit}", *value);
    card(ui, Icon::Timer, title, description, CONTROL_WIDTH, |ui| {
        combo(ui, value, choices, &fallback, |_| true)
    });
}

fn choice_card<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    icon: Icon,
    title: &str,
    description: &str,
    value: &mut T,
    choices: &[(T, &str)],
    enabled: impl Fn(T) -> bool,
) {
    card(ui, icon, title, description, CONTROL_WIDTH, |ui| {
        combo(ui, value, choices, "", enabled)
    });
}

fn combo<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    value: &mut T,
    choices: &[(T, &str)],
    fallback: &str,
    enabled: impl Fn(T) -> bool,
) {
    let selected = choices.iter().find(|(choice, _)| choice == value);
    combo_box(ui, selected.map_or(fallback, |(_, label)| *label), |ui| {
        for &(choice, label) in choices {
            if combo_item(ui, *value == choice, label, enabled(choice)).clicked() {
                *value = choice;
            }
        }
    });
}

fn combo_box(
    ui: &mut egui::Ui,
    selected: &str,
    contents: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    egui::ComboBox::from_id_salt("value")
        .width(CONTROL_WIDTH)
        .truncate()
        .selected_text(RichText::new(selected).size(13.0))
        .icon(|ui, rect, visuals, open| {
            let center = rect.center();
            let y = if open { -1.0 } else { 1.0 };
            ui.painter().add(egui::Shape::line(
                vec![
                    center + vec2(-3.5, -1.5 * y),
                    center + vec2(0.0, 2.0 * y),
                    center + vec2(3.5, -1.5 * y),
                ],
                Stroke::new(1.4_f32, visuals.text_color()),
            ));
        })
        .popup_style(egui::style::StyleModifier::new(menu_style))
        .show_ui(ui, contents)
        .response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn combo_item(ui: &mut egui::Ui, selected: bool, label: &str, enabled: bool) -> egui::Response {
    let check = if selected { "✓" } else { "" };
    let button = egui::Button::selectable(selected, RichText::new(label).size(13.0))
        .right_text(RichText::new(check).color(ACCENT).size(14.0))
        .truncate()
        .min_size(vec2(ui.available_width(), 34.0));
    ui.add_enabled(enabled, button)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn menu_style(style: &mut egui::Style) {
    style.spacing.menu_margin = egui::Margin::same(4);
    style.spacing.item_spacing.y = 2.0;
    style.spacing.interact_size.y = 34.0;
    let visuals = &mut style.visuals;
    visuals.menu_corner_radius = 8.into();
    visuals.window_fill = Color32::from_gray(44);
    visuals.window_stroke = Stroke::new(1.0_f32, Color32::from_gray(65));
    visuals.popup_shadow = egui::epaint::Shadow {
        offset: [0, 5],
        blur: 15,
        spread: 1,
        color: Color32::from_black_alpha(115),
    };
    visuals.selection.bg_fill = Color32::from_rgb(49, 62, 79);
    visuals.selection.stroke = Stroke::new(1.0_f32, TEXT_COLOR);
    visuals.widgets.inactive.bg_stroke = Stroke::NONE;
    visuals.widgets.hovered.bg_stroke = Stroke::NONE;
}

fn draw_icon(painter: &egui::Painter, icon: Icon, center: Pos2) {
    let color = Color32::from_gray(211);
    let stroke = Stroke::new(1.4_f32, color);
    let line = |points: &[[f32; 2]]| {
        painter.add(egui::Shape::line(
            points.iter().map(|&[x, y]| center + vec2(x, y)).collect(),
            stroke,
        ));
    };
    let outline = |size: Vec2, radius| {
        painter.rect_stroke(
            Rect::from_center_size(center, size),
            radius,
            stroke,
            StrokeKind::Middle,
        );
    };
    match icon {
        Icon::Notch => {
            outline(vec2(17.0, 10.0), 4);
            painter.circle_filled(center - vec2(4.5, 0.0), 1.15, color);
            line(&[[-1.0, 0.0], [4.5, 0.0]]);
        }
        Icon::Screen => {
            outline(vec2(17.0, 12.0), 2);
            line(&[[0.0, 6.0], [0.0, 9.0]]);
            line(&[[-4.0, 9.0], [4.0, 9.0]]);
        }
        Icon::Pointer => line(&[
            [-6.0, -8.0],
            [7.0, 0.0],
            [1.0, 1.0],
            [-1.0, 7.5],
            [-6.0, -8.0],
        ]),
        Icon::Timer | Icon::Clock => {
            painter.circle_stroke(center, 7.2, stroke);
            line(&[[0.0, -4.0], [0.0, 0.0], [3.0, 1.8]]);
            if matches!(icon, Icon::Timer) {
                line(&[[-2.0, -9.5], [2.0, -9.5]]);
            }
        }
        Icon::Artwork => {
            outline(vec2(17.0, 15.0), 2);
            painter.circle_filled(center + vec2(3.7, -3.4), 1.35, color);
            line(&[
                [-7.5, 5.5],
                [-2.5, -0.5],
                [1.0, 3.0],
                [3.0, 1.0],
                [7.0, 5.5],
            ]);
        }
        Icon::Media => {
            painter.circle_stroke(center, 8.0, stroke);
            line(&[[-2.0, -3.5], [3.8, 0.0], [-2.0, 3.5], [-2.0, -3.5]]);
        }
        Icon::Power => {
            painter.add(egui::Shape::line(
                (0..=24)
                    .map(|i| {
                        let angle = -0.9 + i as f32 / 24.0 * 4.95;
                        center + Vec2::angled(angle) * 7.0
                    })
                    .collect(),
                stroke,
            ));
            line(&[[0.0, -9.0], [0.0, -1.5]]);
        }
    }
}
