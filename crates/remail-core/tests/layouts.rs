//! Branded layouts through the engine: saving rules, which draft gets which
//! layout, and the AI's design task against a mock OpenAI-compatible server.

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use remail_core::ai::AiSettings;
use remail_core::engine::AiTask;
use remail_core::layout::{self, MAX_LAYOUTS};
use remail_core::model::*;
use remail_core::secrets::MemoryStore;
use remail_core::store::Store;
use remail_core::*;
use serde_json::{json, Value};

struct Quiet;

impl EventSink for Quiet {
    fn emit(&self, _: EngineEvent) {}
}

fn engine() -> Engine {
    Engine::with_store(Store::open_in_memory().unwrap(), Arc::new(MemoryStore::default()), Arc::new(Quiet)).unwrap()
}

fn account(e: &Engine, email: &str) -> Account {
    // A closed local port: nothing touches the network.
    let srv = ServerConfig { host: "127.0.0.1".into(), port: 1, security: Security::None };
    let new = NewAccount { email: email.into(), display_name: "Me".into(), username: String::new(), password: String::new(), imap: srv.clone(), smtp: srv, allow_invalid_certs: false };
    e.store().insert_account(&new, false, "password").unwrap()
}

/// 1x1 transparent PNG.
const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

fn layout_for(account_id: Option<&str>, name: &str) -> BrandedLayout {
    BrandedLayout {
        id: String::new(),
        account_id: account_id.map(String::from),
        name: name.into(),
        org_name: "Acme Ltd".into(),
        about: "Family bakers since 1990.".into(),
        button: Some(LayoutButton { label: "Visit our shop".into(), url: "https://acme.test/shop".into() }),
        logo: Some(LayoutLogo { data: PNG.into(), mime: "image/png".into(), width: 120, height: 40, colors: vec!["#d2691e".into()], ..Default::default() }),
        sender_name: "Dana Lee".into(),
        position: "Head of Sales".into(),
        phone: "+1 555 010 2030".into(),
        email: "dana@acme.test".into(),
        style: LayoutStyle::default(),
        use_on_replies: false,
        is_default: false,
        updated_at: 0,
    }
}

#[tokio::test]
async fn saving_follows_the_rules() {
    let e = engine();
    // The colours are made readable on the way in.
    let mut l = layout_for(None, "Pale");
    l.style.accent = "#ffe066".into();
    let saved = e.save_layout(l).unwrap();
    assert!(saved.id.starts_with("lay_"));
    assert_ne!(saved.style.accent, "#ffe066");
    assert_eq!(e.layouts().unwrap().len(), 1);

    let bad_email = BrandedLayout { email: "dana at acme".into(), ..layout_for(None, "Bad") };
    assert!(e.save_layout(bad_email).unwrap_err().to_string().contains("email"));
    let svg = BrandedLayout { logo: Some(LayoutLogo { mime: "image/svg+xml".into(), ..layout_for(None, "").logo.unwrap() }), ..layout_for(None, "Svg") };
    assert!(e.save_layout(svg).is_err());
    let ghost = layout_for(Some("acc_nope"), "Ghost");
    assert!(e.save_layout(ghost).is_err());

    // Up to six layouts.
    for i in 1..MAX_LAYOUTS {
        e.save_layout(layout_for(None, &format!("L{i}"))).unwrap();
    }
    let err = e.save_layout(layout_for(None, "One too many")).unwrap_err().to_string();
    assert!(err.contains("up to 6"), "{err}");
    // Editing one of them still works at the limit.
    let mut first = e.layouts().unwrap().remove(0);
    first.position = "Director".into();
    e.save_layout(first.clone()).unwrap();
    assert_eq!(e.layouts().unwrap().iter().find(|l| l.id == first.id).unwrap().position, "Director");
    e.delete_layout(&first.id).unwrap();
    assert_eq!(e.layouts().unwrap().len(), MAX_LAYOUTS - 1);
}

