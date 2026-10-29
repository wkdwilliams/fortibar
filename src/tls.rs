//! Lets sign-in windows accept a gateway certificate that macOS rejects (e.g. a
//! gateway reached by IP, so the name doesn't match), but only when the
//! certificate's SHA-256 equals the digest pinned for that VPN.
//!
//! wry's navigation delegate doesn't handle auth challenges, so we add
//! `webView:didReceiveAuthenticationChallenge:completionHandler:` to its class
//! at runtime.

use crate::config::Vpn;
use block2::Block;
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{class, msg_send, sel, Encoding, RefEncode};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr};
use std::sync::Mutex;

/// host -> lowercase hex SHA-256 of the gateway's leaf certificate.
static PINS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

pub fn set_pins(vpns: &[Vpn]) {
    let pins = vpns
        .iter()
        .filter(|v| !v.trusted_cert.is_empty())
        .map(|v| (v.host.clone(), v.trusted_cert.to_ascii_lowercase()))
        .collect();
    *PINS.lock().unwrap() = Some(pins);
}

#[repr(C)]
struct SecTrust([u8; 0]);
unsafe impl RefEncode for SecTrust {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("__SecTrust", &[]));
}

#[link(name = "Security", kind = "framework")]
extern "C" {
    fn SecTrustCopyCertificateChain(trust: *const SecTrust) -> *const c_void;
    fn SecCertificateCopyData(cert: *const c_void) -> *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFArrayGetCount(array: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *const c_void;
    fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
    fn CFDataGetLength(data: *const c_void) -> isize;
    fn CFRelease(obj: *const c_void);
}

unsafe fn leaf_sha256(trust: *const SecTrust) -> Option<String> {
    let chain = SecTrustCopyCertificateChain(trust);
    if chain.is_null() {
        return None;
    }
    let mut digest = None;
    if CFArrayGetCount(chain) > 0 {
        let data = SecCertificateCopyData(CFArrayGetValueAtIndex(chain, 0));
        if !data.is_null() {
            let der = std::slice::from_raw_parts(CFDataGetBytePtr(data), CFDataGetLength(data) as usize);
            digest = Some(Sha256::digest(der).iter().map(|b| format!("{b:02x}")).collect());
            CFRelease(data);
        }
    }
    CFRelease(chain);
    digest
}

unsafe fn ns_string(obj: *mut AnyObject) -> String {
    if obj.is_null() {
        return String::new();
    }
    let ptr: *const c_char = msg_send![obj, UTF8String];
    if ptr.is_null() {
        String::new()
    } else {
        CStr::from_ptr(ptr).to_string_lossy().into_owned()
    }
}

// NSURLSessionAuthChallengeDisposition
const USE_CREDENTIAL: isize = 0;
const DEFAULT_HANDLING: isize = 1;

unsafe extern "C-unwind" fn did_receive_challenge(
    _this: *mut AnyObject,
    _sel: Sel,
    _webview: *mut AnyObject,
    challenge: *mut AnyObject,
    handler: &Block<dyn Fn(isize, *mut AnyObject)>,
) {
    let space: *mut AnyObject = msg_send![challenge, protectionSpace];
    let method = ns_string(msg_send![space, authenticationMethod]);
    if method == "NSURLAuthenticationMethodServerTrust" {
        let host = ns_string(msg_send![space, host]).to_ascii_lowercase();
        let pinned = PINS.lock().unwrap().as_ref().and_then(|p| p.get(&host).cloned());
        let trust: *const SecTrust = msg_send![space, serverTrust];
        if let (Some(pinned), false) = (pinned, trust.is_null()) {
            if leaf_sha256(trust).as_deref() == Some(pinned.as_str()) {
                let credential: *mut AnyObject = msg_send![class!(NSURLCredential), credentialForTrust: trust];
                handler.call((USE_CREDENTIAL, credential));
                return;
            }
        }
    }
    handler.call((DEFAULT_HANDLING, std::ptr::null_mut()));
}

/// Must run after wry has registered its delegate class but before the
/// webviews that need it are created: WebKit checks which delegate methods
/// exist when the delegate is set.
pub fn install() {
    let Some(cls) = AnyClass::get(c"WryNavigationDelegate") else {
        eprintln!("fortibar: wry delegate class not found; pinned certs won't apply to sign-in");
        return;
    };
    let sel = sel!(webView:didReceiveAuthenticationChallenge:completionHandler:);
    unsafe {
        let imp: Imp = std::mem::transmute(
            did_receive_challenge
                as unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject, &Block<dyn Fn(isize, *mut AnyObject)>),
        );
        let added = objc2::ffi::class_addMethod(cls as *const AnyClass as *mut AnyClass, sel, imp, c"v@:@@@?".as_ptr());
        if !added.as_bool() {
            eprintln!("fortibar: couldn't add certificate handler to wry delegate");
        }
    }
}
