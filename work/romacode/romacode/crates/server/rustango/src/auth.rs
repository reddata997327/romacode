//! Authentication — `django.contrib.auth`.
//!
//! Passwords use PBKDF2-SHA256 in Django's exact wire format
//! (`pbkdf2_sha256$<iterations>$<salt>$<base64 hash>`), so hashes produced
//! here verify against Django-generated ones and vice versa. Sessions carry
//! the user id (`_auth_user_id`), rotated on login to prevent fixation.

use std::sync::Arc;

use base64::Engine as _;
use sha2::Sha256;

use crate::http::{AuthUser, HttpRequest, HttpResponse, SetCookie};
use crate::orm::{Backend, Model, ModelExt};

pub use crate::orm::model::User;

const AUTH_SESSION_KEY: &str = "_auth_user_id";

// ---------------------------------------------------------------------------
// Password hashing (Django-compatible PBKDF2-SHA256)
// ---------------------------------------------------------------------------

/// `make_password("pw")` -> `pbkdf2_sha256$600000$<salt>$<hash>`.
pub fn make_password(password: &str) -> String {
    let cfg = &crate::settings::global().password_hasher;
    pbkdf2_encode(password, &random_salt(12), cfg.iterations, cfg.algorithm)
}

/// Same, with an explicit iteration count.
pub fn make_password_with_iterations(password: &str, iterations: u32) -> String {
    pbkdf2_encode(password, &random_salt(12), iterations, "pbkdf2_sha256")
}

/// Django-style alphanumeric salt (`get_random_string(12)` equivalent).
fn random_salt(len: usize) -> String {
    const ALPHA: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut bytes = Vec::with_capacity(len);
    while bytes.len() < len {
        bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    }
    bytes[..len]
        .iter()
        .map(|b| ALPHA[(*b % ALPHA.len() as u8) as usize] as char)
        .collect()
}

/// Encode in Django's exact wire format: the digest is **base64**, not hex
/// (`django.contrib.auth.hashers.PBKDF2PasswordHasher.encode`).
fn pbkdf2_encode(password: &str, salt: &str, iterations: u32, algorithm: &str) -> String {
    let mut out = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), salt.as_bytes(), iterations, &mut out);
    let digest = base64::engine::general_purpose::STANDARD.encode(out);
    format!("{algorithm}${iterations}${salt}${digest}")
}

/// `check_password("pw", stored)` — supports Django's
/// `pbkdf2_sha256$<iterations>$<salt>$<b64hash>` hashes (base64 digest).
pub fn check_password(password: &str, encoded: &str) -> bool {
    let parts: Vec<&str> = encoded.split('$').collect();
    if parts.len() != 4 || parts[0] != "pbkdf2_sha256" {
        return false;
    }
    let iterations: u32 = match parts[1].parse() {
        Ok(i) => i,
        Err(_) => return false,
    };
    let salt = parts[2];
    let expected = parts[3];
    let mut out = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), salt.as_bytes(), iterations, &mut out);
    let computed = base64::engine::general_purpose::STANDARD.encode(out);
    const_time_eq(computed.as_bytes(), expected.as_bytes())
}

fn const_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ---------------------------------------------------------------------------
// Authenticate / login / logout
// ---------------------------------------------------------------------------

/// `authenticate(username=..., password=...)` — verifies against the
/// `auth_user` table.
pub fn authenticate(backend: &dyn Backend, username: &str, password: &str) -> Option<User> {
    let crit = crate::orm::Criteria::eq("username", crate::orm::Value::Text(username.to_string()));
    let rows = backend.select("auth_user", &crit, &[], None, None).ok()?;
    let user = rows.first().map(|r| User::from_row(r))?;
    if !user.is_active {
        return None;
    }
    if check_password(password, &user.password) {
        Some(user)
    } else {
        None
    }
}

/// Log a user in: rotate the session key (fixation defense) and store
/// `_auth_user_id` in the session.
pub fn login(req: &mut HttpRequest, user: &User) {
    let session = req.session.as_mut().expect("session middleware required for login");
    session.cycle_key();
    session.set(AUTH_SESSION_KEY, serde_json::json!(user.pk()));
    session.save();
    req.user = AuthUser::Authenticated {
        id: user.pk(),
        username: user.username.clone(),
        is_staff: user.is_staff,
        is_superuser: user.is_superuser,
    };
}

/// Log out: wipe the auth data and delete the session row.
pub fn logout(req: &mut HttpRequest) {
    if let Some(session) = req.session.as_mut() {
        session.remove(AUTH_SESSION_KEY);
        session.delete();
    }
    req.user = AuthUser::Anonymous;
}

