use crate::{
    display::IosDisplay, keyboard::IosKeyboardLayout, renderer, text_system::IosTextSystem,
    window::IosWindow,
};
use anyhow::Result;
use futures::channel::oneshot;
use gpui::{
    Action, AnyWindowHandle, BackgroundExecutor, ClipboardItem, CursorStyle, DummyKeyboardMapper,
    ForegroundExecutor, Keymap, Menu, MenuItem, PathPromptOptions, Platform, PlatformDisplay,
    PlatformKeyboardLayout, PlatformKeyboardMapper, PlatformTextSystem, PlatformWindow, Task,
    ThermalState, WindowAppearance, WindowParams,
};
use objc::{class, msg_send, sel, sel_impl};
use parking_lot::Mutex;
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

#[derive(Default)]
struct IosPlatformCallbacks {
    open_urls: Option<Box<dyn FnMut(Vec<String>)>>,
    quit: Option<Box<dyn FnMut()>>,
    reopen: Option<Box<dyn FnMut()>>,
    app_menu_action: Option<Box<dyn FnMut(&dyn Action)>>,
    will_open_app_menu: Option<Box<dyn FnMut()>>,
    validate_app_menu_command: Option<Box<dyn FnMut(&dyn Action) -> bool>>,
    keyboard_layout_change: Option<Box<dyn FnMut()>>,
    thermal_state_change: Option<Box<dyn FnMut()>>,
}

/// The iOS GPUI platform implementation.
///
/// On iOS, UIKit owns the run loop. `IosPlatform::run` calls the
/// `on_finish_launching` callback and returns immediately — the caller
/// (Swift `AppDelegate`) is responsible for keeping the process alive.
pub struct IosPlatform {
    background_executor: BackgroundExecutor,
    foreground_executor: ForegroundExecutor,
    text_system: Arc<dyn PlatformTextSystem>,
    active_display: Rc<dyn PlatformDisplay>,
    active_window: RefCell<Option<AnyWindowHandle>>,
    renderer_context: renderer::Context,
    callbacks: RefCell<IosPlatformCallbacks>,
}

// Safety: IosPlatform is only accessed from the main thread (GCD main queue).
unsafe impl Send for IosPlatform {}
unsafe impl Sync for IosPlatform {}

impl IosPlatform {
    pub fn new(_headless: bool) -> Self {
        use crate::dispatcher::IosDispatcher;

        let dispatcher = Arc::new(IosDispatcher::new());
        let background_executor = BackgroundExecutor::new(dispatcher.clone());
        let foreground_executor = ForegroundExecutor::new(dispatcher);

        let text_system: Arc<dyn PlatformTextSystem> = Arc::new(IosTextSystem::new());
        let active_display: Rc<dyn PlatformDisplay> = Rc::new(IosDisplay::primary());

        let renderer_context = renderer::Context::default();

        Self {
            background_executor,
            foreground_executor,
            text_system,
            active_display,
            active_window: RefCell::new(None),
            renderer_context,
            callbacks: RefCell::new(IosPlatformCallbacks::default()),
        }
    }
}

impl Platform for IosPlatform {
    fn background_executor(&self) -> BackgroundExecutor {
        self.background_executor.clone()
    }

    fn foreground_executor(&self) -> ForegroundExecutor {
        self.foreground_executor.clone()
    }

    fn text_system(&self) -> Arc<dyn PlatformTextSystem> {
        self.text_system.clone()
    }

    fn run(&self, on_finish_launching: Box<dyn 'static + FnOnce()>) {
        // UIKit owns the run loop on iOS. We just call the launch callback
        // and return. The application lifecycle is managed by the Swift host.
        on_finish_launching();
    }

    fn quit(&self) {
        // iOS apps cannot programmatically quit. No-op.
    }

    fn restart(&self, _binary_path: Option<PathBuf>) {
        // No subprocess spawning on iOS. No-op.
    }

    fn activate(&self, _ignoring_other_apps: bool) {
        unsafe {
            let app: *mut objc::runtime::Object = msg_send![class!(UIApplication), sharedApplication];
            let _: () = msg_send![app, becomeFirstResponder];
        }
    }

    fn hide(&self) {
        // iOS apps cannot hide themselves; no-op.
    }

