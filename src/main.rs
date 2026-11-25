// Linen — a soft, clean writing app for notes, lists and passwords. Spelling is
// underlined as you type, and right-click offers spelling fixes plus a menu of helpers:
// offline AI rewrites and synonyms, Rinse cleanups, case, insert date/time/password,
// and saving straight into a project or the Obsidian vault.
// GTK4 window hosting a WebKit view; the UI is served from a loopback server (like Velvet).

mod ai;
mod files;
mod lock;
mod server;
mod theme;

use gtk4::gio;
use gtk4::prelude::*;
use serde_json::json;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use webkit6::prelude::*;
use webkit6::ContextMenuAction as Stock;

const APP_ID: &str = "local.kirk.Linen";

thread_local! {
    static VIEW: RefCell<Option<webkit6::WebView>> = const { RefCell::new(None) };
    /// Set once every tab has been saved, so the next close really closes.
    static CLOSE_OK: Cell<bool> = const { Cell::new(false) };
    /// Kept alive for the life of the window: watches Omarchy's theme folder.
    static THEME_WATCH: RefCell<Option<gio::FileMonitor>> = const { RefCell::new(None) };
    static TOGGLE_WATCH: RefCell<Option<gio::FileMonitor>> = const { RefCell::new(None) };
}

/// The window's own background, so there's no flash of another color before the page paints.
fn window_color(bg: &str) -> gtk4::gdk::RGBA {
    gtk4::gdk::RGBA::parse(bg).unwrap_or(gtk4::gdk::RGBA::new(0.11, 0.06, 0.15, 1.0))
}

/// See-through window plus a compositor blur request (like Omarchy's glass.css), only
/// while the Omarchy theme is followed and glassy windows are on.
fn apply_glass(win: &gtk4::Window, view: &webkit6::WebView, t: Option<&serde_json::Value>, follow: bool) {
    let glass = follow && t.is_some_and(|t| t.get("glass").is_some());
    if glass {
        win.add_css_class("linen-glass");
        view.set_background_color(&gtk4::gdk::RGBA::new(0.0, 0.0, 0.0, 0.0));
    } else {
        win.remove_css_class("linen-glass");
        if let Some(t) = t.filter(|_| follow) {
            view.set_background_color(&window_color(t["bg"].as_str().unwrap_or("")));
        }
    }
}

fn install_glass_css() {
    let Some(display) = gtk4::gdk::Display::default() else { return };
    let css = gtk4::CssProvider::new();
    css.load_from_string("window.linen-glass { background: transparent; backdrop-filter: blur(20px); }");
    gtk4::style_context_add_provider_for_display(&display, &css, gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION);
}

/// Pushes the running Omarchy colors into the page (the page ignores them unless the
/// Omarchy theme is chosen) and matches the window background.
fn send_theme() {
    let Some(t) = theme::current() else { return };
    let follow = matches!(files::settings()["theme"].as_str(), None | Some("omarchy"));
    VIEW.with(|v| {
        if let Some(view) = v.borrow().as_ref() {
            if let Some(win) = view.root().and_then(|r| r.downcast::<gtk4::Window>().ok()) {
                apply_glass(&win, view, Some(&t), follow);
            }
            view.evaluate_javascript(&format!("window.linenTheme && linenTheme({t})"), None, None, None::<&gio::Cancellable>, |_| {});
        }
    });
}

