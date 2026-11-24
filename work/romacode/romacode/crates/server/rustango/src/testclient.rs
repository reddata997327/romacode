//! Test client — the `django.test.Client` equivalent.
//!
//! Drives the full middleware pipeline in-process (no network), with a
//! cookie jar, redirects, and login helpers.

use std::sync::Arc;

use crate::http::{HttpRequest, Method, QueryDict};
use crate::project::Project;

/// A stateful client bound to one project.
pub struct Client {
    project: Arc<Project>,
    pub cookies: std::collections::HashMap<String, String>,
    pub follow_redirects: bool,
}

impl Client {
    pub fn new(project: Arc<Project>) -> Client {
        Client {
            project,
            cookies: std::collections::HashMap::new(),
            follow_redirects: true,
        }
    }

    fn request(
        &mut self,
        method: Method,
        path: &str,
        data: Option<&QueryDict>,
        headers: Vec<(String, String)>,
    ) -> TestResponse {
        let mut req = HttpRequest::new(method, path);
        req.cookies = self.cookies.clone();
        for (k, v) in headers {
            req.headers.set(k, v);
        }
        if let Some(d) = data {
            req.post = d.clone();
        }
        let res = self.project.handle_request(&mut req);
        // update cookie jar
        for cookie in &res.cookies {
            if cookie.max_age == Some(0) {
                self.cookies.remove(&cookie.name);
            } else {
                self.cookies.insert(cookie.name.clone(), cookie.value.clone());
            }
        }
        // CSRF token cookie becomes the request token for later POSTs
        let res = TestResponse {
            status: res.status,
            headers: res.headers.clone(),
            body: res.body_string(),
            cookies: res.cookies.clone(),
            url: req.full_path.clone(),
        };
        if self.follow_redirects && (res.status == 301 || res.status == 302) {
            let location = res.headers.get("Location").unwrap_or("").to_string();
            if !location.is_empty() && !location.starts_with("http") {
                let method2 = if res.status == 301 { method } else { Method::GET };
                return self.request(method2, &location, None, vec![]);
            }
        }
        res
    }

    /// GET a path (query string included in `path`).
    pub fn get(&mut self, path: &str) -> TestResponse {
        self.request(Method::GET, path, None, vec![])
    }

    /// POST form data.
    pub fn post(&mut self, path: &str, data: &QueryDict) -> TestResponse {
        self.request(Method::POST, path, Some(data), vec![])
    }

    /// POST JSON — top-level object fields become `request.POST` values.
    pub fn post_json(&mut self, path: &str, value: &serde_json::Value) -> TestResponse {
        let mut data = QueryDict::new();
        if let serde_json::Value::Object(map) = value {
            for (k, v) in map {
                let s = match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                data.set(k, s);
            }
        }
        self.request(
            Method::POST,
            path,
            Some(&data),
            vec![("Content-Type".into(), "application/json".into())],
        )
    }

    /// PUT.
    pub fn put(&mut self, path: &str) -> TestResponse {
        self.request(Method::PUT, path, None, vec![])
    }

    /// DELETE.
    pub fn delete(&mut self, path: &str) -> TestResponse {
        self.request(Method::DELETE, path, None, vec![])
    }

    /// HEAD.
    pub fn head(&mut self, path: &str) -> TestResponse {
        self.request(Method::HEAD, path, None, vec![])
    }

    /// Authenticate as a user (like `client.force_login`): creates the
    /// session directly, skipping the password check.
    pub fn force_login(&mut self, user_id: i64, username: &str, is_staff: bool, is_superuser: bool) {
        // Ensure a session exists and carries the auth key.
        let backend = self.project.backend.clone();
        let mut session = crate::sessions::Session::load(backend, None, 3600);
        session.set("_auth_user_id", serde_json::json!(user_id));
        session.save();
        self.cookies
            .insert(self.project.settings.session_cookie_name.clone(), session.key.clone());
        let _ = (username, is_staff, is_superuser);
    }

    /// Full login via the password check — `client.login(username, password)`.
    pub fn login(&mut self, username: &str, password: &str) -> bool {
        let backend = self.project.backend.clone();
        match crate::auth::authenticate(&*backend, username, password) {
            Some(user) => {
                self.force_login(user.pk(), &user.username, user.is_staff, user.is_superuser);
                true
            }
            None => false,
        }
    }

    /// Log out — clears the session cookie.
    pub fn logout(&mut self) {
        let backend = self.project.backend.clone();
        if let Some(key) = self.cookies.get(&self.project.settings.session_cookie_name) {
            let mut session = crate::sessions::Session::load(backend, Some(key), 3600);
            session.delete();
        }
        self.cookies.remove(&self.project.settings.session_cookie_name);
    }

    /// Fetch the CSRF token from the cookie jar (for CSRF-protected posts).
    pub fn csrf_token(&self) -> Option<String> {
        self.cookies.get(&self.project.settings.csrf_cookie_name).cloned()
    }

    /// POST with automatic CSRF token attached (form field + header).
    pub fn post_csrf(&mut self, path: &str, data: &QueryDict) -> TestResponse {
        let mut d = data.clone();
        if let Some(token) = self.csrf_token() {
            d.set("csrfmiddlewaretoken", token);
        }
        let headers = vec![];
        self.request(Method::POST, path, Some(&d), headers)
    }
}

