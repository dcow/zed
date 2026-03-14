use crate::{
    display::{CGPoint, CGRect, CGSize, IosDisplay},
    events::{
        key_down_event, key_up_event, modifiers_changed_event, modifiers_from_flags,
        mouse_down_from_touch, mouse_move_from_pointer, mouse_up_from_touch,
        scroll_wheel_from_pan,
    },
    metal_renderer::MetalRenderer,
    renderer::{self, Context as RendererContext},
};
use anyhow::Result;
use gpui::{
    AnyWindowHandle, Bounds, Capslock, DevicePixels, DispatchEventResult, GpuSpecs, Keystroke,
    Modifiers, MouseButton, PlatformAtlas, PlatformDisplay, PlatformInput, PlatformInputHandler,
    PlatformWindow, Pixels, Point, PromptButton, PromptLevel, RequestFrameOptions, ResizeEdge,
    Scene, Size, WindowAppearance, WindowBackgroundAppearance, WindowBounds, WindowControlArea,
    WindowControls, WindowDecorations, WindowParams, point, px, size,
};
use objc::{
    class, declare::ClassDecl, msg_send, runtime::{Class, Object, Sel, BOOL, NO, YES}, sel, sel_impl,
};
use parking_lot::Mutex;
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawDisplayHandle,
    RawWindowHandle, UiKitDisplayHandle, UiKitWindowHandle, WindowHandle,
};
use std::{
    cell::RefCell,
    ffi::c_void,
    ptr::{self, NonNull},
    rc::Rc,
    sync::Arc,
};

static mut ZED_METAL_VIEW_CLASS: *const Class = ptr::null();
static mut ZED_DISPLAY_LINK_HANDLER_CLASS: *const Class = ptr::null();

/// Register the Objective-C classes we need, once at startup.
///
/// # Safety
/// Must be called once before any window is created, on the main thread.
pub(crate) unsafe fn register_ios_classes() {
    unsafe {
        if !ZED_METAL_VIEW_CLASS.is_null() {
            return;
        }

        // ZedMetalView: a UIView subclass that returns CAMetalLayer as its layer class.
        ZED_METAL_VIEW_CLASS = {
            let mut decl = ClassDecl::new("ZedMetalView", class!(UIView))
                .expect("failed to declare ZedMetalView");

            // Override +layerClass so CAMetalLayer is the backing layer.
            extern "C" fn layer_class(_class: &Class, _sel: Sel) -> *const Class {
                class!(CAMetalLayer) as *const _
            }
            decl.add_class_method(
                sel!(layerClass),
                layer_class as extern "C" fn(&Class, Sel) -> *const Class,
            );

            // pressesBegan:withEvent: — hardware keyboard key down.
            extern "C" fn presses_began(
                this: &mut Object,
                _sel: Sel,
                presses: *mut Object,
                _event: *mut Object,
            ) {
                unsafe { handle_key_presses(this, presses, true); }
            }
            decl.add_method(
                sel!(pressesBegan:withEvent:),
                presses_began as extern "C" fn(&mut Object, Sel, *mut Object, *mut Object),
            );

            // pressesEnded:withEvent: — hardware keyboard key up.
            extern "C" fn presses_ended(
                this: &mut Object,
                _sel: Sel,
                presses: *mut Object,
                _event: *mut Object,
            ) {
                unsafe { handle_key_presses(this, presses, false); }
            }
            decl.add_method(
                sel!(pressesEnded:withEvent:),
                presses_ended as extern "C" fn(&mut Object, Sel, *mut Object, *mut Object),
            );

            // touchesBegan:withEvent:
            extern "C" fn touches_began(
                this: &mut Object,
                _sel: Sel,
                touches: *mut Object,
                _event: *mut Object,
            ) {
                unsafe { handle_touches(this, touches, TouchPhase::Began); }
            }
            decl.add_method(
                sel!(touchesBegan:withEvent:),
                touches_began as extern "C" fn(&mut Object, Sel, *mut Object, *mut Object),
            );

            // touchesMoved:withEvent:
            extern "C" fn touches_moved(
                this: &mut Object,
                _sel: Sel,
                touches: *mut Object,
                _event: *mut Object,
            ) {
                unsafe { handle_touches(this, touches, TouchPhase::Moved); }
            }
            decl.add_method(
                sel!(touchesMoved:withEvent:),
                touches_moved as extern "C" fn(&mut Object, Sel, *mut Object, *mut Object),
            );

            // touchesEnded:withEvent:
            extern "C" fn touches_ended(
                this: &mut Object,
                _sel: Sel,
                touches: *mut Object,
                _event: *mut Object,
            ) {
                unsafe { handle_touches(this, touches, TouchPhase::Ended); }
            }
            decl.add_method(
                sel!(touchesEnded:withEvent:),
                touches_ended as extern "C" fn(&mut Object, Sel, *mut Object, *mut Object),
            );

            decl.add_ivar::<*mut c_void>("windowStatePtr");
            decl.register() as *const _
        };

        // ZedDisplayLinkHandler: receives CADisplayLink callbacks.
        ZED_DISPLAY_LINK_HANDLER_CLASS = {
            let mut decl = ClassDecl::new("ZedDisplayLinkHandler", class!(NSObject))
                .expect("failed to declare ZedDisplayLinkHandler");
            extern "C" fn display_link_fired(
                this: &Object,
                _sel: Sel,
                _link: *mut Object,
            ) {
                unsafe {
                    let ptr: *mut c_void = *this.get_ivar("windowStatePtr");
                    if ptr.is_null() {
                        return;
                    }
                    let state = &*(ptr as *const Mutex<IosWindowState>);
                    let mut lock = state.lock();
                    if let Some(callback) = lock.request_frame_callback.as_mut() {
                        callback(RequestFrameOptions::default());
                    }
                }
            }
            decl.add_method(
                sel!(displayLinkFired:),
                display_link_fired as extern "C" fn(&Object, Sel, *mut Object),
            );
            decl.add_ivar::<*mut c_void>("windowStatePtr");
            decl.register() as *const _
        };
    }
}

