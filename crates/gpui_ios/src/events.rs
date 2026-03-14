use gpui::{
    Capslock, KeyDownEvent, KeyUpEvent, Keystroke, ModifierKey, Modifiers, ModifiersChangedEvent,
    MouseButton, MouseDownEvent, MouseExitedEvent, MouseMoveEvent, MouseUpEvent, NavigationDirection,
    PlatformInput, ScrollDelta, ScrollWheelEvent, point, px,
};

/// Modifier flags as returned by UIKey.modifierFlags on iPadOS.
///
/// These match the UIKeyModifierFlags values from UIKit.
#[allow(dead_code)]
pub(crate) mod ui_key_modifier_flags {
    pub const ALPHA_SHIFT: usize = 1 << 16; // Caps Lock
    pub const SHIFT: usize = 1 << 17;
    pub const CONTROL: usize = 1 << 18;
    pub const ALTERNATE: usize = 1 << 19; // Option
    pub const COMMAND: usize = 1 << 20;
    pub const NUMERIC_PAD: usize = 1 << 21;
}

/// Converts raw UIKey modifier flags into a GPUI `Modifiers` value.
pub(crate) fn modifiers_from_flags(flags: usize) -> Modifiers {
    Modifiers {
        shift: flags & ui_key_modifier_flags::SHIFT != 0,
        control: flags & ui_key_modifier_flags::CONTROL != 0,
        alt: flags & ui_key_modifier_flags::ALTERNATE != 0,
        command: flags & ui_key_modifier_flags::COMMAND != 0,
        function: false,
    }
}

/// Converts a UIKey press into a GPUI `Keystroke`.
///
/// `characters` is the translated string from `UIKey.characters`.
/// `characters_ignoring_modifiers` is from `UIKey.charactersIgnoringModifiers`.
/// `key_code` is the `UIKeyboardHIDUsage` raw value.
/// `modifier_flags` is the raw `UIKeyModifierFlags` bitmask.
pub(crate) fn keystroke_from_ui_key(
    characters: &str,
    characters_ignoring_modifiers: &str,
    key_code: u32,
    modifier_flags: usize,
) -> Keystroke {
    let modifiers = modifiers_from_flags(modifier_flags);

    // Prefer the key name derived from the HID usage code for well-known keys,
    // falling back to the translated character.
    let key = key_name_for_hid(key_code)
        .map(String::from)
        .unwrap_or_else(|| {
            if characters_ignoring_modifiers.is_empty() {
                characters.to_lowercase()
            } else {
                characters_ignoring_modifiers.to_lowercase()
            }
        });

    Keystroke {
        key,
        modifiers,
        ime_key: None,
    }
}

/// Maps UIKeyboardHIDUsage values to GPUI key name strings.
///
/// Only the keys that need explicit mapping are listed here. Printable characters
/// are handled by the `characters_ignoring_modifiers` path above.
fn key_name_for_hid(usage: u32) -> Option<&'static str> {
    // UIKeyboardHIDUsage values (subset)
    match usage {
        0x0028 => Some("enter"),
        0x0029 => Some("escape"),
        0x002A => Some("backspace"),
        0x002B => Some("tab"),
        0x002C => Some("space"),
        0x004F => Some("right"),
        0x0050 => Some("left"),
        0x0051 => Some("down"),
        0x0052 => Some("up"),
        0x004B => Some("pageup"),
        0x004E => Some("pagedown"),
        0x004A => Some("home"),
        0x004D => Some("end"),
        0x0049 => Some("insert"),
        0x004C => Some("delete"),
        0x003A => Some("f1"),
        0x003B => Some("f2"),
        0x003C => Some("f3"),
        0x003D => Some("f4"),
        0x003E => Some("f5"),
        0x003F => Some("f6"),
        0x0040 => Some("f7"),
        0x0041 => Some("f8"),
        0x0042 => Some("f9"),
        0x0043 => Some("f10"),
        0x0044 => Some("f11"),
        0x0045 => Some("f12"),
        _ => None,
    }
}

/// Synthesises a GPUI `MouseDownEvent` from a UITouch began event.
///
/// `position` is in points in GPUI coordinate space (top-left origin,
/// already converted from UIKit's top-left origin — no flip needed).
pub(crate) fn mouse_down_from_touch(
    position: gpui::Point<gpui::Pixels>,
    click_count: usize,
    modifiers: Modifiers,
) -> PlatformInput {
    PlatformInput::MouseDown(MouseDownEvent {
        button: MouseButton::Left,
        position,
        modifiers,
        click_count,
        first_mouse: false,
    })
}

pub(crate) fn mouse_up_from_touch(
    position: gpui::Point<gpui::Pixels>,
    click_count: usize,
    modifiers: Modifiers,
) -> PlatformInput {
    PlatformInput::MouseUp(MouseUpEvent {
        button: MouseButton::Left,
        position,
        modifiers,
        click_count,
    })
}

pub(crate) fn mouse_move_from_pointer(
    position: gpui::Point<gpui::Pixels>,
    pressed_button: Option<MouseButton>,
    modifiers: Modifiers,
) -> PlatformInput {
    PlatformInput::MouseMove(MouseMoveEvent {
        position,
        pressed_button,
        modifiers,
    })
}

pub(crate) fn scroll_wheel_from_pan(
    position: gpui::Point<gpui::Pixels>,
    delta_x: f32,
    delta_y: f32,
    modifiers: Modifiers,
) -> PlatformInput {
    PlatformInput::ScrollWheel(ScrollWheelEvent {
        position,
        delta: ScrollDelta::Pixels(point(px(delta_x), px(delta_y))),
        modifiers,
        touch_phase: gpui::TouchPhase::Moved,
    })
}

pub(crate) fn key_down_event(keystroke: Keystroke, is_repeat: bool) -> PlatformInput {
    let ime_key = if !keystroke.modifiers.command
        && !keystroke.modifiers.control
        && !keystroke.modifiers.alt
    {
        Some(keystroke.key.clone())
    } else {
        None
    };
    PlatformInput::KeyDown(KeyDownEvent {
        keystroke: Keystroke {
            ime_key: ime_key.clone(),
            ..keystroke
        },
        is_repeat,
    })
}

pub(crate) fn key_up_event(keystroke: Keystroke) -> PlatformInput {
    PlatformInput::KeyUp(KeyUpEvent { keystroke })
}

pub(crate) fn modifiers_changed_event(modifiers: Modifiers) -> PlatformInput {
    PlatformInput::ModifiersChanged(ModifiersChangedEvent { modifiers })
}
