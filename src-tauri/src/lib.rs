mod backup;
mod catalog;
mod confgen;
mod config;
mod deps;
mod envpath;
mod installer;
mod laragon;
mod pix;
mod services;
mod ssl;
mod tray;
mod versions;
mod vhosts;

use catalog::Catalog;
use confgen::Layout;
use config::{base_path, Config, InstalledPackage, ServiceEntry, Settings};
use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

pub struct AppState {
    pub config: Mutex<Config>,
    pub catalog: Catalog,
    pub services: services::Manager,
    /// Vigia da pasta www (hosts virtuais automáticos).
    pub watcher: Mutex<Option<notify::RecommendedWatcher>>,
}

fn toast(app: &AppHandle, msgs: &[String]) {
    if !msgs.is_empty() {
        let msg = msgs.join("\n");
        eprintln!("[jjphpmy] {msg}");
        let _ = app.emit("toast", msg);
    }
}

/// Depois de qualquer mudança (portas, pastas, versões): regrava os arquivos
/// de `etc\` e atualiza o PATH. Avisos e erros viram toast; não falha.
/// Não pode ser chamada com o lock da configuração preso.
fn refresh(app: &AppHandle, state: &AppState) {
    let cfg = {
        let mut cfg = state.config.lock().unwrap();
        if confgen::ensure_secret(&mut cfg) {
            let _ = cfg.save();
        }
        cfg.clone()
    };
    let mut msgs = Vec::new();
    // O httpd.conf/nginx.conf com SSL aponta para o certificado: ele vem antes.
    if ssl::any_enabled(&cfg) {
        if let Err(e) = ssl::ensure_server(&layout(), &vhosts::wanted_hosts(&cfg, &vhosts::sites(&cfg))) {
            msgs.push(format!("Certificado HTTPS não gerado: {e}"));
        }
    }
    match confgen::generate(&cfg, &Layout::system(), false) {
        Ok(r) if !r.kept.is_empty() => msgs.push(format!(
            "Estes arquivos foram editados por você e não foram atualizados: {}. \
             Use \"Recriar arquivos de configuração\" na aba Serviços e Portas para voltar ao padrão.",
            r.kept.join(", ")
        )),
        Ok(_) => {}
        Err(e) => msgs.push(format!("Arquivos de configuração não gerados: {e}")),
    }
    match envpath::sync(&cfg) {
        Ok(w) => msgs.extend(w),
        Err(e) => msgs.push(format!("PATH do Windows não atualizado: {e}")),
    }
    toast(app, &msgs);
    // Hosts virtuais numa thread: o arquivo hosts pode esperar o UAC.
    let handle = app.clone();
    std::thread::spawn(move || sync_sites(&handle));
}

/* ---------- Hosts virtuais (Etapa 6) ---------- */