enum TouchPhase {
    Began,
    Moved,
    Ended,
}

unsafe fn handle_key_presses(view: &Object, presses_set: *mut Object, key_down: bool) {
    let state_ptr: *mut c_void = *view.get_ivar("windowStatePtr");
    if state_ptr.is_null() {
        return;
    }
    let state = &*(state_ptr as *const Mutex<IosWindowState>);

    // Iterate the NSSet of UIPress objects.
    let count: usize = msg_send![presses_set, count];
    let enumerator: *mut Object = msg_send![presses_set, objectEnumerator];
    for _ in 0..count {
        let press: *mut Object = msg_send![enumerator, nextObject];
        if press.is_null() {
            break;
        }
        let key: *mut Object = msg_send![press, key];
        if key.is_null() {
            continue;
        }
        let characters: *mut Object = msg_send![key, characters];
        let chars_ignoring: *mut Object = msg_send![key, charactersIgnoringModifiers];
        let key_code: u32 = msg_send![key, keyCode];
        let modifier_flags: usize = msg_send![key, modifierFlags];

        let chars_str = ns_string_to_str(characters);
        let chars_ignoring_str = ns_string_to_str(chars_ignoring);

        let keystroke = crate::events::keystroke_from_ui_key(
            &chars_str,
            &chars_ignoring_str,
            key_code,
            modifier_flags,
        );

        let event = if key_down {
            key_down_event(keystroke, false)
        } else {
            key_up_event(keystroke)
        };

        let mut lock = state.lock();
        if let Some(callback) = lock.event_callback.as_mut() {
            callback(event);
        }
    }
}

