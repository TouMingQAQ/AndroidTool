use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Id, Pos2, Rect, Sense, Stroke, Ui, Vec2,
};

use crate::gamepad::{GamepadKind, GamepadVisualState};
use crate::theme::{self, ACCENT, ACCENT_DARK, MUTED, TEXT};

// Controller geometry is expressed in a common 560 x 360 coordinate space.
struct Pad<'a> {
    ui: &'a mut Ui,
    painter: egui::Painter,
    origin: Pos2,
    scale: f32,
    state: &'a GamepadVisualState,
    actions: Vec<String>,
}

impl Pad<'_> {
    fn point(&self, x: f32, y: f32) -> Pos2 {
        self.origin + egui::vec2(x, y) * self.scale
    }

    fn rect(&self, x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_size(self.point(x, y), egui::vec2(w, h) * self.scale)
    }

    fn circle(&self, x: f32, y: f32, radius: f32, color: Color32) {
        self.painter
            .circle_filled(self.point(x, y), radius * self.scale, color);
    }

    fn rounded(&self, rect: Rect, radius: f32, color: Color32) {
        self.painter.rect_filled(
            rect,
            CornerRadius::same((radius * self.scale).clamp(0.0, 255.0) as u8),
            color,
        );
    }

    fn text(&self, x: f32, y: f32, text: &str, size: f32, color: Color32) {
        self.painter.text(
            self.point(x, y),
            Align2::CENTER_CENTER,
            text,
            FontId::proportional(size * self.scale),
            color,
        );
    }

    fn button(
        &mut self,
        key: &'static str,
        label: &str,
        center: (f32, f32),
        size: Vec2,
        color: Color32,
        round: bool,
    ) {
        let rect = self.rect(
            center.0 - size.x / 2.0,
            center.1 - size.y / 2.0,
            size.x,
            size.y,
        );
        let response = self
            .ui
            .interact(rect, Id::new(("pad-button", key)), Sense::click())
            .on_hover_text(format!("{label} · {key}"));
        let trigger_value = match key {
            "left_trigger" => self.state.triggers[0],
            "right_trigger" => self.state.triggers[1],
            _ => 0.0,
        };
        let active = response.is_pointer_button_down_on()
            || self.state.buttons.contains(key)
            || trigger_value > 0.01;
        let fill = if active {
            ACCENT_DARK
        } else if response.hovered() {
            Color32::from_rgb(89, 104, 111)
        } else {
            color
        };
        if round {
            self.circle(center.0, center.1, size.x / 2.0, fill);
        } else {
            self.rounded(rect, 5.0, fill);
        }
        if active || response.hovered() {
            let stroke = Stroke::new(self.scale * 1.5, ACCENT);
            if round {
                self.painter
                    .circle_stroke(rect.center(), rect.width() / 2.0, stroke);
            } else {
                self.painter.rect_stroke(
                    rect,
                    CornerRadius::same((5.0 * self.scale) as u8),
                    stroke,
                    egui::StrokeKind::Inside,
                );
            }
        }
        if !label.is_empty() {
            self.text(
                center.0,
                center.1,
                label,
                if size.x < 25.0 { 11.0 } else { 13.0 },
                if active { Color32::WHITE } else { TEXT },
            );
        }
        if response.is_pointer_button_down_on() {
            self.actions.push(key.to_owned());
        }
    }

    fn stick(&mut self, index: usize, center: (f32, f32), deadzone: f32) {
        let (x, y) = center;
        let rect = self.rect(x - 40.0, y - 40.0, 80.0, 80.0);
        let response =
            self.ui
                .interact(rect, Id::new(("pad-stick", index)), Sense::click_and_drag());
        let mut value = egui::vec2(self.state.sticks[index][0], -self.state.sticks[index][1]);
        if response.is_pointer_button_down_on() {
            if let Some(pos) = response.interact_pointer_pos() {
                value = (pos - self.point(x, y)) / (32.0 * self.scale);
                if value.length() > 1.0 {
                    value = value.normalized();
                }
                if value.length() < deadzone {
                    value = Vec2::ZERO;
                }
                self.actions.push(format!(
                    "__stick_{}:{:.3}:{:.3}",
                    if index == 0 { "left" } else { "right" },
                    value.x,
                    -value.y
                ));
            }
        }
        if response.drag_stopped() {
            self.actions.push(format!(
                "__stick_{}:0:0",
                if index == 0 { "left" } else { "right" }
            ));
        }
        self.circle(x, y, 40.0, Color32::from_rgb(19, 27, 31));
        self.painter.circle_stroke(
            self.point(x, y),
            39.0 * self.scale,
            Stroke::new(self.scale, Color32::from_rgb(102, 118, 121)),
        );
        let knob = self.point(x, y) + value * 15.0 * self.scale;
        let thumb_key = if index == 0 {
            "left_thumb"
        } else {
            "right_thumb"
        };
        let active = value.length() > deadzone || self.state.buttons.contains(thumb_key);
        self.painter.circle_filled(
            knob,
            28.0 * self.scale,
            if active {
                ACCENT_DARK
            } else {
                Color32::from_rgb(54, 66, 69)
            },
        );
        self.painter.circle_stroke(
            knob,
            23.0 * self.scale,
            Stroke::new(
                self.scale * 2.0,
                if active || response.hovered() {
                    ACCENT
                } else {
                    Color32::from_rgb(104, 117, 118)
                },
            ),
        );
        self.painter
            .circle_filled(knob, 16.0 * self.scale, Color32::from_rgb(43, 54, 57));
    }

    fn dpad(&mut self, center: (f32, f32), separate: bool) {
        self.circle(center.0, center.1, 42.0, Color32::from_rgb(27, 36, 40));
        for (key, label, dx, dy) in [
            ("dpad_up", "▲", 0.0, -24.0),
            ("dpad_down", "▼", 0.0, 24.0),
            ("dpad_left", "◀", -24.0, 0.0),
            ("dpad_right", "▶", 24.0, 0.0),
        ] {
            self.button(
                key,
                label,
                (center.0 + dx, center.1 + dy),
                Vec2::splat(23.0),
                Color32::from_rgb(67, 79, 81),
                separate,
            );
        }
        if !separate {
            self.circle(center.0, center.1, 9.0, Color32::from_rgb(67, 79, 81));
        }
    }

    fn face(&mut self, center: (f32, f32), kind: GamepadKind) {
        self.circle(center.0, center.1, 50.0, Color32::from_rgb(27, 36, 40));
        let labels = kind.face_labels();
        for (index, (key, dx, dy)) in [
            ("south", 0.0, 26.0),
            ("east", 26.0, 0.0),
            ("north", 0.0, -26.0),
            ("west", -26.0, 0.0),
        ]
        .into_iter()
        .enumerate()
        {
            self.button(
                key,
                "",
                (center.0 + dx, center.1 + dy),
                Vec2::splat(23.0),
                Color32::from_rgb(65, 77, 79),
                true,
            );
            let color = match index {
                0 => {
                    if kind == GamepadKind::PlayStation {
                        Color32::from_rgb(97, 178, 255)
                    } else {
                        Color32::from_rgb(83, 221, 146)
                    }
                }
                1 => Color32::from_rgb(255, 125, 128),
                2 => {
                    if kind == GamepadKind::PlayStation {
                        Color32::from_rgb(83, 221, 146)
                    } else {
                        Color32::from_rgb(244, 207, 105)
                    }
                }
                _ => {
                    if kind == GamepadKind::PlayStation {
                        Color32::from_rgb(239, 147, 199)
                    } else {
                        Color32::from_rgb(97, 178, 255)
                    }
                }
            };
            self.text(
                center.0 + dx,
                center.1 + dy,
                labels[index],
                if kind == GamepadKind::PlayStation {
                    15.0
                } else {
                    11.0
                },
                color,
            );
        }
    }

    fn shell(&self, ps: bool, fill: Color32, edge: Color32) {
        let curves = if ps {
            [
                [(280.0, 98.0), (224.0, 98.0), (144.0, 85.0), (105.0, 105.0)],
                [(105.0, 105.0), (81.0, 120.0), (47.0, 237.0), (60.0, 299.0)],
                [(60.0, 299.0), (65.0, 324.0), (92.0, 329.0), (107.0, 305.0)],
                [
                    (107.0, 305.0),
                    (135.0, 256.0),
                    (141.0, 238.0),
                    (177.0, 244.0),
                ],
                [
                    (177.0, 244.0),
                    (209.0, 252.0),
                    (245.0, 251.0),
                    (280.0, 251.0),
                ],
            ]
        } else {
            [
                [(280.0, 98.0), (224.0, 98.0), (161.0, 88.0), (124.0, 105.0)],
                [(124.0, 105.0), (87.0, 122.0), (47.0, 240.0), (48.0, 288.0)],
                [(48.0, 288.0), (47.0, 327.0), (80.0, 333.0), (107.0, 312.0)],
                [
                    (107.0, 312.0),
                    (132.0, 294.0),
                    (150.0, 255.0),
                    (178.0, 249.0),
                ],
                [
                    (178.0, 249.0),
                    (210.0, 246.0),
                    (245.0, 250.0),
                    (280.0, 250.0),
                ],
            ]
        };
        let mut points = Vec::new();
        for curve in curves {
            let [a, b, c, d] = curve.map(|(x, y)| self.point(x, y));
            for step in 0..16 {
                let t = step as f32 / 16.0;
                let s = 1.0 - t;
                points.push(
                    Pos2::ZERO
                        + a.to_vec2() * s.powi(3)
                        + b.to_vec2() * 3.0 * s * s * t
                        + c.to_vec2() * 3.0 * s * t * t
                        + d.to_vec2() * t.powi(3),
                );
            }
        }
        points.push(self.point(280.0, if ps { 251.0 } else { 250.0 }));
        let mid = self.point(280.0, 180.0);
        let mirrored: Vec<_> = points
            .iter()
            .rev()
            .map(|p| Pos2::new(2.0 * mid.x - p.x, p.y))
            .collect();
        points.extend(mirrored);
        let mut mesh = egui::Mesh::default();
        // Ear clipping preserves the concave grip outline; a center fan crosses the cutouts.
        points.dedup_by(|a, b| a.distance(*b) < 0.01);
        if points
            .first()
            .zip(points.last())
            .is_some_and(|(a, b)| a.distance(*b) < 0.01)
        {
            points.pop();
        }
        for point in &points {
            mesh.colored_vertex(*point, fill);
        }
        for [a, b, c] in triangulate(&points) {
            mesh.add_triangle(a, b, c);
        }
        self.painter.add(egui::Shape::mesh(mesh));
        self.painter.add(egui::Shape::closed_line(
            points,
            Stroke::new(self.scale * 1.5, edge),
        ));
    }
}

