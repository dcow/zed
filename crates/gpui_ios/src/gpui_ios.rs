#![cfg(target_os = "ios")]
//! iOS platform implementation for GPUI.
//!
//! iPadOS uses UIKit exclusively — AppKit is not available and must not be linked.
//! The run loop is owned by UIKit (`UIApplicationMain` in Swift); GPUI's
//! `Platform::run` callback is invoked from our `zed_ios_main` entry point
//! once UIApplication has finished launching.
//!
//! See: docs/ios-port-plan.md Phase 1 for full details.

mod dispatcher;
mod display;
mod events;
mod keyboard;
mod metal_atlas;
pub mod metal_renderer;
mod platform;
mod text_system;
mod window;

use metal_renderer as renderer;

use cocoa::{
    base::{id, nil},
    foundation::{NSString, NSUInteger},
};
use objc::runtime::{BOOL, NO, YES};
use std::{
    ffi::{CStr, c_char},
    ops::Range,
};

pub(crate) use dispatcher::*;
pub(crate) use display::*;
pub(crate) use events::*;
pub(crate) use keyboard::*;
pub(crate) use platform::*;
pub(crate) use window::*;

pub use platform::IosPlatform;

trait BoolExt {
    fn to_objc(self) -> BOOL;
}

impl BoolExt for bool {
    fn to_objc(self) -> BOOL {
        if self { YES } else { NO }
    }
}

trait NSStringExt {
    unsafe fn to_str(&self) -> &str;
}

impl NSStringExt for id {
    unsafe fn to_str(&self) -> &str {
        unsafe {
            let cstr = self.UTF8String();
            if cstr.is_null() {
                ""
            } else {
                CStr::from_ptr(cstr as *mut c_char).to_str().unwrap_or("")
            }
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub(crate) struct NSRange {
    pub location: NSUInteger,
    pub length: NSUInteger,
}

impl NSRange {
    pub(crate) fn invalid() -> Self {
        // NSNotFound = NSIntegerMax; use usize::MAX as sentinel
        Self {
            location: usize::MAX,
            length: 0,
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        self.location != usize::MAX
    }

    pub(crate) fn to_range(self) -> Option<Range<usize>> {
        if self.is_valid() {
            let start = self.location;
            let end = start + self.length;
            Some(start..end)
        } else {
            None
        }
    }
}

impl From<Range<usize>> for NSRange {
    fn from(range: Range<usize>) -> Self {
        NSRange {
            location: range.start,
            length: range.len(),
        }
    }
}

unsafe impl objc::Encode for NSRange {
    fn encode() -> objc::Encoding {
        let encoding = format!(
            "{{NSRange={}{}}}",
            NSUInteger::encode().as_str(),
            NSUInteger::encode().as_str()
        );
        unsafe { objc::Encoding::from_str(&encoding) }
    }
}

/// Helper to create an NSString from a &str.
///
/// # Safety
/// The returned id is autoreleased and must not outlive the current autorelease pool.
#[allow(clippy::disallowed_methods)]
pub(crate) unsafe fn ns_string(string: &str) -> id {
    unsafe { NSString::alloc(nil).init_str(string).autorelease() }
}
