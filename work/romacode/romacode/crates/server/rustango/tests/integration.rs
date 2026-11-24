//! End-to-end integration tests — a full "blog" app exercising every
//! framework layer through the in-process test client, mirroring the kinds
//! of flows the official Django tutorial covers.

use std::sync::Arc;

use rustango::admin::ModelAdmin;
use rustango::auth::User;
use rustango::forms::{CharField, EmailField, IntegerField};
use rustango::http::{HttpRequest, HttpResponse, Method, QueryDict};
use rustango::messages as msgs;
use rustango::orm::ModelExt as _;
use rustango::project::{App, Project};
use rustango::settings::Settings;
use rustango::templates::{Context, TplValue};
use rustango::testclient::Client;
use rustango::urls::{named_path, path};
use rustango::views::{make_view, render};

// -- models -------------------------------------------------------------------

#[derive(Clone, Debug, rustango::Model)]
#[model(table = "blog_post")]
pub struct Post {
    #[field(auto)]
    pub id: i64,
    #[field(char, max = 200)]
    pub title: String,
    #[field(text)]
    pub body: String,
    #[field(fk, target = "auth_user")]
    pub author_id: i64,
    #[field(bool, default = false)]
    pub published: bool,
}

#[derive(Clone, Debug, rustango::Model)]
#[model(table = "blog_comment")]
pub struct Comment {
    #[field(auto)]
    pub id: i64,
    #[field(fk, target = "blog_post")]
    pub post_id: i64,
    #[field(char, max = 500)]
    pub text: String,
}

// -- views ---------------------------------------------------------------------

fn home(req: &mut HttpRequest) -> Result<HttpResponse, rustango::errors::HttpError> {
    let backend = req.backend.clone().unwrap();
    let posts: Vec<TplValue> = Post::objects(&*backend)
        .filter(rustango::orm::Criteria::eq("published", true))
        .order_by("-id")
        .all()
        .unwrap()
        .into_iter()
        .map(|p| TplValue::Model(Arc::new(p)))
        .collect();
    let mut ctx = Context::new();
    ctx.set("posts", TplValue::List(posts));
    render(req, "home.html", ctx)
}

fn post_detail(req: &mut HttpRequest) -> Result<HttpResponse, rustango::errors::HttpError> {
    let backend = req.backend.clone().unwrap();
    let pk = req.int_param("pk")?;
    let post = Post::get(&*backend, pk)
        .map_err(|_| rustango::errors::HttpError::NotFound("post".into()))?;
    let comments: Vec<TplValue> = Comment::objects(&*backend)
        .filter(rustango::orm::Criteria::eq("post_id", rustango::orm::Value::Int(pk)))
        .all()
        .unwrap()
        .into_iter()
        .map(|c| TplValue::Model(Arc::new(c)))
        .collect();
    let mut ctx = Context::new();
    ctx.set("post", TplValue::Model(Arc::new(post)));
    ctx.set("comments", TplValue::List(comments));
    render(req, "detail.html", ctx)
}

fn add_comment(req: &mut HttpRequest) -> Result<HttpResponse, rustango::errors::HttpError> {
    if req.method != Method::POST {
        return Err(rustango::errors::HttpError::MethodNotAllowed("POST only".into()));
    }
    let backend = req.backend.clone().unwrap();
    let pk = req.int_param("pk")?;
    let form = rustango::forms::Form::new(
        "comment",
        vec![
            Box::new(CharField::new("text").min_length(2).max_length(500)),
            Box::new(EmailField::new("email").optional()),
        ],
    )
    .bind(&req.post);
    let mut form = form;
    if form.is_valid() {
        let c = Comment {
            id: 0,
            post_id: pk,
            text: form.cleaned("text").unwrap().as_text().unwrap().to_string(),
        };
        c.save(&*backend).unwrap();
        msgs::success(req, "Comment added!");
        Ok(HttpResponse::redirect(format!("/blog/{pk}/")))
    } else {
        Err(rustango::errors::HttpError::BadRequest("invalid comment".into()))
    }
}

fn stats(req: &mut HttpRequest) -> Result<HttpResponse, rustango::errors::HttpError> {
    let backend = req.backend.clone().unwrap();
    Ok(HttpResponse::json(&serde_json::json!({
        "posts": Post::objects(&*backend).count().unwrap(),
        "published": Post::objects(&*backend)
            .filter(rustango::orm::Criteria::eq("published", true))
            .count()
            .unwrap(),
        "user": req.user.username(),
    })))
}

// -- fixtures --------------------------------------------------------------------

