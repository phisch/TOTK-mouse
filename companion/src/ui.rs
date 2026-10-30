//! The companion window: status, camera settings, shortcuts and mouse bindings.

use crate::config::{Action, Config, SENS_MAX, SENS_MIN};
use crate::session::{Command, Shared};
use gpui::{
    anchored, canvas, deferred, div, prelude::*, px, relative, rgb, size, Anchor, App, Bounds, Context,
    Div, FocusHandle, FontWeight, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Rgba,
    SharedString, Stateful, TitlebarOptions, Window, WindowBounds, WindowOptions,
};
use gpui_platform::application;
use std::borrow::Cow;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;
use std::time::Duration;

const MOUSE_INPUTS: [(&str, &str); 7] = [
    ("left", "Left click"),
    ("right", "Right click"),
    ("middle", "Middle click"),
    ("back", "Back"),
    ("forward", "Forward"),
    ("wheel_up", "Wheel up"),
    ("wheel_down", "Wheel down"),
];
const SWITCH_BUTTONS: [&str; 16] =
    ["A", "B", "X", "Y", "L", "R", "ZL", "ZR", "Plus", "Minus", "L3", "R3", "Up", "Down", "Left", "Right"];

const SANS: &str = "IBM Plex Sans";
const MONO: &str = "Lilex";

fn bg() -> Rgba { rgb(0x000000) }
fn card_bg() -> Rgba { rgb(0x0e0e0e) }
fn border() -> Rgba { rgb(0x262626) }
fn divider() -> Rgba { rgb(0x1c1c1c) }
fn field() -> Rgba { rgb(0x161616) }
fn hover() -> Rgba { rgb(0x1f1f1f) }
fn text() -> Rgba { rgb(0xf2f2f2) }
fn muted() -> Rgba { rgb(0x9a9a9a) }
fn faint() -> Rgba { rgb(0x6b6b6b) }
fn accent() -> Rgba { rgb(0x34d399) }
fn warn() -> Rgba { rgb(0xfbbf24) }
fn idle() -> Rgba { rgb(0x555555) }

pub fn run(shared: Shared, tx: Sender<Command>, session: JoinHandle<()>) {
    let mut session = Some(session);
    application().run(move |cx: &mut App| {
        let fonts: Vec<Cow<'static, [u8]>> = vec![
            Cow::Borrowed(include_bytes!("../assets/fonts/ibm-plex-sans/IBMPlexSans-Regular.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/ibm-plex-sans/IBMPlexSans-SemiBold.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/lilex/Lilex-Regular.ttf")),
        ];
        if let Err(e) = cx.text_system().add_fonts(fonts) {
            eprintln!("cannot load fonts: {e}");
        }
        let bounds = Bounds::centered(None, size(px(1080.), px(880.)), cx);
        let opts = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { title: Some("TOTK Mouse".into()), ..Default::default() }),
            app_id: Some("totk-mouse".into()),
            window_min_size: Some(size(px(860.), px(600.))),
            ..Default::default()
        };
        // Quit with the session (the emulator exited), window or not.
        let s = shared.clone();
        cx.spawn(async move |cx| loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            if s.lock().unwrap().finished {
                let _ = cx.update(|cx| cx.quit());
                break;
            }
        })
        .detach();

        let (s, t) = (shared.clone(), tx.clone());
        let opened = cx.open_window(opts, |window, cx| {
            let view = cx.new(|cx| Companion::new(s, t, cx));
            let focus = view.read(cx).focus.clone();
            window.focus(&focus, cx);
            view
        });
        if let Err(e) = opened {
            // Mouse support does not need the window, so keep going without it.
            crate::show_error(&format!(
                "The window could not be opened: {e:#}\n\nMouse support keeps running until the emulator exits."
            ));
            return;
        }

        // Closing the window ends the companion, and with it mouse support.
        let tx = tx.clone();
        let mut session = session.take();
        cx.on_window_closed(move |cx, _| {
            let _ = tx.send(Command::Quit);
            if let Some(h) = session.take() {
                let _ = h.join();
            }
            cx.quit();
        })
        .detach();
        cx.activate(true);
    });
}