/// Gera os `auto.*.conf`, reinicia Apache/Nginx se mudaram e atualiza o
/// arquivo hosts (com UAC). Se o usuário recusar o UAC, não pergunta de novo
/// até a lista de sites mudar.
fn sync_sites(app: &AppHandle) {
    static LOCK: Mutex<()> = Mutex::new(());
    static DECLINED: Mutex<Option<String>> = Mutex::new(None);
    static CA_DECLINED: Mutex<bool> = Mutex::new(false);
    let _guard = LOCK.lock().unwrap();
    let state = app.state::<AppState>();
    let cfg = state.config.lock().unwrap().clone();
    let list = vhosts::sites(&cfg);
    let mut msgs = Vec::new();

    // HTTPS: certificado com os nomes atuais (site novo = certificado novo).
    let mut cert_changed = false;
    if ssl::any_enabled(&cfg) {
        match ssl::ensure_server(&layout(), &vhosts::wanted_hosts(&cfg, &list)) {
            Ok(c) => cert_changed = c,
            Err(e) => msgs.push(format!("Certificado HTTPS não gerado: {e}")),
        }
    }
    match vhosts::write_configs(&cfg, &layout(), &list) {
        Ok(mut changed) => {
            if cert_changed {
                changed = vec!["apache", "nginx"];
            }
            for server in changed.into_iter().filter(|s| state.services.is_running(s)) {
                state.services.stop(server);
                if let Err(e) = state.services.start(&cfg, &layout(), server) {
                    msgs.push(e);
                }
                services_changed(app);
            }
        }
        Err(e) => msgs.push(e),
    }
    if let Some(new) = vhosts::hosts_update(&vhosts::wanted_hosts(&cfg, &list)) {
        let mut declined = DECLINED.lock().unwrap();
        if declined.as_deref() != Some(new.as_str()) {
            match vhosts::write_hosts(&new, &layout()) {
                Ok(()) => *declined = None,
                Err(e) => {
                    msgs.push(e);
                    *declined = Some(new);
                }
            }
        }
    }
    // A CA precisa estar nas raízes confiáveis do usuário; o Windows pede confirmação.
    if ssl::any_enabled(&cfg) && !ssl::ca_trusted(&layout()) {
        let mut declined = CA_DECLINED.lock().unwrap();
        if !*declined {
            if let Err(e) = ssl::trust_ca(&layout()) {
                msgs.push(e);
                *declined = true;
            }
        }
    }
    toast(app, &msgs);
    let _ = app.emit("sites-changed", ());
}

/// Vigia a pasta www: criar, renomear ou apagar uma subpasta atualiza os
/// hosts virtuais (com 1,5 s de espera para juntar vários eventos).
fn watch_www(app: &AppHandle) {
    use notify::Watcher;
    let state = app.state::<AppState>();
    let cfg = state.config.lock().unwrap().clone();
    let mut slot = state.watcher.lock().unwrap();
    *slot = None; // o vigia anterior para (e a thread dele termina)
    let root = std::path::PathBuf::from(&cfg.settings.document_root);
    let _ = std::fs::create_dir_all(&root);
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_ok() {
            let _ = tx.send(());
        }
    });
    let mut watcher = match watcher {
        Ok(w) => w,
        Err(e) => return eprintln!("[jjphpmy] não foi possível vigiar a pasta www: {e}"),
    };
    if let Err(e) = watcher.watch(&root, notify::RecursiveMode::NonRecursive) {
        return eprintln!("[jjphpmy] não foi possível vigiar {}: {e}", root.display());
    }
    *slot = Some(watcher);
    let handle = app.clone();
    std::thread::spawn(move || {
        let mut last = vhosts::sites(&cfg);
        while rx.recv().is_ok() {
            while rx.recv_timeout(std::time::Duration::from_millis(1500)).is_ok() {}
            let cfg = handle.state::<AppState>().config.lock().unwrap().clone();
            let now = vhosts::sites(&cfg);
            if now != last {
                last = now;
                sync_sites(&handle);
            }
        }
    });
}

/* ---------- Backup (Etapa 8) ---------- */

/// Backup dos bancos rodando, com retenção. Demora: rode fora da interface.
pub fn do_backup(app: &AppHandle) -> Result<backup::Report, String> {
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap();
    let state = app.state::<AppState>();
    let cfg = state.config.lock().unwrap().clone();
    let running = state.services.running_ids();
    let report = backup::run(&cfg, &layout(), &running, cfg.settings.backup_keep as usize)?;
    let _ = app.emit("backup-done", ());
    Ok(report)
}

/// Texto do resultado para o aviso na tela.
pub fn backup_message(r: &backup::Report) -> String {
    let mut msg = if r.file.is_empty() {
        "Nenhum backup feito: nenhum banco de dados está rodando.".to_string()
    } else {
        format!("Backup criado: {}\nIncluído: {}", r.file, r.included.join(", "))
    };
    if !r.skipped.is_empty() {
        msg += &format!("\nFicou de fora: {}", r.skipped.join("; "));
    }
    msg
}