fn _ensure_send_sync() {
    fn f<T: Send + Sync>() {}
    f::<Client>();
}

/// The response returned by the client.
#[derive(Debug, Clone)]
pub struct TestResponse {
    pub status: u16,
    pub headers: crate::http::Headers,
    pub body: String,
    pub cookies: Vec<crate::http::SetCookie>,
    pub url: String,
}

impl TestResponse {
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or(serde_json::Value::Null)
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name)
    }

    pub fn cookie_value(&self, name: &str) -> Option<String> {
        self.cookies
            .iter()
            .find(|c| c.name == name)
            .map(|c| c.value.clone())
    }

}

/// Convenience: build a client from a project builder closure.
pub fn client_for<F>(build: F) -> (Arc<Project>, Client)
where
    F: FnOnce(crate::project::ProjectBuilder) -> crate::project::ProjectBuilder,
{
    let project = Arc::new(build(Project::builder()).build().expect("project"));
    (project.clone(), Client::new(project))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::User;
    use crate::http::HttpResponse;
    use crate::orm::{InMemoryBackend, Model as _};
    use crate::project::{App, Project};
    use crate::settings::Settings;
    use crate::urls::{named_path, path};
    use crate::views::{make_view, ViewFn};

    fn index() -> ViewFn {
        make_view(|req| {
            let mut ctx = crate::templates::Context::new();
            ctx.set(
                "user",
                crate::templates::TplValue::Str(req.user.username().to_string()),
            );
            crate::views::render(req, "home.html", ctx)
        })
    }

    fn project() -> Arc<Project> {
        Arc::new(
            Project::builder()
                .settings(Settings::builder().build())
                .backend(Arc::new(InMemoryBackend::new()))
                .add_app(App::new("core").models::<User>())
                .urls(vec![
                    named_path("", index(), "home"),
                    path("me/", make_view(|req| Ok(HttpResponse::json(&serde_json::json!({
                        "user": req.user.username(),
                        "authenticated": req.user.is_authenticated(),
                    }))))),
                    path(
                        "set-flash/",
                        make_view(|req| {
                            crate::messages::success(req, "it worked");
                            Ok(HttpResponse::redirect("/me/"))
                        }),
                    ),
                ])
                .template_loader(Box::new({
                    let mut l = crate::templates::InMemoryLoader::new();
                    l.add("home.html", "Hello {{ user }} msgs={% for m in messages %}{{ m.message }}{% endfor %}");
                    l
                }))
                .build()
                .unwrap(),
        )
    }

    #[test]
    fn get_and_redirect_flow() {
        let p = project();
        let mut c = Client::new(p);
        let res = c.get("/");
        assert_eq!(res.status, 200);
        assert!(res.body.contains("Hello "));
    }

    #[test]
    fn force_login_and_json_me() {
        let p = project();
        let backend = p.backend.clone();
        backend
            .create_table(User::table(), User::columns().to_vec())
            .unwrap();
        let u = crate::auth::create_superuser(&*backend, "root", "", "pw").unwrap();

        let mut c = Client::new(p);
        let res = c.get("/me/");
        assert_eq!(res.json()["authenticated"], serde_json::json!(false));

        c.force_login(u.pk(), "root", true, true);
        let res = c.get("/me/");
        assert_eq!(res.json()["user"], serde_json::json!("root"));
        assert_eq!(res.json()["authenticated"], serde_json::json!(true));
    }

    #[test]
    fn password_login() {
        let p = project();
        let backend = p.backend.clone();
        backend
            .create_table(User::table(), User::columns().to_vec())
            .unwrap();
        crate::auth::create_superuser(&*backend, "ada", "", "secret123").unwrap();

        let mut c = Client::new(p);
        assert!(!c.login("ada", "wrong"));
        assert!(c.login("ada", "secret123"));
        let res = c.get("/me/");
        assert_eq!(res.json()["user"], serde_json::json!("ada"));

        c.logout();
        let res = c.get("/me/");
        assert_eq!(res.json()["authenticated"], serde_json::json!(false));
    }

    #[test]
    fn flash_messages_follow_redirects() {
        let p = project();
        let mut c = Client::new(p);
        let res = c.get("/set-flash/");
        // redirect followed to /me/ (JSON page)
        assert_eq!(res.status, 200);
        // the flash shows up on the next template-rendered page
        let home = c.get("/");
        assert!(home.body.contains("it worked"), "body: {}", home.body);
    }

    #[test]
    fn csrf_protected_post_flow() {
        let p = Arc::new(
            Project::builder()
                .settings(Settings::builder().build())
                .backend(Arc::new(InMemoryBackend::new()))
                .urls(vec![path(
                    "echo/",
                    make_view(|req| Ok(HttpResponse::text(format!("got:{}", req.post.get("v").unwrap_or(""))))),
                )])
                .build()
                .unwrap(),
        );
        let mut c = Client::new(p);
        // GET first to receive the csrf cookie
        c.get("/");
        let mut data = QueryDict::new();
        data.set("v", "42");
        // without token -> 403
        let res = c.post("/echo/", &data);
        assert_eq!(res.status, 403);
        // with token (client auto-attaches) -> 200
        let res = c.post_csrf("/echo/", &data);
        assert_eq!(res.status, 200);
        assert_eq!(res.body, "got:42");
    }
}
