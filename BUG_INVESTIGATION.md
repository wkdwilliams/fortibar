# Bug — SSO window stays blank after a successful Microsoft sign-in (`SSO sign-in`)

**Date:** 2026-11-13
**Feature area:** SSO (SAML) sign-in window, `src/main.rs`
**Files fixed:** `src/main.rs`
**Status:** FIXED (root cause in FortiBar confirmed by reading the code; the gateway-side trigger is likely but not reproduced)

**Report:** First SSO connect on a new Mac. Microsoft sign-in (with a slow MFA approval)
succeeded, but the sign-in window went blank and never closed or connected. Closing it and
connecting again worked straight away.

**Why the second try worked:** `start_saml` deletes any old `SVPNCOOKIE` first, so the second
connect wasn't reusing a session cookie. It ran SAML again, and because Microsoft was now signed
in, it went through silently.

---

## Issues

### [MUST FIX] 1. A gateway redirect with no usable SAML id is silently dropped

**File:** `src/main.rs:636–642` (navigation handler) and `src/main.rs:746–753` (`AuthSamlId`)
**Impact score:** 8/10
**Reason:** The gateway ends SAML by redirecting to `http://127.0.0.1:8020/?id=…`. Any redirect
there is cancelled. If it had no `id` (or an unexpected one), no event was sent at all. The
window stayed on Microsoft's last page, which is an auto-submit form and looks blank. Nothing
ever timed out. openfortivpn's own listener (`vendor/openfortivpn-src/src/http_server.c:169–210`)
treats the same case as an error ("Invalid redirect response from Fortinet server").

**Current code:**
```rust
if local {
    if let Some(saml_id) = query_param(&url, "id") {
        let _ = nav_proxy.send_event(UserEvent::AuthSamlId(nav_id.clone(), saml_id));
    }
}
// …
if let (true, true, Some(vpn)) = (signing_in, safe, self.vpn(&id)) { /* load auth_id */ }
```

**Fix:**
```rust
if local {
    let saml_id = query_param(&url, "id").unwrap_or_default();
    let _ = nav_proxy.send_event(UserEvent::AuthSamlId(nav_id.clone(), saml_id));
}
// …
if saml_id.is_empty() || !safe {
    return self.saml_failed(target, &id);
}
```

### [MUST FIX] 2. An `auth_id` exchange that doesn't produce a session cookie waits forever

**File:** `src/main.rs:731–753`
**Impact score:** 8/10
**Reason:** After loading `/remote/saml/auth_id?id=…`, FortiBar only polled for `SVPNCOOKIE`. If the
gateway refused the id (for example because the SAML login took longer than it allows, which is
common with MFA push approvals; FortiGate's `remoteauthtimeout` is short by default), no cookie
ever arrived. Polling ran forever and the user saw a dead window.

**Fix:** loading `auth_id` now schedules `AuthFailed(id, attempt)` after `SAML_EXCHANGE_TIMEOUT`
(10s). If sign-in is still pending then, it checks for the cookie one last time and then calls
`saml_failed`.

### Recovery: `saml_failed`

Both cases now call `saml_failed`:
- The first time, it restarts SAML once (`saml_retried`). Microsoft is signed in by then, so this
  normally connects with no user action, which is what the user did by hand.
- If that also fails, it hides the window and shows "Couldn't sign in to <VPN>".
- During an automatic reconnect it doesn't show an alert. It schedules the next attempt instead,
  the same as other failed automatic attempts.

`saml_retried` is reset on every new connect (`start_auth_with`), not on the internal restart.

---

## Not verified

- I couldn't reproduce this here: it needs a fresh Microsoft sign-in with a slow MFA approval
  on a gateway that drops it. I didn't capture which of the two dead ends the colleague hit (the
  sign-in log isn't kept). Both are now handled.
- I checked what the gateway returns for an invalid `auth_id`: a visible `400 Bad Request` page
  that clears `SVPNCOOKIE`. That case used to hang too, and is now caught by the 10s timeout.

---

## Summary Table

| # | Severity | File | Line(s) | Impact | Issue | Action |
|---|----------|------|---------|--------|-------|--------|
| 1 | MUST FIX | `src/main.rs` | 636–642, 746–753 | 8/10 | Local SAML redirect without a usable id dropped silently → blank window forever | Change |
| 2 | MUST FIX | `src/main.rs` | 731–753 | 8/10 | `auth_id` exchange with no session cookie never times out | Add timeout + `saml_failed` (retry once, then alert) |