unsafe fn handle_touches(view: &Object, touches_set: *mut Object, phase: TouchPhase) {
    let state_ptr: *mut c_void = *view.get_ivar("windowStatePtr");
    if state_ptr.is_null() {
        return;
    }
    let state = &*(state_ptr as *const Mutex<IosWindowState>);

    let count: usize = msg_send![touches_set, count];
    let enumerator: *mut Object = msg_send![touches_set, objectEnumerator];
    for _ in 0..count {
        let touch: *mut Object = msg_send![enumerator, nextObject];
        if touch.is_null() {
            break;
        }

        let view_obj = view as *const Object as *mut Object;
        let location: CGPoint = msg_send![touch, locationInView: view_obj];
        let position = point(px(location.x as f32), px(location.y as f32));
        let tap_count: usize = msg_send![touch, tapCount];
        let modifiers = Modifiers::default();

        let event = match phase {
            TouchPhase::Began => {
                let mut lock = state.lock();
                lock.last_touch_position = position;
                lock.touch_click_count = tap_count;
                drop(lock);
                mouse_down_from_touch(position, tap_count, modifiers)
            }
            TouchPhase::Moved => {
                mouse_move_from_pointer(position, Some(MouseButton::Left), modifiers)
            }
            TouchPhase::Ended => {
                let click_count = state.lock().touch_click_count;
                mouse_up_from_touch(position, click_count, modifiers)
            }
        };

        let mut lock = state.lock();
        if let Some(callback) = lock.event_callback.as_mut() {
            callback(event);
        }
    }
}

unsafe fn ns_string_to_str(ns_string: *mut Object) -> String {
    if ns_string.is_null() {
        return String::new();
    }
    let utf8: *const std::os::raw::c_char = msg_send![ns_string, UTF8String];
    if utf8.is_null() {
        return String::new();
    }
    std::ffi::CStr::from_ptr(utf8)
        .to_str()
        .unwrap_or("")
        .to_owned()
}

pub(crate) struct IosWindowCallbacks {
    pub request_frame_callback: Option<Box<dyn FnMut(RequestFrameOptions)>>,
    pub event_callback: Option<Box<dyn FnMut(PlatformInput) -> DispatchEventResult>>,
    pub activate_callback: Option<Box<dyn FnMut(bool)>>,
    pub hover_callback: Option<Box<dyn FnMut(bool)>>,
    pub resize_callback: Option<Box<dyn FnMut(Size<Pixels>, f32)>>,
    pub moved_callback: Option<Box<dyn FnMut()>>,
    pub should_close_callback: Option<Box<dyn FnMut() -> bool>>,
    pub close_callback: Option<Box<dyn FnOnce()>>,
    pub appearance_changed_callback: Option<Box<dyn FnMut()>>,
    pub hit_test_window_control_callback: Option<Box<dyn FnMut() -> Option<WindowControlArea>>>,
}

// Allow Mutex<IosWindowState> to refer to closures that aren't Send. Since we
// only access the state from the main thread via GCD, this is safe.
struct IosWindowState {
    handle: AnyWindowHandle,
    ui_window: *mut Object,
    metal_view: *mut Object,
    display_link_handler: *mut Object,
    display_link: *mut Object,
    renderer: MetalRenderer,
    bounds: Bounds<Pixels>,
    scale_factor: f32,
    is_active: bool,
    last_touch_position: Point<Pixels>,
    touch_click_count: usize,
    input_handler: Option<PlatformInputHandler>,
    // Callbacks
    request_frame_callback: Option<Box<dyn FnMut(RequestFrameOptions)>>,
    event_callback: Option<Box<dyn FnMut(PlatformInput) -> DispatchEventResult>>,
    activate_callback: Option<Box<dyn FnMut(bool)>>,
    hover_callback: Option<Box<dyn FnMut(bool)>>,
    resize_callback: Option<Box<dyn FnMut(Size<Pixels>, f32)>>,
    moved_callback: Option<Box<dyn FnMut()>>,
    should_close_callback: Option<Box<dyn FnMut() -> bool>>,
    close_callback: Option<Box<dyn FnOnce()>>,
    appearance_changed_callback: Option<Box<dyn FnMut()>>,
    hit_test_window_control_callback: Option<Box<dyn FnMut() -> Option<WindowControlArea>>>,
}

// Safety: IosWindowState is only accessed on the main thread (GCD main queue).
unsafe impl Send for IosWindowState {}

pub struct IosWindow(Arc<Mutex<IosWindowState>>);