#[tokio::test]
async fn the_brief_and_the_button_are_tidied_and_checked_on_save() {
    let e = engine();
    // Stray spaces and line breaks in the brief; the button's fields trimmed.
    let mut l = layout_for(None, "Brief");
    l.about = "  Family bakers\n since 1990.   ".into();
    l.button = Some(LayoutButton { label: "  Visit  ".into(), url: " https://acme.test/shop ".into() });
    let saved = e.save_layout(l).unwrap();
    assert_eq!(saved.about, "Family bakers since 1990.");
    assert_eq!(saved.button, Some(LayoutButton { label: "Visit".into(), url: "https://acme.test/shop".into() }));
    // A button with nothing in it is no button.
    let mut l = layout_for(None, "Empty button");
    l.button = Some(LayoutButton { label: " ".into(), url: "".into() });
    assert_eq!(e.save_layout(l).unwrap().button, None);
    // Half a button, or a link that isn't a page or an address, is refused with a reason.
    for (label, url, word) in [("Go", "", "text and a link"), ("", "https://acme.test", "text and a link"), ("Go", "http://acme.test", "https"), ("Go", "javascript:alert(1)", "https"), ("Go", "data:text/html,<script>", "https")] {
        let mut l = layout_for(None, "Bad button");
        l.button = Some(LayoutButton { label: label.into(), url: url.into() });
        let err = e.save_layout(l).unwrap_err().to_string();
        assert!(err.contains(word), "{label:?} {url:?}: {err}");
    }
    // The brief has a limit.
    let mut l = layout_for(None, "Long");
    l.about = "x".repeat(layout::MAX_ABOUT_CHARS + 1);
    assert!(e.save_layout(l).unwrap_err().to_string().contains("description"));
    // The saved layout comes back whole, and its templates are all there to choose from.
    assert_eq!(e.layouts().unwrap().len(), 2);
    for id in layout::PRESETS {
        assert_eq!(e.layout_preset(id, &[]).template.as_deref(), Some(id));
    }
}

#[tokio::test]
async fn new_mail_takes_the_default_layout_instead_of_the_signature() {
    let e = engine();
    let a = account(&e, "me@example.com");
    let b = account(&e, "other@example.com");
    e.save_signature(Signature { id: String::new(), account_id: None, name: "Sig".into(), html: "<b>Me</b>".into(), is_default: true }).unwrap();
    // No layout yet: the signature as always.
    let d = e.new_draft(Some(&a.id)).unwrap();
    assert!(d.html_body.contains("<b>Me</b>") && d.layout_id.is_none() && d.signature_id.is_some());

    let mut l = layout_for(Some(&a.id), "Work");
    l.is_default = true;
    let l = e.save_layout(l).unwrap();
    let d = e.new_draft(Some(&a.id)).unwrap();
    assert_eq!(d.layout_id.as_deref(), Some(l.id.as_str()));
    assert!(d.signature_id.is_none() && !d.html_body.contains("Me</b>"), "the footer is the signature: {}", d.html_body);
    // Another account has no layout of its own: its signature stays.
    let d = e.new_draft(Some(&b.id)).unwrap();
    assert!(d.layout_id.is_none() && d.html_body.contains("<b>Me</b>"));
    // A mailto: link opens with it too.
    let d = e.mailto_draft("mailto:x@y.test?subject=Hi", Some(&a.id)).unwrap();
    assert_eq!(d.layout_id.as_deref(), Some(l.id.as_str()));
}

#[derive(Clone, Default)]
struct Llm {
    requests: Arc<Mutex<Vec<Value>>>,
    replies: Arc<Mutex<std::collections::VecDeque<String>>>,
}

async fn complete(State(m): State<Llm>, Json(body): Json<Value>) -> Response {
    m.requests.lock().unwrap().push(body);
    let reply = m.replies.lock().unwrap().pop_front().unwrap_or_default();
    let sse = format!("data: {}\n\ndata: [DONE]\n\n", json!({"choices":[{"delta":{"content":reply},"finish_reason":"stop"}]}));
    ([("content-type", "text/event-stream")], sse).into_response()
}

