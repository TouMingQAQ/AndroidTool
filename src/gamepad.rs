use std::collections::BTreeSet;

use gilrs::{Axis, Button, EventType, Gilrs};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GamepadKind {
    Xbox,
    PlayStation,
    Nintendo,
    Generic,
}

impl GamepadKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Xbox => "Xbox",
            Self::PlayStation => "PlayStation",
            Self::Nintendo => "Nintendo",
            Self::Generic => "通用手柄",
        }
    }

    pub fn face_labels(self) -> [&'static str; 4] {
        match self {
            Self::Xbox => ["A", "B", "Y", "X"],
            Self::PlayStation => ["×", "○", "△", "□"],
            Self::Nintendo => ["B", "A", "X", "Y"],
            Self::Generic => ["S", "E", "N", "W"],
        }
    }
}

#[derive(Debug, Clone)]
pub struct GamepadInfo {
    pub id: gilrs::GamepadId,
    pub name: String,
    pub kind: GamepadKind,
    pub connected: bool,
}

pub struct GamepadHub {
    gilrs: Option<Gilrs>,
    pub gamepads: Vec<GamepadInfo>,
    pub active: Option<gilrs::GamepadId>,
    pub error: Option<String>,
}

#[derive(Default, Clone)]
pub struct GamepadVisualState {
    pub buttons: BTreeSet<&'static str>,
    pub sticks: [[f32; 2]; 2],
    pub triggers: [f32; 2],
}

impl GamepadHub {
    pub fn new() -> Self {
        match Gilrs::new() {
            Ok(gilrs) => {
                let mut hub = Self {
                    gilrs: Some(gilrs),
                    gamepads: Vec::new(),
                    active: None,
                    error: None,
                };
                hub.refresh();
                hub
            }
            Err(error) => Self {
                gilrs: None,
                gamepads: Vec::new(),
                active: None,
                error: Some(format!("手柄系统初始化失败: {error}")),
            },
        }
    }

    pub fn refresh(&mut self) {
        let Some(gilrs) = self.gilrs.as_ref() else {
            return;
        };
        let mut gamepads = Vec::new();
        for (id, gamepad) in gilrs.gamepads() {
            gamepads.push(GamepadInfo {
                id,
                name: gamepad.name().to_owned(),
                kind: detect_kind(gamepad.name(), gamepad.vendor_id()),
                connected: gamepad.is_connected(),
            });
        }
        self.gamepads = gamepads;
        if let Some(active) = self.active {
            if !self.gamepads.iter().any(|gamepad| gamepad.id == active) {
                self.active = None;
            }
        }
        if self.active.is_none() {
            self.active = self.gamepads.first().map(|gamepad| gamepad.id);
        }
    }

    pub fn active_kind(&self) -> GamepadKind {
        self.active
            .and_then(|active| {
                self.gamepads
                    .iter()
                    .find(|gamepad| gamepad.id == active)
                    .map(|gamepad| gamepad.kind)
            })
            .unwrap_or(GamepadKind::Generic)
    }

    pub fn visual_state(&self) -> GamepadVisualState {
        let mut state = GamepadVisualState::default();
        let Some(gamepad) = self
            .active
            .and_then(|id| self.gilrs.as_ref()?.connected_gamepad(id))
        else {
            return state;
        };
        for button in [
            Button::South,
            Button::East,
            Button::North,
            Button::West,
            Button::LeftTrigger,
            Button::RightTrigger,
            Button::DPadUp,
            Button::DPadDown,
            Button::DPadLeft,
            Button::DPadRight,
            Button::Select,
            Button::Start,
            Button::Mode,
            Button::LeftThumb,
            Button::RightThumb,
        ] {
            if gamepad.is_pressed(button) {
                if let Some(key) = button_key(button) {
                    state.buttons.insert(key);
                }
            }
        }
        state.triggers = [
            trigger_value(&gamepad, Button::LeftTrigger2, Axis::LeftZ),
            trigger_value(&gamepad, Button::RightTrigger2, Axis::RightZ),
        ];
        state.sticks = [
            [
                axis_value(&gamepad, Axis::LeftStickX),
                axis_value(&gamepad, Axis::LeftStickY),
            ],
            [
                axis_value(&gamepad, Axis::RightStickX),
                axis_value(&gamepad, Axis::RightStickY),
            ],
        ];
        state
    }