#[tauri::command]
async fn backup_now(app: AppHandle) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || do_backup(&app).map(|r| backup_message(&r)))
        .await
        .map_err(|e| e.to_string())?
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct BackupInfo {
    count: usize,
    last: Option<String>,
    dir: String,
}

#[tauri::command]
fn backup_info() -> BackupInfo {
    let list = backup::list(&layout());
    BackupInfo {
        count: list.len(),
        last: list.first().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string()),
        dir: backup::dir(&layout()).display().to_string(),
    }
}

/* ---------- Importar do Laragon e restaurar (Etapa 9) ---------- */

/// Lê o Laragon (pastas, MySQL, programas abertos). Só leitura.
#[tauri::command]
async fn laragon_detect() -> Result<laragon::Detected, String> {
    tauri::async_runtime::spawn_blocking(|| laragon::detect(std::path::Path::new(laragon::DEFAULT_ROOT)))
        .await
        .map_err(|e| e.to_string())
}

/// Copia www e/ou importa o MySQL do Laragon, enviando o andamento no evento `import-log`.
#[tauri::command]
async fn laragon_import(app: AppHandle, www: bool, mysql: bool, password: String) -> Result<String, String> {
    static BUSY: AtomicBool = AtomicBool::new(false);
    if BUSY.swap(true, Ordering::SeqCst) {
        return Err("Já existe uma importação em andamento.".into());
    }
    let result = tauri::async_runtime::spawn_blocking(move || {
        let log = |msg: String| {
            let _ = app.emit("import-log", msg);
        };
        let root = std::path::Path::new(laragon::DEFAULT_ROOT);
        let found = laragon::detect(root);
        if !found.found {
            return Err(format!("Laragon não encontrado em {}.", root.display()));
        }
        // Só o MySQL do Laragon atrapalha (os arquivos de dados ficam em uso).
        if mysql && found.running.iter().any(|p| p.eq_ignore_ascii_case("mysqld.exe")) {
            return Err(
                "Pare o MySQL do Laragon antes de importar (no Laragon, \"Parar tudo\"). \
                 Os dois não devem rodar ao mesmo tempo: usam as portas 80 e 3306."
                    .into(),
            );
        }
        let state = app.state::<AppState>();
        let cfg = state.config.lock().unwrap().clone();
        let mut summary = Vec::new();
        if www {
            let dst = std::path::PathBuf::from(&cfg.settings.document_root);
            log(format!("Copiando {} para {} (arquivos que já existem não são sobrescritos)...", root.join("www").display(), dst.display()));
            let s = laragon::robocopy(&root.join("www"), &dst, true)?;
            log(s);
            summary.push(format!("www: {} itens copiados para {}.", found.www_items, dst.display()));
        }
        if mysql {
            let m = found.mysql.as_ref().ok_or("O Laragon não tem MySQL com dados.")?;
            let s = laragon::import_mysql(m, &cfg, &layout(), &state.services, &password, &log)?;
            services_changed(&app);
            summary.push(s);
        }
        log("Concluído.".into());
        Ok(summary.join("\n"))
    })
    .await
    .map_err(|e| e.to_string());
    BUSY.store(false, Ordering::SeqCst);
    result?
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct BackupFile {
    name: String,
    size_kb: u64,
}

#[tauri::command]
fn backup_list() -> Vec<BackupFile> {
    backup::list(&layout())
        .into_iter()
        .map(|p| BackupFile {
            name: p.file_name().unwrap_or_default().to_string_lossy().to_string(),
            size_kb: std::fs::metadata(&p).map(|m| m.len() / 1024).unwrap_or(0),
        })
        .collect()
}

#[tauri::command]
async fn backup_restore(app: AppHandle, name: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let cfg = state.config.lock().unwrap().clone();
        backup::restore(&cfg, &layout(), &name, &state.services.running_ids()).map(|r| r.join("\n"))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn list_sites(state: State<AppState>) -> Vec<vhosts::Site> {
    let cfg = state.config.lock().unwrap().clone();
    vhosts::sites(&cfg)
}

/// Cria os dados dos bancos ativos que ainda não têm (MySQL, PostgreSQL...).
/// Demora alguns segundos: rode fora da thread da interface.
fn init_databases(app: &AppHandle, state: &AppState) {
    let cfg = state.config.lock().unwrap().clone();
    match confgen::init_data(&cfg, &Layout::system()) {
        Ok(done) if !done.is_empty() => toast(app, &[format!("Banco de dados criado: {}.", done.join(", "))]),
        Ok(_) => {}
        Err(e) => toast(app, &[e]),
    }
}

#[tauri::command]
fn get_config(state: State<AppState>) -> Config {
    state.config.lock().unwrap().clone()
}

#[tauri::command]
fn save_settings(app: AppHandle, state: State<AppState>, settings: Settings) -> Result<(), String> {
    let autolaunch = app.autolaunch();
    if autolaunch.is_enabled().unwrap_or(false) != settings.start_with_windows {
        let res = if settings.start_with_windows { autolaunch.enable() } else { autolaunch.disable() };
        res.map_err(|e| format!("Falha ao configurar a inicialização com o Windows: {e}"))?;
    }
    {
        let mut cfg = state.config.lock().unwrap();
        cfg.settings = settings;
        cfg.save()?;
    }
    refresh(&app, &state);
    watch_www(&app);
    Ok(())
}

#[tauri::command]
fn save_services(app: AppHandle, state: State<AppState>, services: Vec<ServiceEntry>) -> Result<(), String> {
    let conflicts = config::port_conflicts(&services);
    if !conflicts.is_empty() {
        return Err(conflicts.join("\n"));
    }
    {
        let mut cfg = state.config.lock().unwrap();
        cfg.services = services;
        cfg.save()?;
    }
    refresh(&app, &state);
    restart_hint(&app, &state, services::ORDER);
    Ok(())
}

/// Botão "Recriar arquivos de configuração": regrava tudo, inclusive os
/// arquivos editados pelo usuário.
#[tauri::command]
fn regenerate_configs(state: State<AppState>) -> Result<confgen::Report, String> {
    let cfg = {
        let mut cfg = state.config.lock().unwrap();
        if confgen::ensure_secret(&mut cfg) {
            cfg.save()?;
        }
        cfg.clone()
    };
    confgen::generate(&cfg, &Layout::system(), true)
}

#[tauri::command]
fn get_catalog(state: State<AppState>) -> Catalog {
    state.catalog.clone()
}

/// Versões disponíveis de um pacote (cache de 24 h; `force` busca de novo).
#[tauri::command]
async fn fetch_versions(
    state: State<'_, AppState>,
    id: String,
    force: Option<bool>,
) -> Result<versions::VersionList, String> {
    let pkg = state.catalog.get(&id).cloned().ok_or_else(|| format!("Pacote desconhecido: '{id}'"))?;
    let list = versions::versions_for(&pkg, force.unwrap_or(false)).await;
    if let Some(e) = &list.error {
        eprintln!("[jjphpmy] versões de {id}: {e}");
    }
    Ok(list)
}

#[tauri::command]
fn resolve_selection(state: State<AppState>, selected: Vec<String>) -> Result<deps::Resolution, String> {
    deps::resolve(&state.catalog, &selected)
}

#[derive(Debug, Deserialize)]
struct InstallRequest {
    id: String,
    /// `None` = versão mais recente.
    version: Option<String>,
}

/// Impede duas instalações ao mesmo tempo (clique duplo, por exemplo).
static INSTALLING: AtomicBool = AtomicBool::new(false);

struct InstallGuard;
impl Drop for InstallGuard {
    fn drop(&mut self) {
        INSTALLING.store(false, Ordering::SeqCst);
    }
}

/// Registra o pacote instalado; vira a versão ativa se não houver outra.
fn register(state: &AppState, pkg: InstalledPackage) -> Result<(), String> {
    let mut cfg = state.config.lock().unwrap();
    cfg.installed.retain(|p| !p.same(&pkg.id, &pkg.version, pkg.variant.as_deref()));
    let has_active = cfg.installed.iter().any(|p| p.id == pkg.id && p.variant == pkg.variant && p.active);
    // Notepad++ instalado vira o editor do menu "Editar configuração" (se ainda era o Bloco de Notas).
    if pkg.id == "notepadpp" && cfg.settings.editor.eq_ignore_ascii_case("notepad.exe") {
        cfg.settings.editor = std::path::Path::new(&pkg.path).join("notepad++.exe").display().to_string();
    }
    // Pacote novo com serviço: já vem marcado como "Ativado" para o "Iniciar tudo".
    if let Some(svc) = cfg.services.iter_mut().find(|s| s.id == pkg.id) {
        svc.enabled = true;
    }
    cfg.installed.push(InstalledPackage { active: !has_active, ..pkg });
    cfg.save()
}

/// Instala na ordem de `install_order`, emitindo `install-progress`.
/// Para no primeiro erro, porque os pacotes seguintes podem depender dele.
#[tauri::command]
async fn install_packages(
    app: AppHandle,
    state: State<'_, AppState>,
    packages: Vec<InstallRequest>,
) -> Result<Config, String> {
    if INSTALLING.swap(true, Ordering::SeqCst) {
        return Err("Já existe uma instalação em andamento.".into());
    }
    let _guard = InstallGuard;

    let ids: Vec<String> = packages.iter().map(|p| p.id.clone()).collect();
    let plan = deps::resolve(&state.catalog, &ids)?;
    let chosen = |id: &str| plan.install_order.iter().any(|x| x == id);

    for id in &plan.install_order {
        let pkg = state.catalog.get(id).cloned().ok_or_else(|| format!("Pacote desconhecido: '{id}'"))?;
        let version = packages.iter().find(|p| &p.id == id).and_then(|p| p.version.clone());
        // PHP: TS para o Apache, NTS para o Nginx (as duas se houver os dois).
        let variants: Vec<Option<&str>> = if id == "php" {
            let mut v = Vec::new();
            if chosen("apache") || !chosen("nginx") {
                v.push(Some("ts"));
            }
            if chosen("nginx") {
                v.push(Some("nts"));
            }
            v
        } else {
            vec![None]
        };
        for variant in variants {
            let already = version.as_ref().is_some_and(|v| {
                let cfg = state.config.lock().unwrap();
                cfg.installed.iter().any(|p| p.same(id, v, variant))
                    && installer::install_dir(id, v, variant).exists()
            });
            if already {
                let _ = app.emit(
                    "install-progress",
                    installer::Progress {
                        id: id.clone(),
                        version: version.clone().unwrap_or_default(),
                        stage: "skipped".into(),
                        received: 0,
                        total: None,
                        message: Some("Esta versão já está instalada.".into()),
                    },
                );
                continue;
            }
            match installer::install(&app, &pkg, version.clone(), variant).await {
                Ok(Some(installed)) => register(&state, installed)?,
                Ok(None) => {}
                Err(e) => {
                    let _ = app.emit(
                        "install-progress",
                        installer::Progress {
                            id: id.clone(),
                            version: version.clone().unwrap_or_default(),
                            stage: "error".into(),
                            received: 0,
                            total: None,
                            message: Some(e.clone()),
                        },
                    );
                    refresh(&app, &state);
                    return Err(e);
                }
            }
        }
    }
    refresh(&app, &state);

    // Bancos novos: cria a pasta de dados (o MySQL leva alguns segundos).
    for id in plan.install_order.iter().filter(|id| ["mysql", "postgresql", "mongodb", "redis"].contains(&id.as_str())) {
        let _ = app.emit(
            "install-progress",
            installer::Progress {
                id: id.clone(),
                version: String::new(),
                stage: "extract".into(),
                received: 0,
                total: None,
                message: Some("Criando o banco de dados...".into()),
            },
        );
    }
    let (app2, cfg) = (app.clone(), state.config.lock().unwrap().clone());
    let result = tauri::async_runtime::spawn_blocking(move || confgen::init_data(&cfg, &Layout::system()))
        .await
        .map_err(|e| e.to_string())?;
    for id in plan.install_order.iter().filter(|id| ["mysql", "postgresql", "mongodb", "redis"].contains(&id.as_str())) {
        let (stage, message) = match &result {
            Ok(_) => ("done", "Pronto. Banco de dados criado.".to_string()),
            Err(e) => ("error", e.clone()),
        };
        let _ = app2.emit(
            "install-progress",
            installer::Progress {
                id: id.clone(),
                version: String::new(),
                stage: stage.into(),
                received: 0,
                total: None,
                message: Some(message),
            },
        );
    }
    result?;
    Ok(state.config.lock().unwrap().clone())
}

/// Aba "Instalados": torna esta a versão ativa do pacote (e variante).
#[tauri::command]
fn set_active_version(
    app: AppHandle,
    state: State<AppState>,
    id: String,
    version: String,
    variant: Option<String>,
) -> Result<Config, String> {
    {
        let mut cfg = state.config.lock().unwrap();
        if !cfg.installed.iter().any(|p| p.same(&id, &version, variant.as_deref())) {
            return Err(format!("{id} {version} não está instalado."));
        }
        for p in cfg.installed.iter_mut().filter(|p| p.id == id && p.variant == variant) {
            p.active = p.version == version;
        }
        cfg.save()?;
    }
    refresh(&app, &state);
    // Outra versão maior do banco usa outra pasta de dados, que pode não existir.
    let handle = app.clone();
    std::thread::spawn(move || init_databases(&handle, &handle.state::<AppState>()));
    let affected = if id == "php" { vec!["apache", "nginx"] } else { vec![id.as_str()] };
    restart_hint(&app, &state, &affected);
    Ok(state.config.lock().unwrap().clone())
}

/// Avisa que serviços rodando só usam a mudança depois de reiniciar.
fn restart_hint(app: &AppHandle, state: &AppState, ids: &[&str]) {
    let names: Vec<&str> =
        ids.iter().filter(|id| state.services.is_running(id)).map(|id| services::display_name(id)).collect();
    if !names.is_empty() {
        toast(app, &[format!("Reinicie {} para aplicar a mudança.", names.join(", "))]);
    }
}

/// Aba "Instalados": apaga a pasta da versão e tira do registro.
#[tauri::command]
fn remove_installed(
    app: AppHandle,
    state: State<AppState>,
    id: String,
    version: String,
    variant: Option<String>,
) -> Result<Config, String> {
    {
        let mut cfg = state.config.lock().unwrap();
        let Some(pos) = cfg.installed.iter().position(|p| p.same(&id, &version, variant.as_deref())) else {
            return Err(format!("{id} {version} não está instalado."));
        };
        let service = if id == "php" { if variant.as_deref() == Some("nts") { "php-cgi" } else { "apache" } } else { id.as_str() };
        if cfg.installed[pos].active && state.services.is_running(service) {
            return Err(format!(
                "{} está rodando com esta versão. Pare o serviço antes de remover.",
                services::display_name(service)
            ));
        }
        installer::remove(&cfg.installed[pos])?;
        let removed = cfg.installed.remove(pos);
        if removed.active {
            if let Some(next) = cfg.installed.iter_mut().find(|p| p.id == id && p.variant == variant) {
                next.active = true;
            }
        }
        cfg.save()?;
    }
    refresh(&app, &state);
    Ok(state.config.lock().unwrap().clone())
}

/* ---------- Serviços (Etapa 5) ---------- */

/// Avisa a interface e atualiza o tooltip da bandeja.
fn services_changed(app: &AppHandle) {
    let running = app.state::<AppState>().services.running_ids();
    let names: Vec<&str> = running.iter().filter(|id| *id != "php-cgi").map(|id| services::display_name(id)).collect();
    let tooltip = if names.is_empty() { "JJPHPMY - nenhum serviço rodando".to_string() } else { format!("JJPHPMY - {}", names.join(", ")) };
    tray::set_tooltip(app, &tooltip);
    let _ = app.emit("services-changed", ());
}

fn layout() -> Layout {
    Layout::system()
}

/// Inicia os serviços instalados e marcados como "Ativado". Continua mesmo se
/// um falhar; devolve os erros.
pub fn start_all(app: &AppHandle) -> Vec<String> {
    let state = app.state::<AppState>();
    let cfg = state.config.lock().unwrap().clone();
    let ids: Vec<&str> = services::ORDER
        .iter()
        .copied()
        .filter(|id| cfg.service(id).is_some_and(|s| s.enabled) && cfg.installed.iter().any(|p| p.id == *id && p.active))
        .collect();
    if ids.is_empty() {
        return vec!["Nenhum serviço instalado e ativado. Instale na aba Pacotes e marque \"Ativado\" em Serviços e Portas.".into()];
    }
    let mut errors = Vec::new();
    for id in ids {
        if let Err(e) = state.services.start(&cfg, &layout(), id) {
            errors.push(e);
        }
        services_changed(app);
    }
    errors
}

/// Para tudo na ordem inversa (servidores web antes dos bancos).
pub fn stop_all(app: &AppHandle) {
    let state = app.state::<AppState>();
    for id in state.services.running_ids().iter().rev() {
        state.services.stop(id);
    }
    services_changed(app);
}

#[tauri::command]
fn services_status(state: State<AppState>) -> Vec<services::Status> {
    let cfg = state.config.lock().unwrap().clone();
    state.services.status(&cfg)
}

/// Iniciar, parar ou reiniciar um serviço (fora da thread da interface).
#[tauri::command]
async fn service_action(app: AppHandle, id: String, action: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let cfg = state.config.lock().unwrap().clone();
        let result = match action.as_str() {
            "start" => state.services.start(&cfg, &layout(), &id),
            "stop" => {
                state.services.stop(&id);
                Ok(())
            }
            "restart" => {
                state.services.stop(&id);
                state.services.start(&cfg, &layout(), &id)
            }
            other => Err(format!("Ação desconhecida: {other}")),
        };
        services_changed(&app);
        result
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn start_all_services(app: AppHandle) -> Result<(), String> {
    let errors = tauri::async_runtime::spawn_blocking(move || start_all(&app)).await.map_err(|e| e.to_string())?;
    if errors.is_empty() { Ok(()) } else { Err(errors.join("\n\n")) }
}

#[tauri::command]
async fn stop_all_services(app: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || stop_all(&app)).await.map_err(|e| e.to_string())
}

#[tauri::command]
fn open_terminal(state: State<AppState>) -> Result<(), String> {
    let cfg = state.config.lock().unwrap().clone();
    services::open_terminal(&cfg, &layout())
}

/* ---------- Atualização automática (Etapa 10) ---------- */

/// Procura uma versão nova no GitHub Releases (assinada). Com `install`,
/// para os serviços, instala e reinicia o JJPHPMY.
#[tauri::command]
async fn check_update(app: AppHandle, install: bool) -> Result<String, String> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|e| format!("Não foi possível verificar atualizações: {e}"))?;
    let Some(update) = update else {
        return Ok("Nenhuma atualização disponível.".into());
    };
    if !install {
        return Ok(format!("A versão {} do JJPHPMY está disponível.", update.version));
    }
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || stop_all(&handle)).await.map_err(|e| e.to_string())?;
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| format!("Não foi possível instalar a atualização: {e}"))?;
    app.restart();
}