/// Watches ~/.local/state/omarchy/current for theme changes (inotify, no polling: quiet
/// for the fan). Omarchy bumps theme.generation and swaps the theme folder on a change.
fn watch_theme() {
    let dir = gio::File::for_path(theme::state_dir());
    let Ok(monitor) = dir.monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, None::<&gio::Cancellable>) else { return };
    let pending = std::rc::Rc::new(Cell::new(false));
    let pending_theme = pending.clone();
    monitor.connect_changed(move |_, file, _, _| {
        let pending = &pending_theme;
        let name = file.basename().map(|b| b.to_string_lossy().into_owned()).unwrap_or_default();
        if !(name == "theme.generation" || name == "theme" || name == "window-glassy.lua") || pending.get() {
            return;
        }
        // One update after the burst of file events a theme switch makes.
        pending.set(true);
        let pending = pending.clone();
        gtk4::glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || {
            pending.set(false);
            send_theme();
        });
    });
    // Omarchy's glassy-windows switch is a file that comes and goes: watch its folder too.
    let toggles = theme::glass_toggle().parent().map(gio::File::for_path).and_then(|d| d.monitor_directory(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>).ok());
    if let Some(t) = &toggles {
        let pending = pending.clone();
        t.connect_changed(move |_, file, _, _| {
            if file.basename().is_some_and(|b| b == std::path::Path::new("window-glassy.lua")) && !pending.get() {
                pending.set(true);
                let pending = pending.clone();
                gtk4::glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || {
                    pending.set(false);
                    send_theme();
                });
            }
        });
    }
    THEME_WATCH.with(|w| *w.borrow_mut() = Some(monitor));
    TOGGLE_WATCH.with(|w| *w.borrow_mut() = toggles);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("serve") => {
            // Development in a normal browser: this mode alone puts the token in the page.
            server::TOKEN_IN_PAGE.store(true, std::sync::atomic::Ordering::Relaxed);
            let (port, token) = server::start();
            println!("http://127.0.0.1:{port}/  token {token}");
            loop {
                std::thread::park();
            }
        }
        Some("password") => println!("{}", server::password(args.get(2).and_then(|n| n.parse().ok()).unwrap_or(20))),
        Some("--help" | "-h") => println!(
            "linen [FILE…]          open Linen (with files)\n\
             linen password [LEN]   print a strong password\n\
             linen serve            run the UI server only (for development)"
        ),
        _ => run_gui(&args[1..]),
    }
}

fn run_gui(files: &[String]) {
    let (port, token) = server::start();
    let app = gtk4::Application::builder().application_id(APP_ID).flags(gio::ApplicationFlags::HANDLES_OPEN).build();
    let tok = token.clone();
    app.connect_startup(|_| install_glass_css());
    app.connect_activate(move |app| show(app, port, &tok, &[]));
    app.connect_open(move |app, files, _| {
        let paths: Vec<PathBuf> = files.iter().filter_map(|f| f.path()).collect();
        show(app, port, &token, &paths);
    });
    let mut argv = vec!["linen".to_string()];
    argv.extend(files.iter().cloned());
    app.run_with_args(&argv);
}

fn run_js(code: &str) {
    VIEW.with(|v| {
        if let Some(view) = v.borrow().as_ref() {
            view.evaluate_javascript(code, None, None, None::<&gio::Cancellable>, |_| {});
        }
    });
}

/// Queue files for the UI, then raise the window (building it the first time).
fn show(app: &gtk4::Application, port: u16, token: &str, paths: &[PathBuf]) {
    server::PENDING.lock().unwrap().extend(paths.iter().cloned());
    if let Some(w) = app.active_window() {
        w.present();
        run_js("window.linenPending && linenPending()");
        return;
    }
    build_window(app, port, token);
}

/// One right-click item that hands its id to the page.
fn item(id: &str, label: &str) -> webkit6::ContextMenuItem {
    let action = gio::SimpleAction::new(&format!("linen-{id}"), None);
    let id = id.to_string();
    action.connect_activate(move |_, _| run_js(&format!("linen.menu({})", json!(id))));
    webkit6::ContextMenuItem::from_gaction(&action, label, None)
}

fn submenu(label: &str, items: &[(&str, &str)]) -> webkit6::ContextMenuItem {
    let menu = webkit6::ContextMenu::new();
    for (id, text) in items {
        menu.append(&item(id, text));
    }
    webkit6::ContextMenuItem::with_submenu(label, &menu)
}