fn triangulate(points: &[Pos2]) -> Vec<[u32; 3]> {
    let cross = |a: Pos2, b: Pos2, c: Pos2| (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
    let area: f32 = points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .map(|(a, b)| a.x * b.y - b.x * a.y)
        .sum();
    let winding = area.signum();
    let mut remaining: Vec<usize> = (0..points.len()).collect();
    let mut triangles = Vec::new();
    while remaining.len() > 2 {
        let ear = (0..remaining.len()).find(|&i| {
            let a = remaining[(i + remaining.len() - 1) % remaining.len()];
            let b = remaining[i];
            let c = remaining[(i + 1) % remaining.len()];
            if cross(points[a], points[b], points[c]) * winding <= 0.0001 {
                return false;
            }
            !remaining.iter().any(|&p| {
                p != a
                    && p != b
                    && p != c
                    && cross(points[a], points[b], points[p]) * winding >= -0.0001
                    && cross(points[b], points[c], points[p]) * winding >= -0.0001
                    && cross(points[c], points[a], points[p]) * winding >= -0.0001
            })
        });
        let Some(i) = ear else {
            break;
        };
        triangles.push([
            remaining[(i + remaining.len() - 1) % remaining.len()] as u32,
            remaining[i] as u32,
            remaining[(i + 1) % remaining.len()] as u32,
        ]);
        remaining.remove(i);
    }
    triangles
}

pub fn show(
    ui: &mut Ui,
    kind: GamepadKind,
    state: &GamepadVisualState,
    deadzone: f32,
) -> Vec<String> {
    let colors = theme::palette(ui.ctx());
    let width = ui.available_width().min(560.0).max(1.0);
    let (area, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), width * 360.0 / 560.0),
        Sense::hover(),
    );
    let origin = Pos2::new(area.center().x - width / 2.0, area.top());
    let mut pad = Pad {
        painter: ui.painter_at(area),
        ui,
        origin,
        scale: width / 560.0,
        state,
        actions: Vec::new(),
    };
    let shell = Color32::from_rgb(187, 201, 197);
    let edge = Color32::from_rgb(117, 137, 135);
    let dark = Color32::from_rgb(53, 68, 70);
    let switch = kind == GamepadKind::Nintendo;
    let ps = kind == GamepadKind::PlayStation;

    for x in (20..550).step_by(24) {
        for y in (12..330).step_by(24) {
            pad.circle(x as f32, y as f32, 0.65, theme::alpha(colors.muted, 0.13));
        }
    }

    if switch {
        pad.rounded(
            pad.rect(96.0, 63.0, 368.0, 236.0),
            14.0,
            Color32::from_rgb(30, 41, 45),
        );
        pad.rounded(
            pad.rect(98.0, 63.0, 114.0, 236.0),
            32.0,
            Color32::from_rgb(49, 144, 151),
        );
        pad.rounded(
            pad.rect(348.0, 63.0, 114.0, 236.0),
            32.0,
            Color32::from_rgb(199, 101, 96),
        );
        pad.text(280.0, 150.0, "NINTENDO", 14.0, MUTED);
        pad.text(280.0, 172.0, "SWITCH", 20.0, TEXT);
    } else {
        pad.shell(ps, shell, edge);
        if ps {
            pad.rounded(pad.rect(212.0, 104.0, 136.0, 62.0), 10.0, dark);
            pad.text(280.0, 135.0, "TOUCHPAD", 11.0, MUTED);
        } else {
            pad.circle(280.0, 128.0, 21.0, dark);
        }
    }

    let (left_stick, right_stick, dpad, face) = if ps {
        (
            (218.0, 221.0),
            (342.0, 221.0),
            (145.0, 159.0),
            (415.0, 159.0),
        )
    } else if switch {
        (
            (155.0, 128.0),
            (405.0, 229.0),
            (155.0, 229.0),
            (405.0, 128.0),
        )
    } else {
        (
            (145.0, 158.0),
            (342.0, 221.0),
            (218.0, 221.0),
            (415.0, 158.0),
        )
    };
    pad.stick(0, left_stick, deadzone);
    pad.stick(1, right_stick, deadzone);
    pad.dpad(dpad, switch || ps);
    pad.face(face, kind);

    let (lt, rt, lb, rb) = if ps {
        ("L2", "R2", "L1", "R1")
    } else if switch {
        ("ZL", "ZR", "L", "R")
    } else {
        ("LT", "RT", "LB", "RB")
    };
    for (key, label, x, y, w, h) in [
        ("left_trigger", lt, 151.0, 36.0, 72.0, 25.0),
        ("right_trigger", rt, 409.0, 36.0, 72.0, 25.0),
        ("left_bumper", lb, 151.0, 68.0, 82.0, 23.0),
        ("right_bumper", rb, 409.0, 68.0, 82.0, 23.0),
    ] {
        pad.button(key, label, (x, y), egui::vec2(w, h), dark, false);
    }
    let (select, start, mode) = if switch {
        ((195.0, 91.0), (365.0, 91.0), (383.0, 278.0))
    } else if ps {
        ((192.0, 125.0), (368.0, 125.0), (280.0, 190.0))
    } else {
        ((229.0, 163.0), (331.0, 163.0), (280.0, 128.0))
    };
    for (key, label, center) in [
        ("select", "−", select),
        ("start", "+", start),
        (
            "mode",
            if ps {
                "PS"
            } else if switch {
                "H"
            } else {
                "X"
            },
            mode,
        ),
    ] {
        pad.button(key, label, center, Vec2::splat(22.0), dark, true);
    }
    for (key, label, x) in [("left_thumb", "L3", 248.0), ("right_thumb", "R3", 312.0)] {
        pad.button(key, label, (x, 285.0), egui::vec2(34.0, 22.0), dark, false);
    }
    pad.text(280.0, 340.0, "点击按键 / 拖动摇杆", 11.0, colors.muted);
    pad.actions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(
        ctx: &egui::Context,
        kind: GamepadKind,
        width: f32,
        events: Vec<egui::Event>,
    ) -> (Vec<String>, egui::FullOutput) {
        let mut actions = Vec::new();
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(width, 440.0))),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    actions = show(ui, kind, &GamepadVisualState::default(), 0.16);
                });
            },
        );
        (actions, output)
    }

    fn pointer(pos: Pos2, pressed: bool) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    #[test]
    fn triangulation_preserves_concave_cutouts() {
        let points: Vec<_> = [(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (2.0, 2.0), (0.0, 4.0)]
            .into_iter()
            .map(|(x, y)| Pos2::new(x, y))
            .collect();
        let triangles = triangulate(&points);
        assert_eq!(triangles.len(), points.len() - 2);
        let area: f32 = triangles
            .into_iter()
            .map(|[a, b, c]| {
                let a = points[a as usize];
                let b = points[b as usize];
                let c = points[c as usize];
                ((b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)).abs() / 2.0
            })
            .sum();
        assert!((area - 12.0).abs() < 0.01);
    }

    #[test]
    fn controller_layouts_fit_wide_and_narrow_panels() {
        for kind in [
            GamepadKind::Xbox,
            GamepadKind::PlayStation,
            GamepadKind::Nintendo,
        ] {
            for width in [300.0, 576.0, 900.0] {
                let ctx = egui::Context::default();
                let (_, output) = frame(&ctx, kind, width, Vec::new());
                for shape in &output.shapes {
                    let bounds = shape.shape.visual_bounding_rect();
                    assert!(bounds.is_finite());
                    assert!(
                        bounds.left() >= -1.0 && bounds.right() <= width + 1.0,
                        "{kind:?}: {bounds:?}"
                    );
                }
                let primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
                assert!(!primitives.is_empty());
            }
        }
    }

    #[test]
    fn face_triggers_and_home_send_held_state_until_release() {
        for (kind, positions) in [
            (
                GamepadKind::Xbox,
                [(415.0, 184.0), (151.0, 36.0), (409.0, 36.0), (280.0, 128.0)],
            ),
            (
                GamepadKind::PlayStation,
                [(415.0, 185.0), (151.0, 36.0), (409.0, 36.0), (280.0, 190.0)],
            ),
            (
                GamepadKind::Nintendo,
                [(405.0, 154.0), (151.0, 36.0), (409.0, 36.0), (383.0, 278.0)],
            ),
        ] {
            for ((x, y), key) in
                positions
                    .into_iter()
                    .zip(["south", "left_trigger", "right_trigger", "mode"])
            {
                let ctx = egui::Context::default();
                frame(&ctx, kind, 576.0, Vec::new());
                let pos = Pos2::new(x + 8.0, y + 8.0);
                let (actions, _) = frame(&ctx, kind, 576.0, pointer(pos, true));
                assert!(
                    actions.iter().any(|action| action == key),
                    "{kind:?}: {actions:?}"
                );
                let (held, _) = frame(&ctx, kind, 576.0, Vec::new());
                assert!(held.iter().any(|action| action == key));
                let (released, _) = frame(&ctx, kind, 576.0, pointer(pos, false));
                assert!(!released.iter().any(|action| action == key));
            }
        }
    }

    #[test]
    fn dragging_up_sends_positive_y_and_release_clears_stick() {
        let ctx = egui::Context::default();
        frame(&ctx, GamepadKind::Xbox, 576.0, Vec::new());
        frame(
            &ctx,
            GamepadKind::Xbox,
            576.0,
            pointer(Pos2::new(153.0, 166.0), true),
        );
        let (actions, _) = frame(
            &ctx,
            GamepadKind::Xbox,
            576.0,
            vec![egui::Event::PointerMoved(Pos2::new(153.0, 134.0))],
        );
        assert!(
            actions.contains(&"__stick_left:0.000:1.000".to_owned()),
            "{actions:?}"
        );
        let (actions, _) = frame(
            &ctx,
            GamepadKind::Xbox,
            576.0,
            pointer(Pos2::new(153.0, 134.0), false),
        );
        assert!(actions.contains(&"__stick_left:0:0".to_owned()));
    }
}