async fn mock_llm(replies: &[&str]) -> (String, Llm) {
    let llm = Llm { replies: Arc::new(Mutex::new(replies.iter().map(|s| s.to_string()).collect())), ..Default::default() };
    let app = Router::new().route("/v1/chat/completions", post(complete)).with_state(llm.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://127.0.0.1:{}/v1", listener.local_addr().unwrap().port());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, llm)
}

async fn design(e: &Engine, l: &BrandedLayout, variation: u32) -> anyhow::Result<LayoutStyle> {
    let task = AiTask::Layout { layout: Box::new(l.clone()), variation, instructions: String::new() };
    e.ai_run("r", task, &mut |_| {}).await.map(|o| o.style.expect("a design"))
}

#[tokio::test]
async fn the_ai_designs_and_redesigns() {
    let first = r##"{"accent":"#0a7d4f","header_bg":"#f2faf6","text_color":"#102a1f","muted_color":"#4a5d55","alignment":"center","divider":"bar","logo_position":"above","footer_align":"center","footer_logo":"above","button_style":"outline","tagline":"Fresh since 1990"}"##;
    let second = "```json\n{\"accent\":\"#7a1f8f\",\"header_bg\":\"#ffffff\",\"text_color\":\"#2b1033\",\"muted_color\":\"#5c4a63\",\"alignment\":\"left\",\"divider\":\"line\",\"logo_position\":\"left\",\"footer_align\":\"right\",\"footer_logo\":\"none\",\"button_style\":\"filled\",\"tagline\":null}\n```";
    let (url, llm) = mock_llm(&[first, second, "Sorry, I can't pick colours."]).await;
    let e = engine();
    e.set_ai_settings(&AiSettings { enabled: true, provider: "openai".into(), base_url: url, model: "mock".into(), ..Default::default() }, Some("sk-test")).unwrap();
    let l = layout_for(None, "Work");

    let a = design(&e, &l, 0).await.unwrap();
    assert_eq!((a.accent.as_str(), a.alignment, a.divider, a.logo_position), ("#0a7d4f", LayoutAlign::Center, LayoutDivider::Bar, LogoPosition::Above));
    assert_eq!(a.tagline.as_deref(), Some("Fresh since 1990"));
    // Regenerate: the same details, a different design.
    let b = design(&e, &l, 1).await.unwrap();
    assert_ne!(a, b);
    assert_eq!((b.footer_align, b.footer_logo, b.button_style, b.tagline), (FooterAlign::Right, FooterLogo::None, ButtonStyle::Filled, None));
    // A reply with no design in it gives the plain suggestion for that variation, not an error.
    let c = design(&e, &l, 2).await.unwrap();
    assert_eq!(c, layout::suggest_style(&["#d2691e".into()], 2));

    let reqs = llm.requests.lock().unwrap();
    let text = |i: usize| reqs[i]["messages"].as_array().unwrap().iter().map(|m| m["content"].as_str().unwrap_or("")).collect::<Vec<_>>().join("\n");
    assert!(text(0).contains("About it: Family bakers since 1990.") && text(0).contains("Has a header button: yes"), "{}", text(0));
    assert!(text(0).contains("Organisation: Acme Ltd") && text(0).contains("Logo colours: #d2691e") && text(0).contains("Request number: 1"), "{}", text(0));
    assert!(text(1).contains("Request number: 2") && text(1).contains("different design"));
    // The picture itself never leaves the device, and the model is told it only chooses a design.
    for r in reqs.iter() {
        assert!(!r.to_string().contains(PNG));
    }
    assert!(text(0).contains("never write HTML"));
}

#[tokio::test]
async fn without_ai_generate_still_designs() {
    let e = engine();
    // Nothing set up: the AI task refuses, the built-in designs work and differ.
    let l = layout_for(None, "Work");
    assert!(design(&e, &l, 0).await.is_err());
    let colors = vec!["#d2691e".to_string()];
    let designs: Vec<LayoutStyle> = (0..6).map(|v| e.layout_suggest(&colors, v)).collect();
    for i in 0..designs.len() {
        for j in i + 1..designs.len() {
            assert_ne!(designs[i], designs[j]);
        }
    }
    // And each one renders into a layout that saves.
    for (v, s) in designs.into_iter().enumerate() {
        let saved = e.save_layout(BrandedLayout { style: s, ..layout_for(None, &format!("Design {v}")) }).unwrap();
        let p = e.layout_preview(&saved, "<p>Hello</p>");
        assert!(p.full.contains("<p>Hello</p>") && p.full.contains(&p.header) && p.full.contains(&p.footer));
        assert!(p.header.contains("Acme Ltd") && p.footer.contains("Dana Lee") && p.adjusted.is_empty(), "{v}: {:?}", p.adjusted);
    }
}

#[tokio::test]
async fn six_largest_logos_still_fit_the_synced_section() {
    use base64::Engine as _;
    let e = engine();
    // A PNG header plus filler up to the size limit.
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.resize(layout::MAX_LOGO_BYTES, 7);
    let big = base64::engine::general_purpose::STANDARD.encode(&bytes);
    for i in 0..MAX_LAYOUTS {
        let mut l = layout_for(None, &format!("Big {i}"));
        l.logo.as_mut().unwrap().data = big.clone();
        e.save_layout(l).unwrap();
    }
    let size = serde_json::to_vec(&remail_core::cloud::mapping::collect(&e, remail_core::proto::sections::LAYOUTS).unwrap().unwrap().into_iter().map(|(id, r)| (id, r.fields)).collect::<std::collections::BTreeMap<_, _>>()).unwrap().len();
    assert!(size + 16 * 1024 < remail_core::proto::sections::MAX_SECTION_BYTES, "{size} bytes of layouts");
    // One byte over the logo limit is refused.
    bytes.push(7);
    let mut l = layout_for(None, "Too big");
    l.logo.as_mut().unwrap().data = base64::engine::general_purpose::STANDARD.encode(&bytes);
    assert!(e.save_layout(l).is_err());
}