/// QR Code PIX da tela Sobre ("Contribua com o projeto").
#[tauri::command]
fn pix_qr(amount_cents: u32) -> Result<pix::PixQr, String> {
    pix::qr(amount_cents)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let config = Config::load();
    let start_hidden = config.settings.run_minimized || std::env::args().any(|a| a == "--minimized");

    tauri::Builder::default()
        // Deve ser o primeiro plugin: uma segunda execução só reabre a janela.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| tray::show_window(app, None)))
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec!["--minimized"])))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(AppState { config: Mutex::new(config), catalog: Catalog::embedded(), services: Default::default(), watcher: Mutex::new(None) })
        .setup(move |app| {
            for dir in ["bin", "etc", "data", "www", "logs", "backup", "tmp", "config"] {
                let _ = std::fs::create_dir_all(base_path(&[dir]));
            }
            tray::create(app.handle())?;
            // Corrige arquivos e PATH se algo mudou com o app fechado (pasta apagada, por exemplo).
            // Depois, se "Iniciar tudo automaticamente" estiver ligado, inicia os serviços.
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                refresh(&handle, &handle.state::<AppState>());
                services_changed(&handle);
                let settings = handle.state::<AppState>().config.lock().unwrap().settings.clone();
                // "Atualização automática": avisa quando há versão nova (instalar fica com o usuário,
                // porque a instalação para os serviços).
                if settings.auto_update {
                    let h = handle.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Ok(msg) = check_update(h.clone(), false).await {
                            if msg.contains("disponível.") && !msg.starts_with("Nenhuma") {
                                toast(&h, &[format!("{msg} Use \"Verificar atualização agora\" em Configurações.")]);
                            }
                        }
                    });
                }
                let auto = settings.auto_start_all;
                if auto {
                    let errors = start_all(&handle);
                    toast(&handle, &errors);
                }
            });
            // Backup automático: confere a cada minuto se já passou o intervalo.
            // Só roda com algum banco ligado (sem banco, não há o que salvar).
            let handle = app.handle().clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_secs(60));
                let state = handle.state::<AppState>();
                let settings = state.config.lock().unwrap().settings.clone();
                let db_running = ["mysql", "postgresql", "mongodb"].iter().any(|id| state.services.is_running(id));
                if settings.auto_backup && db_running && backup::due(&layout(), settings.backup_interval_hours) {
                    match do_backup(&handle) {
                        Ok(r) if !r.skipped.is_empty() => toast(&handle, &[backup_message(&r)]),
                        Ok(_) => {}
                        Err(e) => toast(&handle, &[format!("Backup automático falhou: {e}")]),
                    }
                }
            });
            // Vigia: avisa quando um serviço para sozinho.
            let handle = app.handle().clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_secs(2));
                let state = handle.state::<AppState>();
                if state.services.reap() {
                    let cfg = state.config.lock().unwrap().clone();
                    let msgs: Vec<String> =
                        state.services.status(&cfg).into_iter().filter(|s| s.state == "error").filter_map(|s| s.message).collect();
                    toast(&handle, &msgs);
                    services_changed(&handle);
                }
            });
            watch_www(app.handle());
            if !start_hidden {
                tray::show_window(app.handle(), None);
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Fechar a janela só esconde; para sair use "Sair" no menu da bandeja.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_settings,
            save_services,
            get_catalog,
            fetch_versions,
            resolve_selection,
            install_packages,
            set_active_version,
            remove_installed,
            regenerate_configs,
            services_status,
            service_action,
            start_all_services,
            stop_all_services,
            open_terminal,
            list_sites,
            backup_now,
            backup_info,
            backup_list,
            backup_restore,
            laragon_detect,
            laragon_import,
            check_update,
            pix_qr,
        ])
        .run(tauri::generate_context!())
        .expect("erro ao iniciar o jjphpmy");
}