struct Companion {
    shared: Shared,
    tx: Sender<Command>,
    focus: FocusHandle,
    /// Mouse input whose binding menu is open.
    editing: Option<&'static str>,
    /// Set when a click outside closed the menu, so the same click landing on the
    /// menu's own badge does not open it again right away.
    closed_by_outside: Option<&'static str>,
    /// Action waiting for a key press.
    capturing_key: Option<Action>,
    /// Track bounds per slider, captured during paint, for turning clicks into values.
    slider_bounds: [Rc<Cell<Bounds<Pixels>>>; 2],
    dragging: Option<Slider>,
}

#[derive(Clone, Copy, PartialEq)]
enum Slider {
    Sensitivity,
    Interval,
}

impl Slider {
    fn index(self) -> usize {
        self as usize
    }
}

const INTERVAL_MIN: u64 = 1;
const INTERVAL_MAX: u64 = 33;

fn sens_to_frac(s: f64) -> f32 {
    ((s / SENS_MIN).ln() / (SENS_MAX / SENS_MIN).ln()).clamp(0.0, 1.0) as f32
}

fn frac_to_sens(f: f32) -> f64 {
    SENS_MIN * (SENS_MAX / SENS_MIN).powf(f.clamp(0.0, 1.0) as f64)
}

/// Normalises a GPUI key name to what the X11 backend understands.
fn key_name(key: &str) -> Option<String> {
    let name = if let Some(n) = key.strip_prefix('f').filter(|n| n.parse::<u32>().is_ok()) {
        format!("F{n}")
    } else {
        key.to_string()
    };
    crate::input::key_supported(&name).then_some(name)
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

impl Companion {
    fn new(shared: Shared, tx: Sender<Command>, cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let alive = this.update(cx, |_, cx| cx.notify());
            if alive.is_err() {
                break;
            }
        })
        .detach();
        Self {
            shared,
            tx,
            focus: cx.focus_handle(),
            editing: None,
            closed_by_outside: None,
            capturing_key: None,
            slider_bounds: [Rc::new(Cell::new(Bounds::default())), Rc::new(Cell::new(Bounds::default()))],
            dragging: None,
        }
    }

    fn apply(&self, cx: &mut Context<Self>, f: impl FnOnce(&mut Config)) {
        let cfg = {
            let mut st = self.shared.lock().unwrap();
            f(&mut st.config);
            st.config.clone()
        };
        let _ = self.tx.send(Command::SetConfig(cfg));
        cx.notify();
    }

    fn set_slider_at(&self, slider: Slider, x: Pixels, cx: &mut Context<Self>) {
        let b = self.slider_bounds[slider.index()].get();
        let w = f32::from(b.size.width);
        if w <= 0.0 {
            return;
        }
        let f = (f32::from(x - b.origin.x) / w).clamp(0.0, 1.0);
        match slider {
            Slider::Sensitivity => {
                let s = frac_to_sens(f);
                self.apply(cx, |c| {
                    // Keeps the ratio between the axes.
                    let ratio = c.sensitivity_y / c.sensitivity_x;
                    let round = |v: f64| (v * 1e7).round() / 1e7;
                    c.sensitivity_x = round(s);
                    c.sensitivity_y = round((s * ratio).clamp(SENS_MIN, SENS_MAX));
                });
            }
            Slider::Interval => {
                let ms = INTERVAL_MIN + (f * (INTERVAL_MAX - INTERVAL_MIN) as f32).round() as u64;
                self.apply(cx, |c| c.write_interval_ms = ms);
            }
        }
    }

    fn slider(&self, slider: Slider, frac: f32, cx: &mut Context<Self>) -> Stateful<Div> {
        let bounds_cell = self.slider_bounds[slider.index()].clone();
        div()
            .id(("slider", slider.index()))
            .relative()
            .h(px(24.))
            .w_full()
            .cursor_pointer()
            .child(div().absolute().top(px(10.)).left_0().right_0().h(px(4.)).rounded_full().bg(border()))
            .child(div().absolute().top(px(10.)).left_0().h(px(4.)).w(relative(frac)).rounded_full().bg(text()))
            .child(
                div().absolute().top(px(1.)).left(relative(frac)).ml(px(-11.)).size(px(22.)).rounded_full()
                    .bg(text()).border_4().border_color(bg()),
            )
            .child(canvas(move |b, _, _| bounds_cell.set(b), |_, _, _, _| {}).absolute().size_full())
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                this.dragging = Some(slider);
                this.set_slider_at(slider, ev.position.x, cx);
            }))
            .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, cx| {
                if this.dragging == Some(slider) && ev.pressed_button == Some(MouseButton::Left) {
                    this.set_slider_at(slider, ev.position.x, cx);
                } else if ev.pressed_button.is_none() {
                    this.dragging = None;
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, _| this.dragging = None))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _, _, _| this.dragging = None))
    }

    fn on_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(action) = self.capturing_key else { return };
        match ev.keystroke.key.as_str() {
            "escape" => {}
            "backspace" | "delete" => self.apply(cx, |c| c.bind_key(action, None)),
            k => match key_name(k) {
                Some(name) => self.apply(cx, |c| c.bind_key(action, Some(name))),
                None => return,
            },
        }
        self.capturing_key = None;
        cx.notify();
    }
}