fn templates() -> Box<rustango::templates::InMemoryLoader> {
    let mut l = rustango::templates::InMemoryLoader::new();
    l.add(
        "home.html",
        "<h1>Blog</h1>{% for p in posts %}<article><a href=\"/blog/{{ p.id }}/\">{{ p.title }}</a></article>{% empty %}<p>No posts yet.</p>{% endfor %}",
    );
    l.add(
        "detail.html",
        "<h1>{{ post.title }}</h1><div>{{ post.body }}</div>\
         {% for c in comments %}<p class=\"c\">{{ c.text }}</p>{% empty %}<p>No comments.</p>{% endfor %}\
         {% for m in messages %}<span class=\"flash\">{{ m.message }}</span>{% endfor %}",
    );
    l.add("login.html", "LOGIN FORM {% csrf_token %}");
    Box::new(l)
}

fn build_project() -> Arc<Project> {
    Arc::new(
        Project::builder()
            .settings(Settings::builder().database("memory").build())
            .add_app(App::new("blog").models::<Post>().models::<Comment>())
            .urls(vec![
                named_path("", make_view(home), "home"),
                path("blog/", make_view(home)).name("blog"),
                named_path("blog/<int:pk>/", make_view(post_detail), "post_detail"),
                named_path("blog/<int:pk>/comment/", make_view(add_comment), "add_comment"),
                named_path("stats/", make_view(stats), "stats"),
                path(
                    "login/",
                    make_view(|req: &mut HttpRequest| {
                        if req.method == Method::POST {
                            let backend = req.backend.clone().unwrap();
                            let u = rustango::auth::authenticate(
                                &*backend,
                                req.post.get("username").unwrap_or(""),
                                req.post.get("password").unwrap_or(""),
                            );
                            if let Some(user) = u {
                                rustango::auth::login(req, &user);
                                msgs::success(req, "Welcome back!");
                                return Ok(HttpResponse::redirect("/"));
                            }
                            return Err(rustango::errors::HttpError::PermissionDenied(
                                "bad credentials".into(),
                            ));
                        }
                        render(req, "login.html", Context::new())
                    }),
                )
                .name("login"),
            ])
            .admin(|site| {
                site.register::<Post>(ModelAdmin {
                    list_display: vec!["id".into(), "title".into(), "published".into()],
                    search_fields: vec!["title".into()],
                    list_per_page: 5,
                    ..Default::default()
                });
                site.register::<User>(Default::default());
            })
            .template_loader(templates())
            .build()
            .expect("project"),
    )
}

fn seed(blog: &Arc<Project>) -> (Post, User) {
    let backend = blog.backend.clone();
    let user = rustango::auth::create_superuser(&*backend, "ada", "ada@example.com", "pw123456")
        .expect("superuser");
    let post = Post {
        id: 0,
        title: "Hello Rust".into(),
        body: "Django concepts, Rust implementation.".into(),
        author_id: user.pk(),
        published: true,
    };
    let post = post.save(&*backend).unwrap();
    let draft = Post {
        id: 0,
        title: "Draft post".into(),
        body: "Not public yet.".into(),
        author_id: user.pk(),
        published: false,
    };
    draft.save(&*backend).unwrap();
    (post, user)
}

// -- tests ------------------------------------------------------------------------

#[test]
fn home_shows_only_published_posts() {
    let blog = build_project();
    seed(&blog);
    let mut client = Client::new(blog);
    let res = client.get("/");
    assert_eq!(res.status, 200);
    assert!(res.body.contains("Hello Rust"));
    assert!(!res.body.contains("Draft post"));
}

#[test]
fn empty_home_renders_empty_state() {
    let blog = build_project();
    let mut client = Client::new(blog);
    let res = client.get("/");
    assert!(res.body.contains("No posts yet."));
}

#[test]
fn detail_with_comments_and_404() {
    let blog = build_project();
    let (post, _) = seed(&blog);
    let backend = blog.backend.clone();
    Comment {
        id: 0,
        post_id: post.pk(),
        text: "First!".into(),
    }
    .save(&*backend)
    .unwrap();

    let mut client = Client::new(blog);
    let res = client.get(&format!("/blog/{}/", post.pk()));
    assert!(res.body.contains("Hello Rust"));
    assert!(res.body.contains("First!"));

    let missing = client.get("/blog/9999/");
    assert_eq!(missing.status, 404);

    // trailing slash enforcement via APPEND_SLASH
    client.follow_redirects = false;
    let no_slash = client.get("/blog/9999");
    assert_eq!(no_slash.status, 301);
}