/// Keep WebKit's spelling suggestions and editing basics, drop browser items, then add Linen's.
fn build_menu(menu: &webkit6::ContextMenu, hit: &webkit6::HitTestResult) -> bool {
    if !hit.context_is_editable() {
        return true; // No menu over the toolbar or the margins.
    }
    let keep = [Stock::SpellingGuess, Stock::NoGuessesFound, Stock::IgnoreSpelling, Stock::LearnSpelling, Stock::Cut, Stock::Copy, Stock::Paste, Stock::SelectAll];
    for it in menu.items() {
        if !keep.contains(&it.stock_action()) && !it.is_separator() {
            menu.remove(&it);
        }
    }
    // Tidy separators left behind at the ends or doubled up.
    let mut last_sep = true;
    for it in menu.items() {
        if it.is_separator() {
            if last_sep {
                menu.remove(&it);
            }
            last_sep = true;
        } else {
            last_sep = false;
        }
    }
    if menu.n_items() > 0 && !menu.items().last().is_some_and(|i| i.is_separator()) {
        menu.append(&webkit6::ContextMenuItem::new_separator());
    }
    menu.append(&item("ai_grammar", "✨ Fix spelling & grammar"));
    menu.append(&item("synonyms", "Synonyms…"));
    menu.append(&submenu("Rewrite", &[("ai_shorter", "Make it shorter"), ("ai_friendly", "Make it friendly"), ("ai_professional", "Make it professional"), ("ai_bullets", "Turn into bullets"), ("ai_summary", "Summarize"), ("ai_obsidian", "Notes → Obsidian note")]));
    menu.append(&submenu("Clean up", &[("smart_clean", "Smart clean"), ("remove_extra_spaces", "Remove extra spaces"), ("remove_empty_lines", "Remove empty lines"), ("join_paragraphs", "Join wrapped lines"), ("quotes_straight", "Straight quotes"), ("remove_duplicate_lines", "Remove duplicate lines"), ("sort_lines", "Sort lines A → Z")]));
    menu.append(&submenu("Change case", &[("upper_case", "UPPERCASE"), ("lower_case", "lowercase"), ("title_case", "Title Case"), ("sentence_case", "Sentence case")]));
    menu.append(&submenu("Insert", &[("insert_date", "Today's date"), ("insert_time", "The time"), ("insert_datetime", "Date and time"), ("insert_checkbox", "Checkbox  - [ ]"), ("insert_divider", "Divider line"), ("insert_password", "Strong password")]));
    menu.append(&webkit6::ContextMenuItem::new_separator());
    menu.append(&item("copy_plain", "Copy as plain text"));
    menu.append(&item("count", "Count words"));
    menu.append(&webkit6::ContextMenuItem::new_separator());
    menu.append(&item("save_project", "Save to project…"));
    menu.append(&item("save_vault", "Save to Obsidian…"));
    false
}

