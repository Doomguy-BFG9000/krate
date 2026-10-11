// Sign In: signs the person in to GitHub through the system's sheet
// (krate:ui/sign-in, IC-923) and lists their repositories.
//
// The GitHub OAuth app's client id and secret are baked in at build time
// (GITHUB_CLIENT_ID, GITHUB_CLIENT_SECRET); its callback URL is
// `kratesignin://signed-in`. GitHub still wants the secret beside the PKCE
// verifier when the code is traded, so a real app would trade it on its
// own server; this probe proves the sheet and the round trip.
//
//   krate run sign-in-probe.krate -- quick   one line, then exit:
//                                            `sign-in:code=<c> state=ok pkce=43`,
//                                            `sign-in:cancelled` or
//                                            `sign-in:problem=<why>`
#![no_std]
extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use krate::bindings::krate::net::http_client;
use krate::gfx::{canvas2d, types as gfx};
use krate::layout::{Rect, Screen};
use krate::net::{Header, HttpMethod, Request};
use krate::ui::sign_in::{self, SignInError};
use krate::{
    io::{args, stdio},
    random,
    ui::{
        events, tree,
        types::{self, Event},
        window,
    },
    Guest,
};

const SCHEME: &str = "kratesignin";
const REDIRECT: &str = "kratesignin://signed-in";
const CLIENT_ID: &str = match option_env!("GITHUB_CLIENT_ID") {
    Some(id) => id,
    None => "",
};
const CLIENT_SECRET: &str = match option_env!("GITHUB_CLIENT_SECRET") {
    Some(secret) => secret,
    None => "",
};

fn rgb(r: f32, g: f32, b: f32) -> gfx::Color {
    gfx::Color { r, g, b, a: 1.0 }
}

fn style(weight: u16) -> gfx::TextStyle {
    gfx::TextStyle { weight, italic: false, letter_spacing: 0.0, family: gfx::FontFamily::Sans }
}

fn say(line: &str) {
    let out = stdio::stdout();
    let _ = out.write(line.as_bytes());
    let _ = out.write(b"\n");
}

fn node(id: u64, parent: Option<u64>, kind: types::WidgetKind) -> types::WidgetNode {
    types::WidgetNode {
        id,
        parent,
        kind,
        label: None,
        role: None,
        style: types::Style {
            width: None,
            height: None,
            grow: 1.0,
            padding: 0.0,
            text: None,
            place: None,
            box_: None,
        },
        checked: None,
        value: None,
        selected: None,
        text_cursor: None,
    }
}

/// One query value of an address.
fn query<'a>(address: &'a str, name: &str) -> Option<&'a str> {
    let q = address.split_once('?')?.1.split('#').next()?;
    q.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == name).then_some(v)
    })
}

/// A random `state`, hex.
fn fresh_state() -> String {
    random::bytes(16).unwrap_or_default().iter().map(|b| format!("{b:02x}")).collect()
}

fn authorize_url(state: &str, challenge: &str) -> String {
    format!(
        "https://github.com/login/oauth/authorize?client_id={CLIENT_ID}&redirect_uri={REDIRECT}\
         &scope=read:user&state={state}&code_challenge={challenge}&code_challenge_method=S256"
    )
}

/// The string value of every `"key":"value"` in a JSON text, in order.
fn json_strings(text: &str, key: &str) -> Vec<String> {
    let needle = format!("\"{key}\":\"");
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(&needle) {
        rest = &rest[at + needle.len()..];
        if let Some(end) = rest.find('"') {
            out.push(rest[..end].to_string());
            rest = &rest[end..];
        }
    }
    out
}

fn fetch(method: HttpMethod, url: &str, headers: Vec<Header>, body: &[u8]) -> Result<String, String> {
    let response = http_client::fetch(&Request {
        method,
        url: url.to_string(),
        headers,
        body: body.to_vec(),
        timeout_millis: Some(20_000),
    })
    .map_err(|e| format!("{e:?}"))?;
    String::from_utf8(response.body).map_err(|_| "the answer was not text".to_string())
}

fn header(name: &str, value: &str) -> Header {
    Header { name: name.to_string(), value: value.to_string() }
}

/// Sign in and list the repositories, or say why not.
fn sign_in_and_list(win: u64) -> Result<Vec<String>, String> {
    if CLIENT_ID.is_empty() {
        return Err("This build has no GitHub OAuth app (GITHUB_CLIENT_ID).".to_string());
    }
    let (verifier, challenge) = sign_in::pkce();
    let state = fresh_state();
    let back = match sign_in::start(win, &authorize_url(&state, &challenge), SCHEME) {
        Ok(back) => back,
        Err(SignInError::Cancelled) => return Err("You closed the sign-in sheet.".to_string()),
        Err(e) => return Err(format!("{e:?}")),
    };
    if query(&back, "state") != Some(state.as_str()) {
        return Err("GitHub sent back a different state: not signing in.".to_string());
    }
    let code = query(&back, "code").ok_or("GitHub sent back no code")?;
    let form = format!(
        "client_id={CLIENT_ID}&client_secret={CLIENT_SECRET}&code={code}&redirect_uri={REDIRECT}&code_verifier={verifier}"
    );
    let token_text = fetch(
        HttpMethod::Post,
        "https://github.com/login/oauth/access_token",
        alloc::vec![
            header("Accept", "application/json"),
            header("Content-Type", "application/x-www-form-urlencoded"),
        ],
        form.as_bytes(),
    )?;
    let token = json_strings(&token_text, "access_token")
        .into_iter()
        .next()
        .ok_or_else(|| format!("GitHub gave no token: {token_text}"))?;
    let repos = fetch(
        HttpMethod::Get,
        "https://api.github.com/user/repos?per_page=100&sort=updated",
        alloc::vec![
            header("Accept", "application/vnd.github+json"),
            header("Authorization", &format!("Bearer {token}")),
            header("User-Agent", "Krate-Sign-In"),
        ],
        &[],
    )?;
    Ok(json_strings(&repos, "full_name"))
}