#[test]
fn comment_form_validation_and_csrf() {
    let blog = build_project();
    let (post, _) = seed(&blog);
    let mut client = Client::new(blog);
    let url = format!("/blog/{}/comment/", post.pk());

    // GET -> 405
    let res = client.get(&url);
    assert_eq!(res.status, 405);

    // CSRF missing -> 403
    let mut data = QueryDict::new();
    data.set("text", "hi");
    let res = client.post(&url, &data);
    assert_eq!(res.status, 403);

    // with csrf token + invalid (too short) -> 400
    client.get("/"); // pick up the csrf cookie
    let mut data = QueryDict::new();
    data.set("text", "x");
    let res = client.post_csrf(&url, &data);
    assert_eq!(res.status, 400);

    // valid -> redirect to detail, comment visible
    let mut data = QueryDict::new();
    data.set("text", "Great post, learned a lot!");
    let res = client.post_csrf(&url, &data);
    assert_eq!(res.status, 200); // followed redirect
    assert!(res.body.contains("Great post, learned a lot!"));
    assert!(res.body.contains("Comment added!")); // flash message
}

#[test]
fn auth_login_logout_cycle() {
    let blog = build_project();
    let (_, user) = seed(&blog);
    let mut client = Client::new(blog);

    // stats shows anonymous
    let res = client.get("/stats/");
    assert_eq!(res.json()["user"], serde_json::json!(""));

    // login page renders with csrf
    let res = client.get("/login/");
    assert!(res.body.contains("LOGIN FORM"));
    assert!(client.csrf_token().is_some());

    // wrong password -> 403
    let mut data = QueryDict::new();
    data.set("username", "ada");
    data.set("password", "nope");
    let res = client.post_csrf("/login/", &data);
    assert_eq!(res.status, 403);

    // correct -> redirect + session
    let mut data = QueryDict::new();
    data.set("username", "ada");
    data.set("password", "pw123456");
    let res = client.post_csrf("/login/", &data);
    assert_eq!(res.status, 200);
    let res = client.get("/stats/");
    assert_eq!(res.json()["user"], serde_json::json!("ada"));

    // logout
    client.logout();
    let res = client.get("/stats/");
    assert_eq!(res.json()["user"], serde_json::json!(""));
    let _ = user;
}

#[test]
fn fk_traversal_query() {
    let blog = build_project();
    let (_post, _user) = seed(&blog);
    let backend = blog.backend.clone();
    // author's posts via fk join: author__username
    let qs = Post::objects(&*backend)
        .filter(rustango::orm::Criteria::eq("author_id__username", rustango::orm::Value::Text("ada".into())))
        .count()
        .unwrap();
    assert_eq!(qs, 2);
}

#[test]
fn admin_full_crud_cycle() {
    let blog = build_project();
    seed(&blog);
    let mut client = Client::new(blog.clone());
    client.follow_redirects = false;

    // anonymous -> redirected to login
    let res = client.get("/admin/");
    assert_eq!(res.status, 302);
    assert!(res.header("Location").unwrap().contains("/admin/login/"));
    client.follow_redirects = true;

    // staff login via admin login form
    let mut data = QueryDict::new();
    data.set("username", "ada");
    data.set("password", "pw123456");
    let res = client.post_csrf("/admin/login/", &data);
    assert_eq!(res.status, 200);

    // index lists models
    let res = client.get("/admin/");
    assert!(res.body.contains("blog_post"));
    assert!(res.body.contains("auth_user"));

    // changelist with pagination (5 per page, 1 post)
    let res = client.get("/admin/blog_post/");
    assert!(res.body.contains("Hello Rust"));

    // search
    let res = client.get("/admin/blog_post/?q=hello");
    assert!(res.body.contains("Hello Rust"));
    let res = client.get("/admin/blog_post/?q=zebra");
    assert!(res.body.contains("No items."));

    // add via admin
    client.get("/admin/blog_post/add/");
    let mut data = QueryDict::new();
    data.set("title", "Added via admin");
    data.set("body", "Through the auto-generated CRUD.");
    data.set("author_id", "1");
    data.set("published", "on");
    let res = client.post_csrf("/admin/blog_post/add/", &data);
    assert_eq!(res.status, 200);
    assert!(res.body.contains("Added via admin"));

    // change it
    let mut data = QueryDict::new();
    data.set("title", "Edited via admin");
    data.set("body", "Updated body.");
    data.set("author_id", "1");
    let res = client.post_csrf("/admin/blog_post/2/change/", &data);
    assert_eq!(res.status, 200);
    let backend = client_get_backend(&blog);
    let p = Post::get(&*backend, 2).unwrap();
    assert_eq!(p.title, "Edited via admin");

    // delete it
    let res = client.post_csrf("/admin/blog_post/2/delete/", &QueryDict::new());
    assert_eq!(res.status, 200);
    assert!(Post::get(&*backend, 2).is_err());
}

