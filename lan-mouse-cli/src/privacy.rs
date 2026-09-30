// DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
//! macOS prompts are initiated explicitly by a user action, never by a poll.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyPane {
    Accessibility,
    InputMonitoring,
    LocalNetwork,
}

#[derive(Serialize)]
pub struct PrivacyStatus {
    pub supported: bool,
    pub accessibility: bool,
    pub input_monitoring: bool,
    pub input_control: bool,
}

pub fn status() -> PrivacyStatus {
    #[cfg(target_os = "macos")]
    return macos::status();
    #[cfg(not(target_os = "macos"))]
    PrivacyStatus {
        supported: false,
        accessibility: false,
        input_monitoring: false,
        input_control: false,
    }
}

pub fn request() -> Result<PrivacyStatus, String> {
    #[cfg(target_os = "macos")]
    macos::request();
    Ok(status())
}

pub fn open(pane: PrivacyPane) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let suffix = match pane {
            PrivacyPane::Accessibility => "Privacy_Accessibility",
            PrivacyPane::InputMonitoring => "Privacy_ListenEvent",
            PrivacyPane::LocalNetwork => "Privacy_LocalNetwork",
        };
        std::process::Command::new("/usr/bin/open")
            .arg(format!(
                "x-apple.systempreferences:com.apple.preference.security?{suffix}"
            ))
            .spawn()
            .map_err(|error| error.to_string())?;
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pane;
        Err("此入口仅用于 macOS，请使用系统权限设置".into())
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::PrivacyStatus;
    use core_foundation::{
        base::TCFType,
        boolean::CFBoolean,
        dictionary::CFDictionary,
        string::{CFString, CFStringRef},
    };
    use std::ffi::{c_uchar, c_void};

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> c_uchar;
        fn AXIsProcessTrustedWithOptions(options: *const c_void) -> c_uchar;
        static kAXTrustedCheckOptionPrompt: CFStringRef;
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGPreflightListenEventAccess() -> bool;
        fn CGPreflightPostEventAccess() -> bool;
        fn CGRequestListenEventAccess() -> bool;
        fn CGRequestPostEventAccess() -> bool;
    }
    pub fn status() -> PrivacyStatus {
        // Preflight checks do not display dialogs or change a grant.
        unsafe {
            PrivacyStatus {
                supported: true,
                accessibility: AXIsProcessTrusted() != 0,
                input_monitoring: CGPreflightListenEventAccess(),
                input_control: CGPreflightPostEventAccess(),
            }
        }
    }
    pub fn request() {
        let current = status();
        unsafe {
            if !current.accessibility {
                let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt);
                let options = CFDictionary::from_CFType_pairs(&[(key, CFBoolean::true_value())]);
                AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef().cast());
            } else {
                if !current.input_monitoring {
                    CGRequestListenEventAccess();
                }
                if !current.input_control {
                    CGRequestPostEventAccess();
                }
            }
        }
    }
}
