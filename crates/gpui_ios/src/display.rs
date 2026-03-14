use anyhow::Result;
use gpui::{Bounds, DisplayId, Pixels, PlatformDisplay, Point, Size, px};
use objc::{class, msg_send, sel, sel_impl};
use uuid::Uuid;

use std::sync::OnceLock;

/// Display abstraction backed by UIScreen.
///
/// iOS/iPadOS only has a single main screen (UIScreen.mainScreen). External
/// display support via UIScene is a future enhancement; for Phase 1 we expose
/// exactly one display at all times.
pub(crate) struct IosDisplay {
    id: DisplayId,
    uuid: Uuid,
}

impl IosDisplay {
    /// Returns the primary (and only) iOS display.
    pub fn primary() -> Self {
        Self {
            id: DisplayId::new(1),
            uuid: stable_uuid(),
        }
    }

    /// Returns the bounds of the main UIScreen in points.
    fn screen_bounds_points() -> Bounds<Pixels> {
        unsafe {
            let main_screen: *mut objc::runtime::Object =
                msg_send![class!(UIScreen), mainScreen];
            let bounds: CGRect = msg_send![main_screen, bounds];
            Bounds {
                origin: Point::default(),
                size: Size {
                    width: px(bounds.size.width as f32),
                    height: px(bounds.size.height as f32),
                },
            }
        }
    }

    /// Returns the native scale factor for the main UIScreen.
    pub fn scale_factor() -> f32 {
        unsafe {
            let main_screen: *mut objc::runtime::Object =
                msg_send![class!(UIScreen), mainScreen];
            let scale: f64 = msg_send![main_screen, nativeScale];
            scale as f32
        }
    }
}

impl PlatformDisplay for IosDisplay {
    fn id(&self) -> DisplayId {
        self.id
    }

    fn uuid(&self) -> Result<Uuid> {
        Ok(self.uuid)
    }

    fn bounds(&self) -> Bounds<Pixels> {
        Self::screen_bounds_points()
    }

    fn visible_bounds(&self) -> Bounds<Pixels> {
        // On iOS the full screen rect is the visible area for our purposes.
        // Safe area insets are handled at the window level.
        Self::screen_bounds_points()
    }
}

/// Returns a stable UUID for the primary display, generated once per process.
///
/// iOS doesn't expose persistent display UUIDs the way macOS does, so we
/// generate one at startup and cache it for the life of the process.
fn stable_uuid() -> Uuid {
    static UUID: OnceLock<Uuid> = OnceLock::new();
    *UUID.get_or_init(Uuid::new_v4)
}

/// CGRect as returned by UIKit Objective-C APIs.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CGPoint {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CGSize {
    pub width: f64,
    pub height: f64,
}

unsafe impl objc::Encode for CGRect {
    fn encode() -> objc::Encoding {
        // Matches the ObjC encoding for CGRect on arm64
        unsafe { objc::Encoding::from_str("{CGRect={CGPoint=dd}{CGSize=dd}}") }
    }
}

unsafe impl objc::Encode for CGPoint {
    fn encode() -> objc::Encoding {
        unsafe { objc::Encoding::from_str("{CGPoint=dd}") }
    }
}

unsafe impl objc::Encode for CGSize {
    fn encode() -> objc::Encoding {
        unsafe { objc::Encoding::from_str("{CGSize=dd}") }
    }
}
