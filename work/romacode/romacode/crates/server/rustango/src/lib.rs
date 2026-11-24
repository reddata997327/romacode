//! # rustango — Django, rewritten in pure Rust
//!
//! `rustango` is a batteries-included, Django-style web framework written
//! entirely in Rust with **zero C/C++ dependencies** (the HTTP edge uses
//! tokio/axum; the crypto stack uses RustCrypto; TLS for `contrib.ai` uses
//! rustls + ring).
//!
//! The framework core is synchronous (WSGI-style, thread-per-request at the
//! edge), mirroring Django's request/response model:
//!
//! ```text
//! request -> middleware.request -> url router -> view
//!         <- middleware.response <- template/forms/orm <- view
//! ```
//!
//! ## Module map (Django -> rustango)
//!
//! | Django                  | rustango                        |
//! |-------------------------|---------------------------------|
//! | `django.conf.settings`  | [`settings::Settings`]          |
//! | `django.http`           | [`http`]                        |
//! | `django.urls`           | [`urls`] (`path()`, `reverse()`)|
//! | `django.views`          | [`views`] (+ class-based views) |
//! | `django.middleware`     | [`middleware`]                  |
//! | `django.db.models`      | [`orm`] (`#[derive(Model)]`)    |
//! | `django.template`       | [`templates`]                   |
//! | `django.forms`          | [`forms`]                       |
//! | `django.contrib.auth`   | [`auth`]                        |
//! | `django.contrib.sessions`| [`sessions`]                   |
//! | `django.middleware.csrf`| [`csrf`]                        |
//! | `django.contrib.messages`| [`messages`]                   |
//! | `django.contrib.admin`  | [`admin`]                       |
//! | `django.core.signals`   | [`signals`]                     |
//! | `django.utils.translation` | [`i18n`]                     |
//! | `django.core.cache`     | [`cache`]                       |
//! | `django.contrib.staticfiles` | [`staticfiles`]            |
//! | `django.core.management`| [`management`] + `rustango` CLI |
//! | (new) LLM/agent support | [`contrib::ai`]                 |

// Allow the proc-macros to emit `::rustango::...` paths even when used
// inside this crate.
extern crate self as rustango;

pub mod admin;
pub mod auth;
pub mod cache;
pub mod contrib;
pub mod csrf;
pub mod errors;
pub mod forms;
pub mod http;
pub mod i18n;
pub mod management;
pub mod messages;
pub mod middleware;
pub mod orm;
pub mod project;
pub mod server;
pub mod sessions;
pub mod settings;
pub mod signals;
pub mod staticfiles;
pub mod templates;
pub mod testclient;
pub mod urls;
pub mod views;

/// Shared type re-exports so user crates don't need `chrono` directly.
pub mod types {
    pub use chrono::Utc;
    /// UTC timestamp used by all `DateTime` model fields.
    pub type DateTime = chrono::DateTime<chrono::Utc>;
    pub type DateTimeTz = chrono::DateTime<chrono::Utc>;
}

/// Re-exported derive macro: `#[derive(Model)] #[model(table = "...")]`.
pub use rustango_macros::Model;

/// Everything a typical app needs, in one `use rustango::prelude::*;`.
pub mod prelude {
    // config + errors
    pub use crate::errors::{HttpError, RustangoError};
    pub use crate::settings::Settings;
    // http
    pub use crate::http::{AuthUser, HttpRequest, HttpResponse, Method, QueryDict};
    // routing + views
    pub use crate::urls::{named_path, path, reverse, reverse_with, Router};
    pub use crate::views::{make_view, redirect, render, View, ViewFn};
    // ORM
    pub use crate::orm::{Criteria, Model, ModelExt, Op, Q, QuerySet, Value};
    // templates
    pub use crate::templates::{Context, TplValue};
    // projects
    pub use crate::project::{App, Project, ProjectBuilder};
    // contrib-style helpers
    pub use crate::auth::{authenticate, check_password, login, logout, make_password};
    pub use crate::messages;
    pub use crate::signals;
}

/// Framework version, mirrors the Django release this API surface tracks.
pub const VERSION: &str = "6.2.0.dev0";
