//! core9000: what the 9000 series shares. text9000, splice9000, wave9000,
//! grace9000 and leaf9000 are sisters; this is the part of them that is the
//! same, written once:
//!
//! - [`shell`]: the window, drawn by the CPU (winit + egui), with no
//!   graphics driver loaded. leaf9000 draws its own and leaves it out.
//! - [`adapters`]: the adapter host. Every sister runs add-ons the same way:
//!   an `adapter.toml`, a program speaking JSON lines, approvals, installing
//!   from a dropped zip, removing built-in ones.
//! - [`family`]: the protocol every sister speaks to adapters, on top of
//!   which each adds its own requests (`edit.*`, `doc.*`, `book.*`).
//! - [`sisters`]: the five apps, which files each opens, and handing a file
//!   to the sister that opens it.
//! - [`watch`], [`incoming`], [`log`]: config folders watched for changes,
//!   files the system hands over, and the Log panel's lines.
//!
//! Each app calls [`init`] first, with who it is.

use std::sync::OnceLock;

pub mod adapters;
pub mod family;
#[cfg(feature = "shell")]
pub mod incoming;
pub mod log;
#[cfg(feature = "shell")]
pub mod shell;
pub mod sisters;
pub mod watch;

/// Who the app using core9000 is.
#[derive(Debug)]
pub struct App {
    /// "splice9000": also how it's found among the sisters.
    pub name: &'static str,
    /// Its version, `env!("CARGO_PKG_VERSION")`.
    pub version: &'static str,
    /// Its own environment variables start with this: "SPLICE9000".
    pub env_prefix: &'static str,
    /// An older prefix still read after it ("FOLIO" for text9000), or "".
    pub legacy_env_prefix: &'static str,
    /// The adapter protocol version of its own requests (see [`family`]).
    pub protocol: u32,
    /// The permissions its adapters can ask for, with what each allows in
    /// words for the install prompt.
    pub permissions: &'static [(&'static str, &'static str)],
    /// The permission an adapter needs to receive events (seeing the work).
    pub events_permission: &'static str,
}

static APP: OnceLock<&'static App> = OnceLock::new();

/// Says which app this is. Call once, first thing.
pub fn init(app: &'static App) {
    let _ = APP.set(app);
}

/// The app using core9000 (a placeholder before [`init`], as in tests).
pub fn app() -> &'static App {
    static UNNAMED: App = App {
        name: "core9000",
        version: env!("CARGO_PKG_VERSION"),
        env_prefix: "CORE9000",
        legacy_env_prefix: "",
        protocol: 0,
        permissions: &[],
        events_permission: "",
    };
    APP.get().copied().unwrap_or(&UNNAMED)
}

/// The app's own environment variable `<PREFIX>_<suffix>`, if set (or the
/// legacy prefix's).
pub fn env(suffix: &str) -> Option<std::ffi::OsString> {
    [app().env_prefix, app().legacy_env_prefix]
        .into_iter()
        .filter(|p| !p.is_empty())
        .find_map(|p| std::env::var_os(format!("{p}_{suffix}")).filter(|v| !v.is_empty()))
}
