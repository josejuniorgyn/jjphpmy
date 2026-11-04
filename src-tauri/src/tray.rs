//! Ícone e menu da bandeja do sistema (área de notificação).

use crate::config::base_path;
use crate::AppState;
use std::path::PathBuf;
use std::process::Command;
use tauri::menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Wry};
use tauri_plugin_opener::OpenerExt;

const HOSTS_FILE: &str = r"C:\Windows\System32\drivers\etc\hosts";

/// (id do item de menu, rótulo, arquivo)
fn config_files() -> Vec<(&'static str, &'static str, PathBuf)> {
    vec![
        ("edit:apache", "Apache - httpd.conf", base_path(&["etc", "apache", "httpd.conf"])),
        ("edit:php", "PHP - php.ini", base_path(&["etc", "php", "php.ini"])),
        ("edit:php-cgi", "PHP para Nginx - php-cgi.ini", base_path(&["etc", "php", "php-cgi.ini"])),
        ("edit:mysql", "MySQL - my.ini", base_path(&["etc", "mysql", "my.ini"])),
        ("edit:postgresql", "PostgreSQL - postgresql.conf", base_path(&["etc", "postgresql", "postgresql.conf"])),
        ("edit:nginx", "Nginx - nginx.conf", base_path(&["etc", "nginx", "nginx.conf"])),
        ("edit:redis", "Redis - redis.conf", base_path(&["etc", "redis", "redis.conf"])),
        ("edit:mongodb", "MongoDB - mongod.cfg", base_path(&["etc", "mongodb", "mongod.cfg"])),
        ("edit:phpmyadmin", "phpMyAdmin - config.inc.php", base_path(&["etc", "phpmyadmin", "config.inc.php"])),
        ("edit:hosts", "Windows - hosts (administrador)", PathBuf::from(HOSTS_FILE)),
    ]
}

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let item = |id: &str, label: &str| MenuItem::with_id(app, id, label, true, None::<&str>);
    let sep = || PredefinedMenuItem::separator(app);

    let edit_items = config_files()
        .iter()
        .map(|(id, label, _)| item(*id, *label))
        .collect::<tauri::Result<Vec<_>>>()?;
    let edit_refs: Vec<&dyn IsMenuItem<Wry>> = edit_items.iter().map(|i| i as &dyn IsMenuItem<Wry>).collect();
    let edit = Submenu::with_items(app, "Editar configuração", true, &edit_refs)?;

    let menu = Menu::with_items(
        app,
        &[
            &item("open", "Abrir JJPHPMY")?,
            &sep()?,
            &item("start_all", "Iniciar tudo")?,
            &item("stop_all", "Parar tudo")?,
            &item("terminal", "Terminal")?,
            &sep()?,
            &edit,
            &item("phpmyadmin", "phpMyAdmin")?,
            &item("mailpit", "Mailpit (e-mails)")?,
            &item("www", "Abrir pasta www")?,
            &item("backup", "Fazer backup agora")?,
            &item("notepadpp", "Notepad++")?,
            &item("winscp", "WinSCP")?,
            &item("filezilla", "FileZilla")?,
            &sep()?,
            &item("settings", "Configurações...")?,
            &item("services", "Serviços e Portas...")?,
            &item("about", "Sobre...")?,
            &sep()?,
            &item("quit", "Sair")?,
        ],
    )?;

    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().expect("ícone padrão ausente").clone())
        .tooltip("JJPHPMY")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| on_menu(app, event.id.as_ref()))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } = event {
                show_window(tray.app_handle(), None);
            }
        })
        .build(app)?;
    Ok(())
}

/// Mostra a janela principal; `tab` = "pacotes" | "configuracoes" | "servicos" | "sobre".
pub fn show_window(app: &AppHandle, tab: Option<&str>) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
        if let Some(tab) = tab {
            let _ = w.emit("navigate", tab);
        }
    }
}

pub fn set_tooltip(app: &AppHandle, text: &str) {
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_tooltip(Some(text));
    }
}

/// Exibe uma mensagem na janela principal.
fn notify(app: &AppHandle, msg: impl Into<String>) {
    show_window(app, None);
    let _ = app.emit("toast", msg.into());
}