/// Load the request user from the session (`AuthMiddleware` calls this).
pub fn get_user(req: &mut HttpRequest) {
    let Some(session) = req.session.as_ref() else {
        return;
    };
    let Some(user_id) = session.get_int(AUTH_SESSION_KEY) else {
        return;
    };
    let backend = req.backend.clone();
    if let Some(backend) = backend {
        if let Ok(user) = User::get(&*backend, user_id) {
            if user.is_active {
                req.user = AuthUser::Authenticated {
                    id: user.pk(),
                    username: user.username.clone(),
                    is_staff: user.is_staff,
                    is_superuser: user.is_superuser,
                };
            }
        }
    }
}

/// Attach a `sessionid` Set-Cookie when the session was persisted during
/// this request (created, rotated, or re-saved) — like Django, which only
/// sends the cookie when the session data actually changed.
pub fn apply_session_cookie(req: &HttpRequest, response: &mut HttpResponse, cookie_name: &str, max_age: i64) {
    if let Some(session) = req.session.as_ref() {
        if session.written {
            let cookie = SetCookie::new(cookie_name, session.key.clone())
                .max_age(max_age)
                .http_only()
                .same_site("Lax");
            response.set_cookie(cookie);
        }
    }
}

/// `LoginRequired` view wrapper — like `@login_required`.
pub fn login_required(view: crate::views::ViewFn, login_url: impl Into<String>) -> crate::views::ViewFn {
    let login_url: Arc<String> = Arc::new(login_url.into());
    Arc::new(move |req: &mut HttpRequest| {
        if !req.user.is_authenticated() {
            let next = crate::http::urlencode(&req.full_path);
            return Ok(HttpResponse::redirect(format!("{}/?next={}", login_url.trim_end_matches('/'), next)));
        }
        view(req)
    })
}

/// Staff-only wrapper — like `@staff_member_required`.
pub fn staff_required(view: crate::views::ViewFn, login_url: impl Into<String>) -> crate::views::ViewFn {
    let login_url: Arc<String> = Arc::new(login_url.into());
    Arc::new(move |req: &mut HttpRequest| {
        if !req.user.is_staff() && !req.user.is_superuser() {
            return Ok(HttpResponse::redirect(login_url.trim_end_matches('/').to_string() + "/"));
        }
        view(req)
    })
}

// ---------------------------------------------------------------------------
// createsuperuser helper
// ---------------------------------------------------------------------------