fn quick(win: u64) -> i32 {
    let (verifier, challenge) = sign_in::pkce();
    let state = fresh_state();
    let url = format!(
        "https://example.com/authorize?client_id=probe&state={state}&code_challenge={challenge}"
    );
    match sign_in::start(win, &url, SCHEME) {
        Ok(back) => {
            let same = query(&back, "state") == Some(state.as_str());
            say(&format!(
                "sign-in:code={} state={} pkce={}",
                query(&back, "code").unwrap_or(""),
                if same { "ok" } else { "wrong" },
                verifier.len()
            ));
        }
        Err(SignInError::Cancelled) => say("sign-in:cancelled"),
        Err(e) => say(&format!("sign-in:problem={e:?}")),
    }
    // A bad request is refused before any sheet opens.
    if let Err(SignInError::Invalid(_)) = sign_in::start(win, "http://example.com/authorize", SCHEME) {
        say("sign-in:http-refused");
    }
    0
}

struct Page {
    status: String,
    repos: Vec<String>,
}

fn button_rect(screen: &Screen) -> Rect {
    let area = screen.safe_area().inset(16.0, 16.0, 16.0, 16.0);
    Rect::new(area.x, area.y + 80.0, area.w, 52.0)
}

fn draw(canvas: u64, screen: &Screen, page: &Page) -> Result<(), gfx::GfxError> {
    let ink = rgb(0.07, 0.07, 0.09);
    let muted = rgb(0.43, 0.44, 0.48);
    canvas2d::clear(canvas, rgb(0.96, 0.96, 0.97))?;
    let area = screen.safe_area().inset(16.0, 16.0, 16.0, 16.0);
    canvas2d::draw_text_styled(canvas, "Sign In", gfx::Point { x: area.x, y: area.y + 32.0 }, 30.0, ink, style(800))?;
    canvas2d::draw_text_styled(canvas, "Through GitHub's own page, never a password here.", gfx::Point { x: area.x, y: area.y + 58.0 }, 14.0, muted, style(450))?;
    let b = button_rect(screen);
    let radii = gfx::CornerRadii { top_left: 12.0, top_right: 12.0, bottom_right: 12.0, bottom_left: 12.0 };
    canvas2d::fill_round_rect(canvas, gfx::Rect { x: b.x, y: b.y, width: b.w, height: b.h }, radii, ink)?;
    let label = if page.repos.is_empty() { "Sign in with GitHub" } else { "Sign in again" };
    let m = canvas2d::measure_text_styled(canvas, label, 17.0, style(600))?;
    canvas2d::draw_text_styled(canvas, label, gfx::Point { x: b.x + (b.w - m.width) / 2.0, y: b.y + 33.0 }, 17.0, rgb(1.0, 1.0, 1.0), style(600))?;
    let mut y = b.bottom() + 34.0;
    canvas2d::draw_text_styled(canvas, &page.status, gfx::Point { x: area.x, y }, 15.0, muted, style(500))?;
    y += 30.0;
    for repo in &page.repos {
        if y > area.bottom() {
            break;
        }
        canvas2d::draw_text_styled(canvas, repo, gfx::Point { x: area.x, y }, 16.0, ink, style(500))?;
        y += 28.0;
    }
    canvas2d::present(canvas)
}

struct Component;

impl Guest for Component {
    fn run() -> i32 {
        let Ok(win) = window::create("Sign In", types::WindowSize { width: 420, height: 720 }) else {
            return 1;
        };
        if args::raw().split('\n').any(|a| a == "quick") {
            return quick(win);
        }
        if tree::set_root(win, &node(1, None, types::WidgetKind::Stack)).is_err()
            || tree::upsert_node(win, &node(2, Some(1), types::WidgetKind::Canvas)).is_err()
        {
            return 1;
        }
        let Ok(canvas) = canvas2d::bind(win, 2) else {
            return 1;
        };
        let mut page = Page { status: String::from("Not signed in."), repos: Vec::new() };
        loop {
            if draw(canvas, &Screen::of(win), &page).is_err() {
                return 1;
            }
            let mut tapped = false;
            let mut next = events::wait(Some(250));
            while let Some(event) = next {
                match event {
                    Event::CloseRequested(_) => return 0,
                    Event::Pointer(p) if p.button.is_some() && !p.pressed => {
                        tapped |= button_rect(&Screen::of(win)).contains(p.x, p.y);
                    }
                    _ => {}
                }
                next = events::poll();
            }
            if tapped {
                match sign_in_and_list(win) {
                    Ok(repos) => {
                        page.status = format!("Signed in. {} repositories:", repos.len());
                        say(&format!("sign-in:repos={}", repos.len()));
                        page.repos = repos;
                    }
                    Err(why) => {
                        say(&format!("sign-in:problem={why}"));
                        page.status = why;
                    }
                }
            }
        }
    }
}

krate::export!(Component);