fn on_menu(app: &AppHandle, id: &str) {
    match id {
        "open" => show_window(app, None),
        "settings" => show_window(app, Some("configuracoes")),
        "services" => show_window(app, Some("servicos")),
        "about" => show_window(app, Some("sobre")),
        "start_all" => {
            let app = app.clone();
            std::thread::spawn(move || {
                let errors = crate::start_all(&app);
                if !errors.is_empty() {
                    notify(&app, errors.join("\n\n"));
                }
            });
        }
        "stop_all" => {
            let app = app.clone();
            std::thread::spawn(move || crate::stop_all(&app));
        }
        "backup" => {
            let app = app.clone();
            std::thread::spawn(move || {
                let msg = match crate::do_backup(&app) {
                    Ok(r) => crate::backup_message(&r),
                    Err(e) => e,
                };
                notify(&app, msg);
            });
        }
        "notepadpp" => open_tool(app, "notepadpp", "notepad++.exe", "Notepad++"),
        "winscp" => open_tool(app, "winscp", "WinSCP.exe", "WinSCP"),
        "filezilla" => match filezilla_exe() {
            Some(exe) => {
                if let Err(e) = Command::new(exe).spawn() {
                    notify(app, format!("Não foi possível abrir o FileZilla: {e}"));
                }
            }
            None => {
                open_url(app, FILEZILLA_PAGE.into());
                notify(app, "FileZilla não encontrado: abri a página oficial para você baixar e instalar.");
            }
        },
        "terminal" => {
            let cfg = app.state::<AppState>().config.lock().unwrap().clone();
            if let Err(e) = crate::services::open_terminal(&cfg, &crate::confgen::Layout::system()) {
                notify(app, e);
            }
        }
        "phpmyadmin" => open_url(app, phpmyadmin_url(app)),
        "mailpit" => open_url(app, "http://localhost:8025".into()),
        "www" => {
            let root = app.state::<AppState>().config.lock().unwrap().settings.document_root.clone();
            let _ = std::fs::create_dir_all(&root);
            if let Err(e) = app.opener().open_path(root, None::<&str>) {
                notify(app, format!("Não foi possível abrir a pasta: {e}"));
            }
        }
        // Para os serviços com calma (bancos fecham os arquivos) antes de sair.
        "quit" => {
            let app = app.clone();
            set_tooltip(&app, "JJPHPMY - parando os serviços...");
            std::thread::spawn(move || {
                crate::stop_all(&app);
                app.exit(0);
            });
        }
        other if other.starts_with("edit:") => edit_file(app, other),
        _ => {}
    }
}

/// Abre uma ferramenta instalada pelo JJPHPMY (versão ativa em `bin\{id}`).
fn open_tool(app: &AppHandle, id: &str, exe: &str, name: &str) {
    let path = app.state::<AppState>().config.lock().unwrap().installed.iter()
        .find(|p| p.id == id && p.active)
        .map(|p| PathBuf::from(&p.path).join(exe));
    match path.filter(|p| p.exists()) {
        Some(exe) => {
            if let Err(e) = Command::new(exe).spawn() {
                notify(app, format!("Não foi possível abrir o {name}: {e}"));
            }
        }
        None => notify(app, format!("{name} não está instalado. Instale na aba Pacotes (Ferramentas).")),
    }
}

const FILEZILLA_PAGE: &str = "https://filezilla-project.org/download.php?type=client";

/// FileZilla instalado pelo instalador oficial (para todos ou só para o usuário).
fn filezilla_exe() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .map(PathBuf::from)
        .collect();
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        dirs.push(PathBuf::from(local).join("Programs"));
    }
    dirs.into_iter().map(|d| d.join("FileZilla FTP Client").join("filezilla.exe")).find(|p| p.exists())
}

fn open_url(app: &AppHandle, url: String) {
    if let Err(e) = app.opener().open_url(url, None::<&str>) {
        notify(app, format!("Não foi possível abrir o navegador: {e}"));
    }
}

/// phpMyAdmin é servido pelo Apache se ativado; senão, pelo Nginx.
fn phpmyadmin_url(app: &AppHandle) -> String {
    let cfg = app.state::<AppState>().config.lock().unwrap().clone();
    let port = ["apache", "nginx"]
        .iter()
        .filter_map(|id| cfg.service(id))
        .find(|s| s.enabled)
        .or_else(|| cfg.service("apache"))
        .map(|s| s.port)
        .unwrap_or(80);
    match port {
        80 => "http://localhost/phpmyadmin".into(),
        p => format!("http://localhost:{p}/phpmyadmin"),
    }
}

fn edit_file(app: &AppHandle, menu_id: &str) {
    let Some((_, _, path)) = config_files().into_iter().find(|(id, _, _)| *id == menu_id) else {
        return;
    };
    if !path.exists() {
        notify(app, format!("Arquivo não encontrado: {} (instale o pacote primeiro).", path.display()));
        return;
    }
    let editor = app.state::<AppState>().config.lock().unwrap().settings.editor.clone();

    let result = if menu_id == "edit:hosts" {
        // O arquivo hosts só pode ser salvo com privilégio de administrador.
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-Command",
                &format!("Start-Process -FilePath '{editor}' -ArgumentList '\"{HOSTS_FILE}\"' -Verb RunAs"),
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
    } else {
        Command::new(&editor).arg(&path).spawn()
    };

    if let Err(e) = result {
        notify(app, format!("Não foi possível abrir o editor '{editor}': {e}"));
    }
}