fn build_window(app: &gtk4::Application, port: u16, token: &str) {
    let s = files::settings();
    let size = |k: &str, d: i64| s[k].as_i64().unwrap_or(d) as i32;
    let window = gtk4::ApplicationWindow::builder()
        .application(app)
        .title("Linen")
        .default_width(size("window_width", 980))
        .default_height(size("window_height", 720))
        .build();
    // Closing waits for every tab to save (review fix #2): the first close asks the page
    // to save all tabs and the session, then closes for real. A 3-second fallback means a
    // stuck page can never stop Linen from quitting.
    window.connect_close_request(|w| {
        if CLOSE_OK.with(Cell::get) {
            return gtk4::glib::Propagation::Proceed;
        }
        let (width, height) = w.default_size();
        let _ = files::update_settings(&json!({ "window_width": width, "window_height": height }));
        let finish = {
            let w = w.clone();
            move || {
                if !CLOSE_OK.with(Cell::get) {
                    CLOSE_OK.with(|c| c.set(true));
                    w.close();
                }
            }
        };
        let fallback = finish.clone();
        gtk4::glib::timeout_add_local_once(std::time::Duration::from_secs(3), fallback);
        let ran = VIEW.with(|v| {
            v.borrow().as_ref().map(|view| {
                view.call_async_javascript_function(
                    "if (window.linenClosing) { await window.linenClosing(); } return true;",
                    None, None, None, None::<&gio::Cancellable>, move |_| finish(),
                );
            })
        });
        if ran.is_none() {
            return gtk4::glib::Propagation::Proceed;
        }
        gtk4::glib::Propagation::Stop
    });

    // Spelling, underlined as you type. "Learn Spelling" adds a word to your own dictionary.
    let context = webkit6::WebContext::default().expect("web context");
    context.set_spell_checking_enabled(true);
    context.set_spell_checking_languages(&["en_US"]);

    // The API token goes straight into Linen's own page before any script runs, and only
    // for Linen's own address, so it is never served over HTTP (review fix #1).
    let content = webkit6::UserContentManager::new();
    // (The script checks the address itself; WebKit's allow-list patterns can't name a port.)
    let origin = format!("http://127.0.0.1:{port}");
    content.add_script(&webkit6::UserScript::new(
        &format!("if (location.origin === {}) window.LINEN_TOKEN = {};", json!(origin), json!(token)),
        webkit6::UserContentInjectedFrames::TopFrame,
        webkit6::UserScriptInjectionTime::Start,
        &[],
        &[],
    ));
    let view = webkit6::WebView::builder().user_content_manager(&content).build();
    // Start in the right color: the Omarchy theme when it's being followed.
    let follow = matches!(files::settings()["theme"].as_str(), None | Some("omarchy"));
    let initial = theme::current();
    match initial.as_ref().filter(|_| follow) {
        Some(t) => view.set_background_color(&window_color(t["bg"].as_str().unwrap_or(""))),
        None => view.set_background_color(&gtk4::gdk::RGBA::new(0.13, 0.09, 0.19, 1.0)),
    }
    apply_glass(window.upcast_ref::<gtk4::Window>(), &view, initial.as_ref(), follow);
    let dev = std::env::var_os("LINEN_DEV").is_some();
    if let Some(settings) = webkit6::prelude::WebViewExt::settings(&view) {
        settings.set_enable_developer_extras(dev);
        settings.set_enable_back_forward_navigation_gestures(false);
        settings.set_javascript_can_access_clipboard(true);
    }
    view.connect_context_menu(|_, menu, hit| build_menu(menu, hit));
    // The window only ever shows Linen's own page. Links (and anything dragged onto the
    // window) never navigate it; https ones open in the browser (review fix #14).
    let home = format!("http://127.0.0.1:{port}/");
    view.connect_decide_policy(move |_, decision, kind| {
        if kind != webkit6::PolicyDecisionType::NewWindowAction && kind != webkit6::PolicyDecisionType::NavigationAction {
            return false;
        }
        let uri = decision
            .downcast_ref::<webkit6::NavigationPolicyDecision>()
            .and_then(|nav| nav.navigation_action())
            .and_then(|a| a.request())
            .and_then(|r| r.uri())
            .map(|u| u.to_string())
            .unwrap_or_default();
        if kind == webkit6::PolicyDecisionType::NavigationAction && uri.starts_with(&home) {
            return false;
        }
        if uri.starts_with("https://") {
            let _ = std::process::Command::new("xdg-open").arg(&uri).spawn();
        }
        decision.ignore();
        true
    });
    view.load_uri(&format!("http://127.0.0.1:{port}/"));
    // Swallow touchpad pinches so the page can never zoom.
    let no_pinch = gtk4::GestureZoom::new();
    no_pinch.set_propagation_phase(gtk4::PropagationPhase::Capture);
    no_pinch.connect_begin(|g, _| { g.set_state(gtk4::EventSequenceState::Claimed); });
    window.add_controller(no_pinch);
    window.set_child(Some(&view));
    VIEW.with(|v| *v.borrow_mut() = Some(view));
    watch_theme();
    window.present();
}