impl IosWindow {
    pub fn new(
        handle: AnyWindowHandle,
        params: WindowParams,
        renderer_context: RendererContext,
    ) -> Result<Self> {
        unsafe { register_ios_classes() };

        let scale_factor = IosDisplay::scale_factor();
        let display_bounds = IosDisplay::primary().bounds();

        let bounds = params
            .bounds
            .map(|b| match b {
                gpui::WindowBounds::Windowed(b) => b,
                gpui::WindowBounds::Fullscreen(_) | gpui::WindowBounds::Maximized(_) => {
                    display_bounds
                }
            })
            .unwrap_or(display_bounds);

        let (ui_window, metal_view) = unsafe {
            let screen_bounds = CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize {
                    width: display_bounds.size.width.0 as f64,
                    height: display_bounds.size.height.0 as f64,
                },
            };

            // Create UIWindow filling the entire screen.
            let window_alloc: *mut Object = msg_send![class!(UIWindow), alloc];
            let ui_window: *mut Object = msg_send![window_alloc, initWithFrame: screen_bounds];
            let bg_color: *mut Object = msg_send![class!(UIColor), blackColor];
            let _: () = msg_send![ui_window, setBackgroundColor: bg_color];

            // Create ZedMetalView as the root view.
            let view_alloc: *mut Object = msg_send![class!(ZedMetalView), alloc];
            let metal_view: *mut Object = msg_send![view_alloc, initWithFrame: screen_bounds];

            let root_vc_alloc: *mut Object = msg_send![class!(UIViewController), alloc];
            let root_vc: *mut Object = msg_send![root_vc_alloc, init];
            let _: () = msg_send![root_vc, setView: metal_view];
            let _: () = msg_send![ui_window, setRootViewController: root_vc];
            let _: () = msg_send![ui_window, makeKeyAndVisible];

            (ui_window, metal_view)
        };

        // Compute device pixel size for the initial drawable.
        let device_size = Size::<DevicePixels> {
            width: DevicePixels((bounds.size.width.0 * scale_factor) as i32),
            height: DevicePixels((bounds.size.height.0 * scale_factor) as i32),
        };

        let mut renderer = unsafe {
            renderer::new_renderer(
                renderer_context,
                ui_window as *mut c_void,
                metal_view as *mut c_void,
                bounds.size.map(|p| p.0),
                false,
            )
        };
        renderer.update_drawable_size(device_size);

        // Attach the CAMetalLayer to the UIView.
        unsafe {
            if let Some(layer) = renderer.layer() {
                let view_layer: *mut Object = msg_send![metal_view, layer];
                let _: () = msg_send![view_layer, addSublayer: layer.as_ptr()];
                // Set the drawable size on the layer.
                let cs = CGSize {
                    width: device_size.width.0 as f64,
                    height: device_size.height.0 as f64,
                };
                let _: () = msg_send![layer.as_ptr(), setDrawableSize: cs];
                let _: () = msg_send![layer.as_ptr(), setContentsScale: scale_factor as f64];
                let bounds_for_layer = CGRect {
                    origin: CGPoint { x: 0.0, y: 0.0 },
                    size: CGSize {
                        width: bounds.size.width.0 as f64,
                        height: bounds.size.height.0 as f64,
                    },
                };
                let _: () = msg_send![layer.as_ptr(), setFrame: bounds_for_layer];
            }
        }

        let state = Arc::new(Mutex::new(IosWindowState {
            handle,
            ui_window,
            metal_view,
            display_link_handler: ptr::null_mut(),
            display_link: ptr::null_mut(),
            renderer,
            bounds,
            scale_factor,
            is_active: true,
            last_touch_position: point(px(0.0), px(0.0)),
            touch_click_count: 1,
            input_handler: None,
            request_frame_callback: None,
            event_callback: None,
            activate_callback: None,
            hover_callback: None,
            resize_callback: None,
            moved_callback: None,
            should_close_callback: None,
            close_callback: None,
            appearance_changed_callback: None,
            hit_test_window_control_callback: None,
        }));

        // Store a raw pointer to the state in the view so ObjC callbacks can
        // reach it. The Arc keeps the state alive as long as IosWindow exists.
        unsafe {
            let state_ptr = Arc::as_ptr(&state) as *mut c_void;
            let view_obj = metal_view as *mut Object;
            (*view_obj).set_ivar("windowStatePtr", state_ptr);
        }

