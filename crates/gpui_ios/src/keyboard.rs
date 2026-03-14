use gpui::{
    DummyKeyboardMapper, Keystroke, PlatformKeyboardLayout, PlatformKeyboardMapper,
    SharedString,
};

/// Minimal keyboard layout for iOS.
///
/// On iOS, the system handles most keyboard layout concerns. Hardware keyboard
/// events arrive via `pressesBegan:withEvent:` with pre-translated characters.
/// We don't need full layout mapping the way macOS does.
pub(crate) struct IosKeyboardLayout;

impl PlatformKeyboardLayout for IosKeyboardLayout {
    fn current_layout(&self) -> SharedString {
        SharedString::from_static("com.apple.keylayout.US")
    }

    fn keystroke_for_key_equivalent(&self, _equiv: &str) -> Option<Keystroke> {
        None
    }
}

/// Returns the keyboard mapper for iOS.
///
/// We reuse the dummy mapper from GPUI since iOS provides translated characters
/// in UIKey events — there's no need for a full layout mapping table.
pub(crate) fn ios_keyboard_mapper() -> std::rc::Rc<dyn PlatformKeyboardMapper> {
    std::rc::Rc::new(DummyKeyboardMapper)
}