    pub fn poll(&mut self) {
        let mut changed = false;
        if let Some(gilrs) = self.gilrs.as_mut() {
            while let Some(event) = gilrs.next_event() {
                changed |= matches!(event.event, EventType::Connected | EventType::Disconnected);
            }
        }
        if changed {
            self.refresh();
        }
    }
}

fn trigger_value(gamepad: &gilrs::Gamepad<'_>, button: Button, axis: Axis) -> f32 {
    // SDL mappings expose triggers as analog buttons; unmapped drivers may use Z axes.
    normalize_trigger(
        gamepad.button_data(button).map(|data| data.value()),
        gamepad.axis_data(axis).map(|data| data.value()),
        gamepad.is_pressed(button),
    )
}

fn normalize_trigger(button: Option<f32>, axis: Option<f32>, pressed: bool) -> f32 {
    let value = button
        .filter(|value| value.is_finite())
        .or_else(|| {
            axis.filter(|value| value.is_finite())
                .map(|value| (value + 1.0) * 0.5)
        })
        .unwrap_or(if pressed { 1.0 } else { 0.0 });
    value.clamp(0.0, 1.0)
}

fn axis_value(gamepad: &gilrs::Gamepad<'_>, axis: Axis) -> f32 {
    gamepad
        .axis_data(axis)
        .map(|data| data.value())
        .unwrap_or(0.0)
}

pub fn detect_kind(name: &str, vendor: Option<u16>) -> GamepadKind {
    let name = name.to_ascii_lowercase();
    if vendor == Some(0x045e)
        || name.contains("xbox")
        || name.contains("microsoft")
        || name.contains("xinput")
    {
        GamepadKind::Xbox
    } else if vendor == Some(0x054c)
        || name.contains("dualsense")
        || name.contains("dualshock")
        || name.contains("playstation")
        || name.contains("sony")
    {
        GamepadKind::PlayStation
    } else if vendor == Some(0x057e)
        || name.contains("nintendo")
        || name.contains("joy-con")
        || name.contains("switch pro")
        || name.contains("pro controller")
    {
        GamepadKind::Nintendo
    } else {
        GamepadKind::Generic
    }
}

pub fn button_key(button: Button) -> Option<&'static str> {
    match button {
        Button::South => Some("south"),
        Button::East => Some("east"),
        Button::North => Some("north"),
        Button::West => Some("west"),
        Button::LeftTrigger => Some("left_bumper"),
        Button::RightTrigger => Some("right_bumper"),
        Button::LeftTrigger2 => Some("left_trigger"),
        Button::RightTrigger2 => Some("right_trigger"),
        Button::DPadUp => Some("dpad_up"),
        Button::DPadDown => Some("dpad_down"),
        Button::DPadLeft => Some("dpad_left"),
        Button::DPadRight => Some("dpad_right"),
        Button::Select => Some("select"),
        Button::Start => Some("start"),
        Button::Mode => Some("mode"),
        Button::LeftThumb => Some("left_thumb"),
        Button::RightThumb => Some("right_thumb"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triggers_support_analog_buttons_signed_axes_and_digital_fallback() {
        for (button, axis, pressed, expected) in [
            (Some(0.0), Some(1.0), false, 0.0),
            (Some(0.35), None, true, 0.35),
            (Some(1.0), None, true, 1.0),
            (None, Some(-1.0), false, 0.0),
            (None, Some(0.0), false, 0.5),
            (None, Some(1.0), true, 1.0),
            (None, None, true, 1.0),
            (None, None, false, 0.0),
            (Some(f32::NAN), Some(-1.0), false, 0.0),
        ] {
            assert_eq!(normalize_trigger(button, axis, pressed), expected);
        }
    }

    #[test]
    fn detects_controller_families() {
        assert_eq!(
            detect_kind("Xbox Wireless Controller", None),
            GamepadKind::Xbox
        );
        assert_eq!(
            detect_kind("DualSense Wireless Controller", Some(0x054c)),
            GamepadKind::PlayStation
        );
        assert_eq!(
            detect_kind("Nintendo Switch Pro Controller", Some(0x057e)),
            GamepadKind::Nintendo
        );
        assert_eq!(detect_kind("Generic USB Pad", None), GamepadKind::Generic);
    }
}