        Ok(IosWindow(state))
    }

    fn start_display_link(&self) {
        let mut lock = self.0.lock();
        if !lock.display_link.is_null() {
            return;
        }
        unsafe {
            let handler_alloc: *mut Object =
                msg_send![class!(ZedDisplayLinkHandler), alloc];
            let handler: *mut Object = msg_send![handler_alloc, init];

            let state_ptr = Arc::as_ptr(&self.0) as *mut c_void;
            let handler_obj = handler as *mut Object;
            (*handler_obj).set_ivar("windowStatePtr", state_ptr);

            let display_link: *mut Object = msg_send![
                class!(CADisplayLink),
                displayLinkWithTarget: handler
                selector: sel!(displayLinkFired:)
            ];

            // Prefer 120fps on ProMotion displays; fall back gracefully.
            let preferred_fps_range_min: f64 = 24.0;
            let preferred_fps_range_max: f64 = 120.0;
            let preferred_fps_range_preferred: f64 = 60.0;

            // Use preferredFrameRateRange (iOS 15+). Ignore errors on older OS.
            struct CAFrameRateRange {
                minimum: f32,
                maximum: f32,
                preferred: f32,
            }
            #[allow(dead_code)]
            let range = CAFrameRateRange {
                minimum: preferred_fps_range_min as f32,
                maximum: preferred_fps_range_max as f32,
                preferred: preferred_fps_range_preferred as f32,
            };

            let run_loop: *mut Object = msg_send![class!(NSRunLoop), currentRunLoop];
            let common_mode: *mut Object =
                msg_send![class!(NSRunLoopCommonModes), class];
            let _: () = msg_send![display_link, addToRunLoop: run_loop forMode: common_mode];

            lock.display_link = display_link;
            lock.display_link_handler = handler;
        }
    }

    fn stop_display_link(&self) {
        let mut lock = self.0.lock();
        if lock.display_link.is_null() {
            return;
        }
        unsafe {
            let _: () = msg_send![lock.display_link, invalidate];
        }
        lock.display_link = ptr::null_mut();
        lock.display_link_handler = ptr::null_mut();
    }
}

impl HasWindowHandle for IosWindow {
    fn window_handle(&self) -> std::result::Result<WindowHandle<'_>, HandleError> {
        let lock = self.0.lock();
        let raw = UiKitWindowHandle::new(
            NonNull::new(lock.ui_window as *mut c_void).expect("ui_window is null"),
        );
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::UiKit(raw)) })
    }
}

impl HasDisplayHandle for IosWindow {
    fn display_handle(&self) -> std::result::Result<DisplayHandle<'_>, HandleError> {
        let raw = UiKitDisplayHandle::new();
        Ok(unsafe { DisplayHandle::borrow_raw(RawDisplayHandle::UiKit(raw)) })
    }
}

impl PlatformWindow for IosWindow {
    fn bounds(&self) -> Bounds<Pixels> {
        self.0.lock().bounds
    }

    fn is_maximized(&self) -> bool {
        // On iOS, the window always fills the screen (or scene in Stage Manager).
        true
    }

    fn window_bounds(&self) -> WindowBounds {
        WindowBounds::Fullscreen(self.0.lock().bounds)
    }

    fn content_size(&self) -> Size<Pixels> {
        self.0.lock().bounds.size
    }

    fn resize(&mut self, size: Size<Pixels>) {
        let mut lock = self.0.lock();
        lock.bounds.size = size;
        let scale = lock.scale_factor;
        let device_size = Size::<DevicePixels> {
            width: DevicePixels((size.width.0 * scale) as i32),
            height: DevicePixels((size.height.0 * scale) as i32),
        };
        lock.renderer.update_drawable_size(device_size);
    }

    fn scale_factor(&self) -> f32 {
        self.0.lock().scale_factor
    }

    fn appearance(&self) -> WindowAppearance {
        // Respect the UIUserInterfaceStyle (dark/light) of the UITraitCollection.
        // For Phase 1, default to dark mode to match Zed's default theme.
        unsafe {
            let lock = self.0.lock();
            let trait_collection: *mut Object =
                msg_send![lock.ui_window, traitCollection];
            let style: usize = msg_send![trait_collection, userInterfaceStyle];
            // UIUserInterfaceStyleDark = 2
            if style == 2 {
                WindowAppearance::Dark
            } else {
                WindowAppearance::Light
            }
        }
    }