fn card() -> Div {
    div().flex().flex_col().rounded_xl().border_1().border_color(border()).bg(card_bg())
}

fn card_title(t: &str) -> Div {
    div().text_base().font_weight(FontWeight::SEMIBOLD).text_color(text()).child(t.to_string())
}

/// A settings row: label on the left, control on the right, divider below unless last.
fn setting_row(label: impl Into<SharedString>, last: bool) -> Div {
    let r = div()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .py_3()
        .child(div().text_lg().text_color(muted()).child(label.into()));
    if last { r } else { r.border_b_1().border_color(divider()) }
}

fn dot(color: Rgba) -> Div {
    div().size(px(8.)).rounded_full().bg(color)
}

fn status_cell(label: &str, color: Rgba, value: &str, last: bool) -> Div {
    let c = div()
        .flex_1()
        .flex()
        .flex_col()
        .gap_2()
        .px_8()
        .py_6()
        .child(div().text_base().text_color(muted()).child(label.to_string()))
        .child(div().flex().items_center().gap_3().text_lg().child(dot(color)).child(value.to_string()));
    if last { c } else { c.border_r_1().border_color(divider()) }
}

/// A binding badge: solid when bound, dashed "Unbound" otherwise, accented while
/// waiting for input.
fn badge(id: impl Into<SharedString>, value: Option<&str>, active: bool) -> Stateful<Div> {
    let b = div()
        .id(gpui::ElementId::Name(id.into()))
        .min_w(px(100.))
        .h(px(44.))
        .px_4()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .font_family(MONO)
        .text_base()
        .cursor_pointer()
        .hover(|s| s.bg(hover()));
    let b = match value {
        Some(v) => b.border_color(border()).text_color(text()).child(v.to_string()),
        None => b.border_dashed().border_color(border()).text_color(faint()).child("Unbound"),
    };
    if active { b.border_color(accent()) } else { b }
}

fn outline_button(id: &'static str, label: &str, on: bool) -> Stateful<Div> {
    div()
        .id(id)
        .px_5()
        .py_2()
        .rounded_md()
        .border_1()
        .border_color(if on { text() } else { border() })
        .bg(if on { text() } else { card_bg() })
        .text_color(if on { bg() } else { text() })
        .text_base()
        .cursor_pointer()
        .when(!on, |s| s.hover(|s| s.bg(hover())))
        .child(label.to_string())
}

fn menu_item(id: impl Into<SharedString>, label: &str, active: bool) -> Stateful<Div> {
    div()
        .id(gpui::ElementId::Name(id.into()))
        .px_3()
        .py_1()
        .rounded_md()
        .text_base()
        .cursor_pointer()
        .bg(if active { text() } else { card_bg() })
        .text_color(if active { bg() } else { text() })
        .when(!active, |s| s.hover(|s| s.bg(hover())))
        .child(label.to_string())
}

impl Render for Companion {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (cfg, status, emulator, input, notices) = {
            let st = self.shared.lock().unwrap();
            (st.config.clone(), st.status, st.emulator.clone(), st.input, st.notices.clone())
        };

