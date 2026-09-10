//! Native macOS services. Permissions are requested only when used.

use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::string::{CFString, CFStringRef};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOPSCopyPowerSourcesInfo() -> CFTypeRef;
    fn IOPSCopyPowerSourcesList(info: CFTypeRef) -> CFArrayRef;
    fn IOPSGetPowerSourceDescription(info: CFTypeRef, source: CFTypeRef) -> CFDictionaryRef;
    fn IOPSGetProvidingPowerSourceType(info: CFTypeRef) -> CFStringRef;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightPostEventAccess() -> bool;
    fn CGRequestPostEventAccess() -> bool;
    fn CGEventSourceFlagsState(state: i32) -> u64;
    fn CGEventSourceKeyState(state: i32, key: u16) -> bool;
}

fn power_info() -> Option<CFType> {
    // Copy functions transfer ownership; the CF wrappers release on drop.
    let info = unsafe { IOPSCopyPowerSourcesInfo() };
    (!info.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(info) })
}

pub fn on_ac_power() -> bool {
    let Some(info) = power_info() else {
        return false;
    };
    let source = unsafe { IOPSGetProvidingPowerSourceType(info.as_CFTypeRef()) };
    !source.is_null()
        && unsafe { CFString::wrap_under_get_rule(source) } == CFString::new("AC Power")
}

fn is_internal_battery(description: &CFDictionary<CFString, CFType>) -> bool {
    let kind = description
        .find(CFString::new("Type"))
        .and_then(|v| v.downcast::<CFString>());
    let present = description
        .find(CFString::new("Is Present"))
        .and_then(|v| v.downcast::<CFBoolean>());
    kind == Some(CFString::new("InternalBattery")) && present.is_some_and(bool::from)
}

pub fn battery_present() -> bool {
    let Some(info) = power_info() else {
        return false;
    };
    let sources = unsafe { IOPSCopyPowerSourcesList(info.as_CFTypeRef()) };
    if sources.is_null() {
        return false;
    }
    let sources = unsafe { CFArray::<CFType>::wrap_under_create_rule(sources) };
    sources.iter().any(|source| {
        let description =
            unsafe { IOPSGetPowerSourceDescription(info.as_CFTypeRef(), source.as_CFTypeRef()) };
        !description.is_null()
            && is_internal_battery(&unsafe { CFDictionary::wrap_under_get_rule(description) })
    })
}

/// Called on the recording thread before opening CoreAudio. A denied mic
/// must produce an actionable error instead of recording silent samples.
pub fn ensure_microphone_access() -> Result<(), String> {
    use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
    let media = unsafe { AVMediaTypeAudio }.ok_or("macOS audio capture is unavailable")?;
    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media) };
    if status == AVAuthorizationStatus::Authorized {
        return Ok(());
    }
    if status == AVAuthorizationStatus::NotDetermined {
        let (tx, rx) = mpsc::channel();
        let completion = block2::RcBlock::new(move |granted| {
            let _ = tx.send(bool::from(granted));
        });
        unsafe {
            AVCaptureDevice::requestAccessForMediaType_completionHandler(media, &completion);
        }
        match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(true) => return Ok(()),
            Err(_) => return Err("Microphone permission is pending. Respond to the macOS dialog, then start dictation again.".into()),
            Ok(false) => {}
        }
    }
    Err("Allow Tiro in System Settings → Privacy & Security → Microphone, then try again.".into())
}

static PASTE_GATE: Mutex<()> = Mutex::new(());

/// The paste chord is Command+V on macOS. Keep physical modifiers out of
/// it; if the user continues holding a shortcut, leave the text copied.
pub fn paste() -> Result<(), String> {
    use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    let _guard = PASTE_GATE.lock().unwrap_or_else(|e| e.into_inner());
    if !unsafe { CGPreflightPostEventAccess() } && !unsafe { CGRequestPostEventAccess() } {
        return Err("Text copied. Allow Tiro in System Settings → Privacy & Security → Accessibility to paste at the cursor.".into());
    }
    // kVK_ANSI_V; HID state is physical keyboard state. Caps Lock and Fn
    // are intentionally excluded from the shortcut-modifier mask.
    const V: u16 = 9;
    let modifiers = (CGEventFlags::CGEventFlagShift
        | CGEventFlags::CGEventFlagControl
        | CGEventFlags::CGEventFlagAlternate
        | CGEventFlags::CGEventFlagCommand)
        .bits();
    let started = Instant::now();
    while unsafe { CGEventSourceFlagsState(1) } & modifiers != 0
        || unsafe { CGEventSourceKeyState(1, V) }
    {
        if started.elapsed() >= Duration::from_secs(2) {
            return Err("Text copied. Release the shortcut keys and paste with Command+V.".into());
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    std::thread::sleep(Duration::from_millis(120));
    let source = CGEventSource::new(CGEventSourceStateID::Private)
        .map_err(|_| "Could not create a macOS keyboard event source")?;
    // Construct both events before posting so allocation failure cannot
    // leave the target with a key-down and no matching key-up.
    let down = CGEvent::new_keyboard_event(source.clone(), V, true)
        .map_err(|_| "Could not create the paste key-down event")?;
    let up = CGEvent::new_keyboard_event(source, V, false)
        .map_err(|_| "Could not create the paste key-up event")?;
    down.set_flags(CGEventFlags::CGEventFlagCommand);
    up.set_flags(CGEventFlags::CGEventFlagCommand);
    down.post(CGEventTapLocation::HID);
    up.post(CGEventTapLocation::HID);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_present_internal_battery_makes_a_laptop() {
        for (kind, present, expected) in [
            ("InternalBattery", true, true),
            ("InternalBattery", false, false),
            ("UPS", true, false),
            ("ExternalBattery", true, false),
        ] {
            let description = CFDictionary::from_CFType_pairs(&[
                (CFString::new("Type"), CFString::new(kind).as_CFType()),
                (
                    CFString::new("Is Present"),
                    CFBoolean::from(present).as_CFType(),
                ),
            ]);
            assert_eq!(is_internal_battery(&description), expected, "{kind}");
        }
        let empty = CFDictionary::from_CFType_pairs(&[]);
        assert!(!is_internal_battery(&empty));
    }
}