    fn hide_other_apps(&self) {}

    fn unhide_other_apps(&self) {}

    fn displays(&self) -> Vec<Rc<dyn PlatformDisplay>> {
        vec![self.active_display.clone()]
    }

    fn primary_display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        Some(self.active_display.clone())
    }

    fn active_window(&self) -> Option<AnyWindowHandle> {
        *self.active_window.borrow()
    }

    fn open_window(
        &self,
        handle: AnyWindowHandle,
        params: WindowParams,
    ) -> Result<Box<dyn PlatformWindow>> {
        let window = IosWindow::new(handle, params, self.renderer_context.clone())?;
        *self.active_window.borrow_mut() = Some(handle);
        Ok(Box::new(window))
    }

    fn window_appearance(&self) -> WindowAppearance {
        // Check UIScreen's traitCollection for dark mode.
        unsafe {
            let screen: *mut objc::runtime::Object =
                msg_send![class!(UIScreen), mainScreen];
            let trait_collection: *mut objc::runtime::Object =
                msg_send![screen, traitCollection];
            let style: usize = msg_send![trait_collection, userInterfaceStyle];
            if style == 2 {
                WindowAppearance::Dark
            } else {
                WindowAppearance::Light
            }
        }
    }

    fn open_url(&self, url: &str) {
        unsafe {
            let app: *mut objc::runtime::Object =
                msg_send![class!(UIApplication), sharedApplication];
            let cf_url = crate::ns_string(url);
            let url_obj: *mut objc::runtime::Object = msg_send![class!(NSURL), URLWithString: cf_url];
            if !url_obj.is_null() {
                let options: *mut objc::runtime::Object =
                    msg_send![class!(NSDictionary), dictionary];
                let _: () = msg_send![app, openURL: url_obj options: options completionHandler: std::ptr::null::<std::ffi::c_void>()];
            }
        }
    }

    fn on_open_urls(&self, callback: Box<dyn FnMut(Vec<String>)>) {
        self.callbacks.borrow_mut().open_urls = Some(callback);
    }

    fn register_url_scheme(&self, _url: &str) -> Task<Result<()>> {
        Task::ready(Ok(()))
    }

    fn prompt_for_paths(
        &self,
        _options: PathPromptOptions,
    ) -> oneshot::Receiver<Result<Option<Vec<PathBuf>>>> {
        // TODO: implement UIDocumentPickerViewController for SSH key import
        let (sender, receiver) = oneshot::channel();
        sender
            .send(Err(anyhow::anyhow!(
                "prompt_for_paths: UIDocumentPicker not yet implemented"
            )))
            .ok();
        receiver
    }

    fn prompt_for_new_path(
        &self,
        _directory: &Path,
        _suggested_name: Option<&str>,
    ) -> oneshot::Receiver<Result<Option<PathBuf>>> {
        let (sender, receiver) = oneshot::channel();
        sender
            .send(Err(anyhow::anyhow!(
                "prompt_for_new_path not supported on iOS"
            )))
            .ok();
        receiver
    }

    fn can_select_mixed_files_and_dirs(&self) -> bool {
        false
    }

    fn reveal_path(&self, _path: &Path) {
        // Remote file — no local Files.app reveal. No-op.
    }

    fn open_with_system(&self, _path: &Path) {}

    fn on_quit(&self, callback: Box<dyn FnMut()>) {
        self.callbacks.borrow_mut().quit = Some(callback);
    }

    fn on_reopen(&self, callback: Box<dyn FnMut()>) {
        self.callbacks.borrow_mut().reopen = Some(callback);
    }

    fn set_menus(&self, _menus: Vec<Menu>, _keymap: &Keymap) {
        // No menu bar on iOS (UIMenuSystem is a future enhancement).
    }

    fn set_dock_menu(&self, _menu: Vec<MenuItem>, _keymap: &Keymap) {}

    fn on_app_menu_action(&self, callback: Box<dyn FnMut(&dyn Action)>) {
        self.callbacks.borrow_mut().app_menu_action = Some(callback);
    }

    fn on_will_open_app_menu(&self, callback: Box<dyn FnMut()>) {
        self.callbacks.borrow_mut().will_open_app_menu = Some(callback);
    }

    fn on_validate_app_menu_command(&self, callback: Box<dyn FnMut(&dyn Action) -> bool>) {
        self.callbacks.borrow_mut().validate_app_menu_command = Some(callback);
    }

    fn thermal_state(&self) -> ThermalState {
        // NSProcessInfo.thermalState is available on iOS 11+.
        unsafe {
            let process_info: *mut objc::runtime::Object =
                msg_send![class!(NSProcessInfo), processInfo];
            let state: usize = msg_send![process_info, thermalState];
            match state {
                0 => ThermalState::Nominal,
                1 => ThermalState::Fair,
                2 => ThermalState::Serious,
                _ => ThermalState::Critical,
            }
        }
    }

    fn on_thermal_state_change(&self, callback: Box<dyn FnMut()>) {
        self.callbacks.borrow_mut().thermal_state_change = Some(callback);
    }

    fn compositor_name(&self) -> &'static str {
        "iOS Metal"
    }

    fn app_path(&self) -> Result<PathBuf> {
        unsafe {
            let bundle: *mut objc::runtime::Object =
                msg_send![class!(NSBundle), mainBundle];
            let path: *mut objc::runtime::Object = msg_send![bundle, bundlePath];
            let cstr: *const std::os::raw::c_char = msg_send![path, UTF8String];
            if cstr.is_null() {
                return Err(anyhow::anyhow!("failed to get bundle path"));
            }
            let s = std::ffi::CStr::from_ptr(cstr).to_str()?;
            Ok(PathBuf::from(s))
        }
    }

    fn path_for_auxiliary_executable(&self, _name: &str) -> Result<PathBuf> {
        Err(anyhow::anyhow!(
            "path_for_auxiliary_executable: no subprocess spawning on iOS"
        ))
    }

    fn set_cursor_style(&self, _style: CursorStyle) {
        // UIPointerStyle for trackpad cursor is set per-view via UIPointerInteraction.
        // Global cursor style changes are not supported on iOS.
    }

    fn should_auto_hide_scrollbars(&self) -> bool {
        true
    }

    fn read_from_clipboard(&self) -> Option<ClipboardItem> {
        unsafe {
            let pasteboard: *mut objc::runtime::Object =
                msg_send![class!(UIPasteboard), generalPasteboard];
            let string: *mut objc::runtime::Object = msg_send![pasteboard, string];
            if string.is_null() {
                return None;
            }
            let cstr: *const std::os::raw::c_char = msg_send![string, UTF8String];
            if cstr.is_null() {
                return None;
            }
            let s = std::ffi::CStr::from_ptr(cstr)
                .to_str()
                .ok()?
                .to_owned();
            Some(ClipboardItem::new_string(s))
        }
    }

    fn write_to_clipboard(&self, item: ClipboardItem) {
        let text = item.text().unwrap_or("");
        unsafe {
            let pasteboard: *mut objc::runtime::Object =
                msg_send![class!(UIPasteboard), generalPasteboard];
            let ns_str = crate::ns_string(text);
            let _: () = msg_send![pasteboard, setString: ns_str];
        }
    }

    fn write_credentials(&self, _url: &str, _username: &str, _password: &[u8]) -> Task<Result<()>> {
        // TODO Phase 2.1: implement via iOS Keychain (Security.framework)
        Task::ready(Err(anyhow::anyhow!(
            "write_credentials: Keychain not yet implemented; coming in Phase 2.1"
        )))
    }

    fn read_credentials(&self, _url: &str) -> Task<Result<Option<(String, Vec<u8>)>>> {
        Task::ready(Ok(None))
    }

    fn delete_credentials(&self, _url: &str) -> Task<Result<()>> {
        Task::ready(Ok(()))
    }

    fn keyboard_layout(&self) -> Box<dyn PlatformKeyboardLayout> {
        Box::new(IosKeyboardLayout)
    }

    fn keyboard_mapper(&self) -> Rc<dyn PlatformKeyboardMapper> {
        crate::keyboard::ios_keyboard_mapper()
    }

    fn on_keyboard_layout_change(&self, callback: Box<dyn FnMut()>) {
        self.callbacks.borrow_mut().keyboard_layout_change = Some(callback);
    }
}