/// Create (or reset the password of) a superuser — the
/// `createsuperuser` management command's core.
pub fn create_superuser(backend: &dyn Backend, username: &str, email: &str, password: &str) -> crate::orm::OrmResult<User> {
    let existing = User::objects(backend)
        .filter(crate::orm::Criteria::eq(
            "username",
            crate::orm::Value::Text(username.to_string()),
        ))
        .first()?;
    let encoded = make_password(password);
    match existing {
        Some(mut u) => {
            u.password = encoded;
            u.email = email.to_string();
            u.is_staff = true;
            u.is_superuser = true;
            u.is_active = true;
            u.save(backend)
        }
        None => {
            let u = User {
                id: 0,
                username: username.to_string(),
                email: email.to_string(),
                password: encoded,
                first_name: String::new(),
                last_name: String::new(),
                is_staff: true,
                is_superuser: true,
                is_active: true,
                date_joined: chrono::Utc::now(),
                last_login: None,
            };
            u.save(backend)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orm::InMemoryBackend;
    use crate::sessions::Session;
    use std::sync::Arc;

    fn backend_with_user(password: &str) -> Arc<InMemoryBackend> {
        let backend = Arc::new(InMemoryBackend::new());
        backend.create_table("auth_user", User::columns().to_vec()).unwrap();
        let user = User {
            id: 0,
            username: "ada".into(),
            email: "ada@example.com".into(),
            password: make_password_with_iterations(password, 1000),
            first_name: "Ada".into(),
            last_name: "L".into(),
            is_staff: true,
            is_superuser: true,
            is_active: true,
            date_joined: chrono::Utc::now(),
            last_login: None,
        };
        user.save(&*backend).unwrap();
        backend
    }

    #[test]
    fn password_format_is_django_wire_format() {
        let encoded = make_password_with_iterations("hunter2", 600_000);
        let parts: Vec<&str> = encoded.split('$').collect();
        assert_eq!(parts[0], "pbkdf2_sha256");
        assert_eq!(parts[1], "600000");
        assert_eq!(parts[2].len(), 12); // Django-style alphanumeric salt
        assert!(parts[2].chars().all(|c| c.is_ascii_alphanumeric()));
        assert_eq!(parts[3].len(), 44); // base64(32-byte sha256) incl. padding
        assert!(parts[3].ends_with('='));
    }

    #[test]
    fn check_roundtrip() {
        let encoded = make_password("s3cret");
        assert!(check_password("s3cret", &encoded));
        assert!(!check_password("wrong", &encoded));
    }

    #[test]
    fn django_generated_hash_verification() {
        // Real Django wire format: produced by
        //   hashlib.pbkdf2_hmac('sha256', b'rustango', b'cd3f9dfb48af6bb6', 1000)
        // + base64.b64encode — byte-identical to Django's
        //   make_password() output for these parameters.
        let django_hash =
            "pbkdf2_sha256$1000$cd3f9dfb48af6bb6$neYS372AH6ACJOHwTgouFTY3cyAeE49gvPo0H9szp2Q=";
        assert!(check_password("rustango", django_hash));
        assert!(!check_password("wrong", django_hash));

        // Our own encoder reproduces the same wire format.
        let ours = pbkdf2_encode("rustango", "cd3f9dfb48af6bb6", 1000, "pbkdf2_sha256");
        assert_eq!(ours, django_hash);
    }

    #[test]
    fn authenticate_success_and_failure() {
        let backend = backend_with_user("correct horse");
        assert!(authenticate(&*backend, "ada", "correct horse").is_some());
        assert!(authenticate(&*backend, "ada", "wrong").is_none());
        assert!(authenticate(&*backend, "nobody", "x").is_none());
    }

    #[test]
    fn inactive_users_cannot_authenticate() {
        let backend = Arc::new(InMemoryBackend::new());
        backend.create_table("auth_user", User::columns().to_vec()).unwrap();
        let user = User {
            id: 0,
            username: "bob".into(),
            email: String::new(),
            password: make_password_with_iterations("pw", 1000),
            first_name: String::new(),
            last_name: String::new(),
            is_staff: false,
            is_superuser: false,
            is_active: false,
            date_joined: chrono::Utc::now(),
            last_login: None,
        };
        user.save(&*backend).unwrap();
        assert!(authenticate(&*backend, "bob", "pw").is_none());
    }

    #[test]
    fn login_logout_flow() {
        let backend = backend_with_user("pw");
        let user = authenticate(&*backend, "ada", "pw").unwrap();

        let mut req = crate::http::HttpRequest::new(crate::http::Method::POST, "/login/");
        req.backend = Some(backend.clone());
        req.session = Some(Session::load(backend.clone(), None, 3600));
        assert!(req.user.is_anonymous());

        login(&mut req, &user);
        assert!(req.user.is_authenticated());
        assert_eq!(req.user.username(), "ada");
        assert!(req.user.is_staff());

        // session persists across a "new request" with the same cookie
        let key = req.session.as_ref().unwrap().key.clone();
        let mut req2 = crate::http::HttpRequest::new(crate::http::Method::GET, "/");
        req2.backend = Some(backend.clone());
        req2.session = Some(Session::load(backend.clone(), Some(&key), 3600));
        get_user(&mut req2);
        assert!(req2.user.is_authenticated());
        assert_eq!(req2.user.username(), "ada");

        logout(&mut req2);
        assert!(req2.user.is_anonymous());
    }

    #[test]
    fn login_required_wrapper() {
        let inner = crate::views::make_view(|_req| Ok(crate::http::HttpResponse::text("secret")));
        let guarded = login_required(inner, "/login/");
        let mut anon = crate::http::HttpRequest::new(crate::http::Method::GET, "/profile/");
        let res = guarded(&mut anon).unwrap();
        assert_eq!(res.status, 302);
        assert!(res.headers.get("Location").unwrap().contains("/login/"));

        let mut authed = crate::http::HttpRequest::new(crate::http::Method::GET, "/profile/");
        authed.user = AuthUser::Authenticated {
            id: 1,
            username: "ada".into(),
            is_staff: false,
            is_superuser: false,
        };
        let res = guarded(&mut authed).unwrap();
        assert_eq!(res.status, 200);
        assert_eq!(res.body_string(), "secret");
    }

    #[test]
    fn create_superuser_upserts() {
        let backend = Arc::new(InMemoryBackend::new());
        backend.create_table("auth_user", User::columns().to_vec()).unwrap();
        let u = create_superuser(&*backend, "root", "root@x.io", "pw").unwrap();
        assert!(u.is_superuser);
        // upsert path
        let u2 = create_superuser(&*backend, "root", "new@x.io", "pw2").unwrap();
        assert_eq!(u2.email, "new@x.io");
        assert_eq!(User::objects(&*backend).count().unwrap(), 1);
        assert!(check_password("pw2", &u2.password));
    }
}