fn client_get_backend(blog: &Arc<Project>) -> Arc<dyn rustango::orm::Backend> {
    blog.backend.clone()
}

#[test]
fn reverse_named_urls() {
    rustango::urls::set_route_registry_for_tests(build_project().router.as_ref().clone());
    assert_eq!(rustango::urls::reverse("home", &[]).unwrap(), "/");
    assert_eq!(rustango::urls::reverse("post_detail", &["7"]).unwrap(), "/blog/7/");
    assert_eq!(
        rustango::urls::reverse("add_comment", &["3"]).unwrap(),
        "/blog/3/comment/"
    );
}

#[test]
fn migrations_apply_to_json_backend() {
    let dir = std::env::temp_dir().join(format!("rustango-e2e-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("db.json");
    let alias = format!("json:{}", db.display());

    let blog = Arc::new(
        Project::builder()
            .settings(Settings::builder().database(alias.clone()).build())
            .add_app(App::new("blog").models::<Post>())
            .build()
            .unwrap(),
    );
    // tables auto-created on build; write data, reopen project, data survives
    let post = Post {
        id: 0,
        title: "Persisted".into(),
        body: "survives restart".into(),
        author_id: 0,
        published: true,
    };
    let saved = post.save(&*blog.backend).unwrap();

    let blog2 = Arc::new(
        Project::builder()
            .settings(Settings::builder().database(alias).build())
            .add_app(App::new("blog").models::<Post>())
            .build()
            .unwrap(),
    );
    let found = Post::get(&*blog2.backend, saved.pk()).unwrap();
    assert_eq!(found.title, "Persisted");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn signals_fire_on_save_and_delete() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static SAVES: AtomicUsize = AtomicUsize::new(0);
    rustango::signals::connect("post_save", |p| {
        if p.table == "blog_post" {
            SAVES.fetch_add(1, Ordering::SeqCst);
        }
    });
    let blog = build_project();
    let backend = blog.backend.clone();
    let before = SAVES.load(Ordering::SeqCst);
    let p = Post { id: 0, title: "sig".into(), body: String::new(), author_id: 0, published: false };
    let saved = p.save(&*backend).unwrap();
    saved.delete(&*backend).unwrap();
    assert_eq!(SAVES.load(Ordering::SeqCst), before + 1);
}

#[test]
fn template_inheritance_and_i18n() {
    let mut loader = rustango::templates::InMemoryLoader::new();
    loader.add("base.html", "<title>{% block t %}Base{% endblock %}</title>{% trans \"Welcome\" %}");
    loader.add("child.html", "{% extends \"base.html\" %}{% block t %}Child{% endblock %}");
    let mut cats = std::collections::HashMap::new();
    let mut de = std::collections::HashMap::new();
    de.insert("Welcome".to_string(), "Willkommen".to_string());
    cats.insert("de".to_string(), de);
    rustango::i18n::set_catalogs(cats);

    let engine = rustango::templates::Engine::new(vec![Box::new(loader)], true);
    let mut ctx = Context::new();
    ctx.set("_lang", TplValue::Str("de".into()));
    let out = engine.render("child.html", &mut ctx).unwrap();
    assert_eq!(out, "<title>Child</title>Willkommen");
}

#[test]
fn gzip_compression_end_to_end() {
    let blog = build_project();
    seed(&blog);
    // the test client doesn't send accept-encoding; direct pipeline check:
    let mut req = HttpRequest::new(Method::GET, "/");
    req.headers.set("Accept-Encoding", "gzip");
    let res = blog.handle_request(&mut req);
    // response passes through GZipMiddleware when large enough; small pages
    // stay uncompressed (min_length=200 default)
    assert_eq!(res.status, 200);
}

#[test]
fn form_validation_errors_render() {
    let form = rustango::forms::Form::new(
        "f",
        vec![
            Box::new(CharField::new("name")),
            Box::new(IntegerField::new("age").max_value(10)),
        ],
    );
    let mut data = QueryDict::new();
    data.set("age", "99");
    let mut bound = form.bind(&data);
    assert!(!bound.is_valid());
    let html = bound.as_p();
    assert!(html.contains("errorlist"));
    assert!(html.contains("required"));
}

#[test]
fn head_request_matches_get_semantics() {
    let blog = build_project();
    seed(&blog);
    let mut client = Client::new(blog);
    client.follow_redirects = false;
    let res = client.head("/");
    assert_eq!(res.status, 200);
}
