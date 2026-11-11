//! Gives the Dock the app's icon however it was launched, in the variant that suits the system's
//! light or dark mode.
//!
//! macOS reads the icon from the bundle, which only helps when there is a bundle it recognizes.
//! Launched with `cargo run`, or still running after its folder has moved, the Dock falls back to a
//! generic icon. Setting it from the binary covers those cases too, and lets it follow the system's
//! appearance while the app runs: the bundle's own icon (Finder, Launchpad) is the light one.

use std::ffi::{c_char, c_void};

const ICON_LIGHT: &[u8] = include_bytes!("../assets/AppIcon-light.png");
const ICON_DARK: &[u8] = include_bytes!("../assets/AppIcon-dark.png");

/// Whether macOS is in dark mode, read before any window exists to know which to ask.
pub fn system_is_dark() -> bool {
    std::process::Command::new("/usr/bin/defaults")
        .args(["read", "-g", "AppleInterfaceStyle"])
        .output()
        .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).trim() == "Dark")
}

type Id = *mut c_void;

#[link(name = "AppKit", kind = "framework")]
extern "C" {}

#[link(name = "objc")]
extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    fn objc_msgSend();
    fn objc_autoreleasePoolPush() -> *mut c_void;
    fn objc_autoreleasePoolPop(pool: *mut c_void);
}

/// Must run on the main thread, once `NSApplication` exists. `dark` picks the variant for a dark Dock.
pub fn apply(dark: bool) {
    let png = if dark { ICON_DARK } else { ICON_LIGHT };
    type SendId = unsafe extern "C" fn(Id, *mut c_void) -> Id;
    type SendObject = unsafe extern "C" fn(Id, *mut c_void, Id) -> Id;
    type SendBytes = unsafe extern "C" fn(Id, *mut c_void, *const u8, usize) -> Id;
    type SendSetObject = unsafe extern "C" fn(Id, *mut c_void, Id);
    type SendVoid = unsafe extern "C" fn(Id, *mut c_void);
    // SAFETY: The class and selector names are NUL-terminated literals of real AppKit and Foundation
    // API with exactly these signatures, and `objc_msgSend` must be called through a cast to the
    // signature of the method it dispatches to. `png` is static, so it outlives the call, and NSData
    // copies it.
    // The image comes from `alloc`, so it is checked for null and released once; NSApplication
    // retains what it is given.
    unsafe {
        let send_id = std::mem::transmute::<unsafe extern "C" fn(), SendId>(objc_msgSend);
        let send_object = std::mem::transmute::<unsafe extern "C" fn(), SendObject>(objc_msgSend);
        let send_bytes = std::mem::transmute::<unsafe extern "C" fn(), SendBytes>(objc_msgSend);
        let send_set = std::mem::transmute::<unsafe extern "C" fn(), SendSetObject>(objc_msgSend);
        let send_void = std::mem::transmute::<unsafe extern "C" fn(), SendVoid>(objc_msgSend);

        let pool = objc_autoreleasePoolPush();
        let data = send_bytes(
            objc_getClass(c"NSData".as_ptr()),
            sel_registerName(c"dataWithBytes:length:".as_ptr()),
            png.as_ptr(),
            png.len(),
        );
        let image = send_id(
            objc_getClass(c"NSImage".as_ptr()),
            sel_registerName(c"alloc".as_ptr()),
        );
        let image = send_object(image, sel_registerName(c"initWithData:".as_ptr()), data);
        if !image.is_null() {
            let app = send_id(
                objc_getClass(c"NSApplication".as_ptr()),
                sel_registerName(c"sharedApplication".as_ptr()),
            );
            send_set(
                app,
                sel_registerName(c"setApplicationIconImage:".as_ptr()),
                image,
            );
            send_void(image, sel_registerName(c"release".as_ptr()));
        }
        objc_autoreleasePoolPop(pool);
    }
}