        // Header with the mouselook switch.
        let toggle_key = cfg.key_for(Action::Toggle).map(str::to_string);
        let switch = div()
            .id("mouselook-switch")
            .w(px(56.))
            .h(px(30.))
            .p(px(3.))
            .flex()
            .items_center()
            .rounded_full()
            .cursor_pointer()
            .bg(if status.enabled { accent() } else { rgb(0x333333) })
            .when(status.enabled, |s| s.justify_end())
            .child(div().size(px(24.)).rounded_full().bg(if status.enabled { bg() } else { muted() }))
            .on_click(cx.listener(|this, _, _, _| {
                let _ = this.tx.send(Command::ToggleMouselook);
            }));
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).child("TOTK Mouse"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .px_6()
                    .py_3()
                    .rounded_xl()
                    .border_1()
                    .border_color(border())
                    .bg(card_bg())
                    .child(div().text_lg().child("Mouselook"))
                    .when_some(toggle_key, |s, k| {
                        s.child(
                            div().px_2().rounded_md().border_1().border_color(border())
                                .font_family(MONO).text_sm().text_color(muted()).child(k),
                        )
                    })
                    .child(switch),
            );

        // Status.
        let (game_color, game_value) =
            if status.connected { (accent(), "Connected") } else { (warn(), "Waiting for TOTK") };
        let (cam_color, cam_value) = match (status.connected, status.camera_active) {
            (false, _) => (idle(), "-"),
            (true, true) => (accent(), "In game"),
            (true, false) => (idle(), "Paused"),
        };
        let (mouse_color, mouse_value) = if status.grabbed { (accent(), "Locked") } else { (idle(), "Free") };
        let emulator_name = std::path::Path::new(&emulator)
            .file_name()
            .map(|n| capitalize(&n.to_string_lossy()))
            .unwrap_or_default();
        let info = |label: &str, value: Div| {
            div().flex().items_center().gap_2().child(div().text_base().text_color(muted()).child(label.to_string())).child(value)
        };
        let status_card = card()
            .child(
                div()
                    .flex()
                    .child(status_cell("Game", game_color, game_value, false))
                    .child(status_cell("Camera", cam_color, cam_value, false))
                    .child(status_cell("Mouse", mouse_color, mouse_value, true)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_8()
                    .px_8()
                    .py_5()
                    .border_t_1()
                    .border_color(divider())
                    .child(info("Emulator", div().text_base().child(emulator_name)))
                    .child(info("Input", div().text_base().child(input)))
                    .child(info(
                        "Game",
                        div().flex().items_center().gap_4()
                            .child(div().text_base().child("Tears of the Kingdom"))
                            .child(div().font_family(MONO).text_sm().text_color(muted()).child("0100F2C0115B6000"))
                            .when(status.connected, |s| {
                                s.child(div().font_family(MONO).text_sm().text_color(muted()).child("1.4.1"))
                            }),
                    )),
            );

        // Camera.
        let sens_slider = self.slider(Slider::Sensitivity, sens_to_frac(cfg.sensitivity_x), cx);
        let interval = cfg.write_interval_ms.clamp(INTERVAL_MIN, INTERVAL_MAX);
        let interval_frac = (interval - INTERVAL_MIN) as f32 / (INTERVAL_MAX - INTERVAL_MIN) as f32;
        let interval_slider = self.slider(Slider::Interval, interval_frac, cx);
        let value_box = |text: String| {
            div().px_3().py_1().rounded_md().border_1().border_color(border()).bg(field())
                .font_family(MONO).text_base().child(text)
        };
        let camera_card = card()
            .gap_4()
            .p_8()
            .child(card_title("Camera"))
            .child(
                div().flex().items_center().justify_between()
                    .child(div().text_lg().text_color(muted()).child("Sensitivity"))
                    .child(value_box(format!("{:.5}", cfg.sensitivity_x))),
            )
            .child(sens_slider)
            .child(
                div().flex().items_center().justify_between()
                    .child(div().text_lg().text_color(muted()).child("Update interval"))
                    .child(value_box(format!("{interval} ms"))),
            )
            .child(interval_slider)
            .child(div().text_sm().text_color(faint())
                .child("How often mouse movement is sent to the game. Lower feels snappier, higher is lighter on the emulator."))
            .child(
                div().flex().items_center().justify_between()
                    .child(div().text_lg().text_color(muted()).child("Invert"))
                    .child(
                        div().flex().gap_2()
                            .child(outline_button("invert-x", "Horizontal", cfg.invert_x)
                                .on_click(cx.listener(|this, _, _, cx| this.apply(cx, |c| c.invert_x = !c.invert_x))))
                            .child(outline_button("invert-y", "Vertical", cfg.invert_y)
                                .on_click(cx.listener(|this, _, _, cx| this.apply(cx, |c| c.invert_y = !c.invert_y)))),
                    ),
            );

        // Shortcuts.
        let mut shortcuts = card().p_8().pb_4().child(div().pb_2().child(card_title("Shortcuts")));
        for (i, action) in Action::ALL.into_iter().enumerate() {
            let capturing = self.capturing_key == Some(action);
            let value = if capturing { Some("Press a key") } else { cfg.key_for(action) };
            shortcuts = shortcuts.child(setting_row(action.label(), i == Action::ALL.len() - 1).child(
                badge(format!("key-{}", action.name()), value, capturing).on_click(cx.listener(move |this, _, window, cx| {
                    this.capturing_key = if this.capturing_key == Some(action) { None } else { Some(action) };
                    this.editing = None;
                    window.focus(&this.focus, cx);
                    cx.notify();
                })),
            ));
        }
        if self.capturing_key.is_some() {
            shortcuts = shortcuts.child(
                div().pt_1().text_sm().text_color(faint()).child("Esc cancels, Backspace unbinds."),
            );
        }

        // Mouse buttons.
        let mut mouse = card().p_8().pb_4().child(div().pb_2().child(card_title("Mouse buttons")));
        for (i, (id, label)) in MOUSE_INPUTS.into_iter().enumerate() {
            let target = cfg.buttons.get(id).cloned();
            let action = target.as_deref().and_then(Action::parse);
            let shown = match (&target, action) {
                (_, Some(a)) => Some(a.label().to_string()),
                (Some(t), None) => Some(t.clone()),
                (None, _) => None,
            };
            let open = self.editing == Some(id);
            let mut control = div().relative().child(
                badge(format!("map-{id}"), shown.as_deref(), open).on_click(cx.listener(move |this, _, _, cx| {
                    if this.closed_by_outside.take() == Some(id) {
                        return;
                    }
                    this.editing = if this.editing == Some(id) { None } else { Some(id) };
                    this.capturing_key = None;
                    cx.notify();
                })),
            );
            if open {
                let mut actions = div().flex().flex_col().gap_1();
                actions = actions.child(menu_item(format!("{id}-none"), "Unbound", target.is_none()).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.apply(cx, |c| { c.buttons.remove(id); });
                        this.editing = None;
                    }),
                ));
                for a in Action::ALL {
                    actions = actions.child(menu_item(format!("{id}-{}", a.name()), a.label(), action == Some(a)).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.apply(cx, |c| { c.buttons.insert(id.to_string(), a.name().to_string()); });
                            this.editing = None;
                        }),
                    ));
                }
                let mut menu = div()
                    .id(gpui::ElementId::Name(format!("menu-{id}").into()))
                    .w(px(280.))
                    .mt_2()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(border())
                    .bg(card_bg())
                    .shadow_lg()
                    .on_mouse_down_out(cx.listener(move |this, _, _, cx| {
                        this.editing = None;
                        this.closed_by_outside = Some(id);
                        cx.notify();
                    }))
                    .child(div().text_sm().text_color(faint()).child("Action"))
                    .child(actions);
                {
                    let mut grid = div().flex().flex_wrap().gap_1();
                    for sb in SWITCH_BUTTONS {
                        let active = action.is_none() && target.as_deref().is_some_and(|t| t.eq_ignore_ascii_case(sb));
                        grid = grid.child(
                            menu_item(format!("{id}-{sb}"), sb, active).font_family(MONO).min_w(px(56.)).flex().justify_center()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.apply(cx, |c| { c.buttons.insert(id.to_string(), sb.to_string()); });
                                    this.editing = None;
                                })),
                        );
                    }
                    menu = menu.child(div().text_sm().text_color(faint()).child("Switch button")).child(grid);
                }
                control = control.child(
                    div().absolute().top_full().right_0().child(
                        deferred(anchored().anchor(Anchor::TopRight).snap_to_window_with_margin(px(8.)).child(menu))
                            .priority(1),
                    ),
                );
            }
            mouse = mouse.child(setting_row(label, i == MOUSE_INPUTS.len() - 1).child(control));
        }

        let mut root = div()
            .id("root")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| this.on_key(ev, cx)))
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_4()
            .px_5()
            .py_6()
            .bg(bg())
            .font_family(SANS)
            .text_color(text())
            .child(header)
            .child(status_card);
        if !notices.is_empty() {
            let mut n = card().gap_2().px_8().py_5().border_color(rgb(0x4a3a12));
            for msg in notices {
                n = n.child(div().text_base().text_color(warn()).child(msg));
            }
            root = root.child(n);
        }
        root.child(
            div()
                .flex()
                .gap_4()
                .items_start()
                .child(div().flex_1().flex().flex_col().gap_4().child(camera_card).child(shortcuts))
                .child(div().flex_1().child(mouse)),
        )
    }
}