    fn display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        Some(Rc::new(IosDisplay::primary()))
    }

    fn mouse_position(&self) -> Point<Pixels> {
        self.0.lock().last_touch_position
    }

    fn modifiers(&self) -> Modifiers {
        Modifiers::default()
    }

    fn capslock(&self) -> Capslock {
        Capslock::Off
    }

    fn set_input_handler(&mut self, input_handler: PlatformInputHandler) {
        self.0.lock().input_handler = Some(input_handler);
    }

    fn take_input_handler(&mut self) -> Option<PlatformInputHandler> {
        self.0.lock().input_handler.take()
    }

    fn prompt(
        &self,
        _level: PromptLevel,
        _msg: &str,
        _detail: Option<&str>,
        _answers: &[PromptButton],
    ) -> Option<futures::channel::oneshot::Receiver<usize>> {
        // TODO: implement UIAlertController prompt
        None
    }

    fn activate(&self) {
        unsafe {
            let lock = self.0.lock();
            let _: () = msg_send![lock.ui_window, makeKeyAndVisible];
        }
    }

    fn is_active(&self) -> bool {
        self.0.lock().is_active
    }

    fn is_hovered(&self) -> bool {
        // No hover concept on iOS (trackpad hover is handled via UIPointerInteraction
        // separately). Return false for now.
        false
    }

    fn background_appearance(&self) -> WindowBackgroundAppearance {
        WindowBackgroundAppearance::Opaque
    }

    fn set_title(&mut self, _title: &str) {
        // UIWindow has no title bar; no-op.
    }

    fn set_background_appearance(&self, _appearance: WindowBackgroundAppearance) {}

    fn minimize(&self) {}

    fn zoom(&self) {}

    fn toggle_fullscreen(&self) {}

    fn is_fullscreen(&self) -> bool {
        true
    }

    fn on_request_frame(&self, callback: Box<dyn FnMut(RequestFrameOptions)>) {
        self.0.lock().request_frame_callback = Some(callback);
        self.start_display_link();
    }

    fn on_input(&self, callback: Box<dyn FnMut(PlatformInput) -> DispatchEventResult>) {
        self.0.lock().event_callback = Some(callback);
    }

    fn on_active_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.0.lock().activate_callback = Some(callback);
    }

    fn on_hover_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.0.lock().hover_callback = Some(callback);
    }

    fn on_resize(&self, callback: Box<dyn FnMut(Size<Pixels>, f32)>) {
        self.0.lock().resize_callback = Some(callback);
    }

    fn on_moved(&self, callback: Box<dyn FnMut()>) {
        self.0.lock().moved_callback = Some(callback);
    }

    fn on_should_close(&self, callback: Box<dyn FnMut() -> bool>) {
        self.0.lock().should_close_callback = Some(callback);
    }

    fn on_hit_test_window_control(&self, callback: Box<dyn FnMut() -> Option<WindowControlArea>>) {
        self.0.lock().hit_test_window_control_callback = Some(callback);
    }

    fn on_close(&self, callback: Box<dyn FnOnce()>) {
        self.0.lock().close_callback = Some(callback);
    }

    fn on_appearance_changed(&self, callback: Box<dyn FnMut()>) {
        self.0.lock().appearance_changed_callback = Some(callback);
    }

    fn draw(&self, scene: &Scene) {
        self.0.lock().renderer.draw(scene);
    }

    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
        self.0.lock().renderer.sprite_atlas().clone()
    }

    fn is_subpixel_rendering_supported(&self) -> bool {
        // iOS Retina displays don't use subpixel AA.
        false
    }

    fn update_ime_position(&self, _bounds: Bounds<Pixels>) {
        // UITextInput position management — implemented when full UITextInput
        // protocol is wired up (Phase 1.5).
    }

    fn gpu_specs(&self) -> Option<GpuSpecs> {
        None
    }
}

impl Drop for IosWindow {
    fn drop(&mut self) {
        self.stop_display_link();

        // Fire the close callback if set.
        let mut lock = self.0.lock();
        if let Some(callback) = lock.close_callback.take() {
            drop(lock);
            callback();
        }
    }
}
