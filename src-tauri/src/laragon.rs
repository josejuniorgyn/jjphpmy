//! Etapa 9: importar do Laragon (`C:\laragon`) ou do XAMPP (`C:\xampp`), ou
//! de outra pasta escolhida pelo usuário. Só lê a origem; nunca apaga nem
//! altera nada lá.
//!
//! - **www** (Laragon) / **htdocs** (XAMPP): copia as pastas para o Document
//!   Root sem sobrescrever arquivos que já existam (robocopy). Do XAMPP, as
//!   páginas de exemplo dele (dashboard, img...) ficam de fora.
//! - **XAMPP usa MariaDB:** a pasta de dados não serve no MySQL, então os
//!   bancos sempre vão por dump e importação.
//! - **MySQL, mesma versão maior.menor** (ex.: 8.4 e 8.4): copia a pasta de
//!   dados inteira (mantém usuários e senhas). A pasta antiga do JJPHPMY é
//!   renomeada para `*.antes-laragon-{data}`, nunca apagada.
//! - **MySQL, versões diferentes:** liga o mysqld do Laragon numa porta
//!   temporária, faz `mysqldump` dos bancos do usuário, desliga e importa no
//!   MySQL do JJPHPMY (pasta de dados do Laragon não é copiada).

use crate::confgen::{self, Layout};
use crate::config::{Config, InstalledPackage};
use crate::installer::hidden;
use crate::services::{self, Job, Manager};
use serde::Serialize;
use std::fs::{self, File};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Lugares padrão procurados ao abrir a aba Importar: (tipo, pasta).
pub const DEFAULT_ROOTS: &[(&str, &str)] = &[("laragon", r"C:\laragon"), ("xampp", r"C:\xampp")];
/// Itens de exemplo do XAMPP em htdocs que não são do usuário.
const XAMPP_SAMPLES_DIRS: &[&str] = &["dashboard", "img", "webalizer", "xampp"];
const XAMPP_SAMPLES_FILES: &[&str] = &["applications.html", "bitnami.css"];
/// Porta do mysqld temporário da origem durante o dump.
const TEMP_PORT: u16 = 13399;
const SYSTEM_DBS: &[&str] = &["mysql", "sys", "information_schema", "performance_schema"];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaragonMysql {
    pub version: String,
    /// Pasta da instalação (contém bin\mysqld.exe).
    pub dir: String,
    pub data: String,
    pub data_mb: u64,
    /// MariaDB (XAMPP): a pasta de dados não pode ir para o MySQL.
    pub mariadb: bool,
    /// my.ini da origem, usado ao ligar o banco temporariamente.
    pub ini: Option<String>,
    /// "Laragon" ou "XAMPP", para as mensagens.
    pub origin: String,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Detected {
    pub found: bool,
    /// "laragon" | "xampp" (vazio se não encontrado em pasta escolhida).
    pub kind: String,
    /// "Laragon" | "XAMPP"
    pub name: String,
    pub root: String,
    /// Pasta dos sites: `www` (Laragon) ou `htdocs` (XAMPP).
    pub www: String,
    pub www_items: usize,
    pub mysql: Option<LaragonMysql>,
    /// Programas do Laragon abertos agora (precisam ser fechados antes).
    pub running: Vec<String>,
    pub has_postgresql: bool,
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).into_iter().flatten().flatten() {
            match e.metadata() {
                Ok(m) if m.is_dir() => stack.push(e.path()),
                Ok(m) => total += m.len(),
                Err(_) => {}
            }
        }
    }
    total
}

/// "mysql-8.4.3-winx64" -> "8.4.3"
fn version_from_dir(name: &str) -> Option<String> {
    let v = name.strip_prefix("mysql-")?.split('-').next()?;
    v.split('.').all(|p| p.parse::<u32>().is_ok()).then(|| v.to_string())
}

/// MySQL do Laragon com dados: a pasta `data\mysql-X.Y` mais nova que tenha
/// um `bin\mysql\mysql-X.Y.*` correspondente.
pub fn find_mysql(root: &Path) -> Option<LaragonMysql> {
    let mut found: Vec<(Vec<u64>, LaragonMysql)> = Vec::new();
    for data in fs::read_dir(root.join("data")).ok()?.flatten() {
        let name = data.file_name().to_string_lossy().to_string();
        let Some(mm) = name.strip_prefix("mysql-") else { continue };
        for bin in fs::read_dir(root.join("bin").join("mysql")).into_iter().flatten().flatten() {
            let bname = bin.file_name().to_string_lossy().to_string();
            let Some(version) = version_from_dir(&bname) else { continue };
            if confgen::major_minor(&version) == mm && bin.path().join("bin").join("mysqld.exe").exists() {
                let key = crate::versions::version_key(&version);
                found.push((
                    key,
                    LaragonMysql {
                        version,
                        dir: bin.path().display().to_string(),
                        data: data.path().display().to_string(),
                        data_mb: 0,
                        mariadb: false,
                        ini: Some(bin.path().join("my.ini").display().to_string()).filter(|i| Path::new(i).exists()),
                        origin: "Laragon".into(),
                    },
                ));
            }
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0));
    let mut m = found.into_iter().next()?.1;
    m.data_mb = dir_size(Path::new(&m.data)) / (1024 * 1024);
    Some(m)
}

/// Programas abertos a partir da pasta do Laragon.
pub fn running_programs(root: &Path) -> Vec<String> {
    let script = format!(
        "Get-Process | Where-Object {{ $_.Path -like '{}\\*' }} | ForEach-Object {{ $_.ProcessName + '.exe' }} | Sort-Object -Unique",
        root.display()
    );
    hidden(Command::new("powershell").args(["-NoProfile", "-Command", &script]))
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default()
}

/// "mysqld  Ver 10.4.32-MariaDB for Win64..." -> ("10.4.32", true)
pub fn parse_mysqld_version(text: &str) -> Option<(String, bool)> {
    let after = text.split("Ver ").nth(1)?;
    let token = after.split_whitespace().next()?;
    let version: String = token.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    (!version.is_empty()).then(|| (version, text.contains("MariaDB")))
}

/// Banco do XAMPP: `mysql\bin\mysqld.exe` com dados em `mysql\data`.
pub fn find_xampp_db(root: &Path) -> Option<LaragonMysql> {
    let dir = root.join("mysql");
    let mysqld = dir.join("bin").join("mysqld.exe");
    let data = dir.join("data");
    if !mysqld.exists() || !data.is_dir() {
        return None;
    }
    let out = hidden(Command::new(&mysqld).arg("--version")).output().ok()?;
    let (version, mariadb) = parse_mysqld_version(&String::from_utf8_lossy(&out.stdout))?;
    let ini = dir.join("bin").join("my.ini");
    Some(LaragonMysql {
        version,
        dir: dir.display().to_string(),
        data_mb: dir_size(&data) / (1024 * 1024),
        data: data.display().to_string(),
        mariadb,
        ini: ini.exists().then(|| ini.display().to_string()),
        origin: "XAMPP".into(),
    })
}

/// Tipo da pasta pela estrutura: Laragon (`laragon.exe` ou `www` + `bin\mysql`)
/// ou XAMPP (`xampp-control.exe` ou `htdocs` + `mysql\bin`).
pub fn kind_of(root: &Path) -> Option<&'static str> {
    if root.join("laragon.exe").exists() || (root.join("www").is_dir() && root.join("bin").join("mysql").is_dir()) {
        Some("laragon")
    } else if root.join("xampp-control.exe").exists()
        || (root.join("htdocs").is_dir() && root.join("mysql").join("bin").is_dir())
    {
        Some("xampp")
    } else {
        None
    }
}

/// Lê a origem. `expected` dá o nome usado na mensagem quando não encontrada.
pub fn detect(root: &Path, expected: Option<&str>) -> Detected {
    let name_of = |k: &str| if k == "xampp" { "XAMPP" } else { "Laragon" }.to_string();
    let Some(kind) = kind_of(root) else {
        return Detected {
            root: root.display().to_string(),
            kind: expected.unwrap_or_default().into(),
            name: expected.map(name_of).unwrap_or_else(|| "Laragon ou XAMPP".into()),
            ..Default::default()
        };
    };
    let www = root.join(if kind == "xampp" { "htdocs" } else { "www" });
    Detected {
        found: true,
        kind: kind.into(),
        name: name_of(kind),
        root: root.display().to_string(),
        www_items: fs::read_dir(&www).map(|d| d.count()).unwrap_or(0),
        www: www.display().to_string(),
        mysql: if kind == "xampp" { find_xampp_db(root) } else { find_mysql(root) },
        running: running_programs(root),
        has_postgresql: root.join("bin").join("postgresql").is_dir(),
    }
}

/// Copia os sites da origem para o Document Root, sem sobrescrever nada.
/// Do XAMPP, as páginas de exemplo dele ficam de fora.
pub fn copy_www(found: &Detected, dst: &Path) -> Result<String, String> {
    let src = Path::new(&found.www);
    if found.kind != "xampp" {
        return robocopy(src, dst, true);
    }
    let had_index = dst.join("index.php").exists();
    let mut cmd = Command::new("robocopy");
    cmd.arg(src).arg(dst).args(["/E", "/XC", "/XN", "/XO", "/R:1", "/W:1", "/NP", "/NFL", "/NDL", "/NJH", "/XD"]);
    cmd.args(XAMPP_SAMPLES_DIRS.iter().map(|d| src.join(d)));
    cmd.arg("/XF").args(XAMPP_SAMPLES_FILES);
    let out = hidden(&mut cmd).output().map_err(|e| format!("Não foi possível executar o robocopy: {e}"))?;
    if out.status.code().is_none_or(|c| c >= 8) {
        return Err(format!("A cópia de {} falhou (robocopy {:?}).", src.display(), out.status.code()));
    }
    // O index.php da raiz do XAMPP só redireciona para /dashboard/.
    let index = dst.join("index.php");
    if !had_index && fs::read_to_string(&index).is_ok_and(|t| t.contains("/dashboard/")) {
        let _ = fs::remove_file(index);
    }
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    Ok(text.lines().map(str::trim).filter(|l| l.contains(':')).take(4).collect::<Vec<_>>().join("\n"))
}

/* ---------- Cópia (robocopy) ---------- */

/// Copia `src` para `dst`. `keep_existing`: não sobrescreve arquivos que já
/// existam no destino. Devolve o resumo do robocopy.
pub fn robocopy(src: &Path, dst: &Path, keep_existing: bool) -> Result<String, String> {
    let mut cmd = Command::new("robocopy");
    cmd.arg(src).arg(dst).args(["/E", "/R:1", "/W:1", "/NP", "/NFL", "/NDL", "/NJH"]);
    if keep_existing {
        cmd.args(["/XC", "/XN", "/XO"]);
    }
    let out = hidden(&mut cmd).output().map_err(|e| format!("Não foi possível executar o robocopy: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let summary: Vec<&str> = text.lines().map(str::trim).filter(|l| l.contains(':') && !l.is_empty()).take(4).collect();
    // robocopy: 0 a 7 = sucesso (com ou sem arquivos copiados); 8 ou mais = erro.
    match out.status.code() {
        Some(c) if c < 8 => Ok(summary.join("\n")),
        code => Err(format!("A cópia de {} falhou (robocopy {code:?}):\n{}", src.display(), summary.join("\n"))),
    }
}

/* ---------- MySQL ---------- */

fn port_open(port: u16) -> bool {
    TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_millis(300)).is_ok()
}

fn stamp() -> String {
    crate::backup::stamp()
}

/// mysqld do Laragon numa porta temporária, só em 127.0.0.1, no Job Object.
fn start_laragon_mysql(m: &LaragonMysql, layout: &Layout) -> Result<(Child, Job), String> {
    if let Some((pid, prog)) = services::port_owner(TEMP_PORT) {
        return Err(format!("A porta temporária {TEMP_PORT} está em uso por {prog} (PID {pid})."));
    }
    let dir = Path::new(&m.dir);
    let log = layout.dir(&["logs", "mysql", "laragon-import.err"]);
    let _ = fs::create_dir_all(log.parent().unwrap_or(dir));
    let mut cmd = Command::new(dir.join("bin").join("mysqld.exe"));
    if let Some(ini) = &m.ini {
        cmd.arg(format!("--defaults-file={ini}")); // tem que ser o primeiro
    }
    cmd.arg(format!("--datadir={}", m.data))
        .arg(format!("--basedir={}", m.dir))
        .arg(format!("--port={TEMP_PORT}"))
        .arg("--bind-address=127.0.0.1");
    if !m.mariadb {
        cmd.arg("--mysqlx=OFF"); // opção só do MySQL
    }
    cmd
        .arg(format!("--log-error={}", log.display()))
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let job = Job::new()?;
    let mut child = hidden(&mut cmd).spawn().map_err(|e| format!("Não foi possível iniciar o banco do {}: {e}", m.origin))?;
    job.assign(&child);
    let deadline = Instant::now() + Duration::from_secs(90);
    while !port_open(TEMP_PORT) {
        if let Ok(Some(st)) = child.try_wait() {
            return Err(format!(
                "O banco do {} não iniciou ({st}). Ele está aberto em outro programa? Log:\n{}",
                m.origin,
                services::tail(&log, 6)
            ));
        }
        if Instant::now() > deadline {
            job.terminate();
            return Err(format!("O banco do {} não respondeu em 90 s.", m.origin));
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    Ok((child, job))
}

/// Desliga com calma (mysqladmin shutdown) para não estragar os dados do Laragon.
fn stop_laragon_mysql(m: &LaragonMysql, mut child: Child, job: Job, password: &str) {
    let admin = Path::new(&m.dir).join("bin").join("mysqladmin.exe");
    let _ = hidden(Command::new(admin).args(["-u", "root", "-h", "127.0.0.1", "-P", &TEMP_PORT.to_string(), "shutdown"]).env("MYSQL_PWD", password))
        .output();
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    job.terminate();
}

fn client_output(cmd: &mut Command, what: &str) -> Result<String, String> {
    let out = hidden(cmd).output().map_err(|e| format!("{what}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        let hint = if err.contains("Access denied") { " A senha do root da origem está correta?" } else { "" };
        Err(format!("{what} falhou: {}{hint}", err.lines().last().unwrap_or("").trim()))
    }
}

/// O mariadb-dump mais novo começa com `/*M!999999\- enable the sandbox mode */`,
/// que o cliente do MySQL não entende: tira essa linha antes de importar.
fn strip_mariadb_sandbox(dump: &Path) -> Result<(), String> {
    use std::io::{BufRead, BufReader, Write};
    let mut reader = BufReader::new(File::open(dump).map_err(|e| e.to_string())?);
    let mut first = String::new();
    reader.read_line(&mut first).map_err(|e| e.to_string())?;
    if !first.starts_with("/*M!999999") {
        return Ok(());
    }
    let tmp = dump.with_extension("sql.tmp");
    let mut out = File::create(&tmp).map_err(|e| e.to_string())?;
    std::io::copy(&mut reader, &mut out).map_err(|e| e.to_string())?;
    out.flush().map_err(|e| e.to_string())?;
    drop(out);
    fs::rename(&tmp, dump).map_err(|e| e.to_string())
}

/// Bancos do usuário (sem os do sistema).
pub fn user_databases(list: &str) -> Vec<String> {
    list.lines().map(str::trim).filter(|d| !d.is_empty() && !SYSTEM_DBS.contains(d)).map(String::from).collect()
}

fn active_mysql(cfg: &Config) -> Result<&InstalledPackage, String> {
    cfg.installed
        .iter()
        .find(|p| p.id == "mysql" && p.active)
        .ok_or_else(|| "Instale o MySQL no JJPHPMY primeiro (aba Pacotes). Com a versão 8.4, a pasta de dados é copiada direto.".into())
}

/// Importa os bancos do MySQL do Laragon. `log` recebe o andamento.
pub fn import_mysql(
    m: &LaragonMysql,
    cfg: &Config,
    layout: &Layout,
    manager: &Manager,
    password: &str,
    log: &dyn Fn(String),
) -> Result<String, String> {
    let jj = active_mysql(cfg)?;
    if !m.mariadb && confgen::major_minor(&jj.version) == confgen::major_minor(&m.version) {
        copy_data(m, jj, cfg, layout, manager, log)
    } else {
        dump_and_load(m, jj, cfg, layout, manager, password, log)
    }
}

/// Mesma versão maior.menor: copia a pasta de dados.
fn copy_data(
    m: &LaragonMysql,
    jj: &InstalledPackage,
    cfg: &Config,
    layout: &Layout,
    manager: &Manager,
    log: &dyn Fn(String),
) -> Result<String, String> {
    let dest = confgen::data_dir(cfg, jj).ok_or("Pasta de dados do MySQL desconhecida.")?;
    log(format!("MySQL {} e {}: copiando a pasta de dados ({} MB)...", m.version, jj.version, m.data_mb));
    manager.stop("mysql");
    let backup = PathBuf::from(format!("{}.antes-laragon-{}", dest.display(), stamp()));
    if dest.exists() {
        fs::rename(&dest, &backup).map_err(|e| format!("Não foi possível renomear {}: {e}", dest.display()))?;
        log(format!("A pasta anterior do JJPHPMY foi guardada em {}", backup.display()));
    }
    let restore = |why: String| -> String {
        let _ = fs::remove_dir_all(&dest);
        if backup.exists() {
            let _ = fs::rename(&backup, &dest);
        }
        format!("{why} A pasta de dados anterior do JJPHPMY foi restaurada.")
    };
    if let Err(e) = robocopy(Path::new(&m.data), &dest, false) {
        return Err(restore(e));
    }
    // Arquivos de log/pid do Laragon não servem aqui.
    for e in fs::read_dir(&dest).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_lowercase();
        if name.ends_with(".err") || name.ends_with(".pid") {
            let _ = fs::remove_file(e.path());
        }
    }
    log("Iniciando o MySQL do JJPHPMY com os dados do Laragon...".into());
    if let Err(e) = manager.start(cfg, layout, "mysql") {
        return Err(restore(format!("O MySQL não iniciou com os dados copiados: {e}")));
    }
    Ok(format!(
        "MySQL: pasta de dados copiada do Laragon (usuários e senhas mantidos). A cópia anterior está em {}.",
        backup.display()
    ))
}

/// Versões diferentes: dump no Laragon e importação no JJPHPMY.
fn dump_and_load(
    m: &LaragonMysql,
    jj: &InstalledPackage,
    cfg: &Config,
    layout: &Layout,
    manager: &Manager,
    password: &str,
    log: &dyn Fn(String),
) -> Result<String, String> {
    let lbin = Path::new(&m.dir).join("bin");
    let port = TEMP_PORT.to_string();
    let banco = if m.mariadb { "MariaDB" } else { "MySQL" };
    log(format!(
        "{banco} {} ({}) e MySQL {} (JJPHPMY): {}, usando dump e importação.",
        m.version,
        m.origin,
        jj.version,
        if m.mariadb { "a pasta de dados do MariaDB não serve no MySQL" } else { "versões diferentes" }
    ));
    log(format!("Iniciando o {banco} do {} temporariamente na porta {TEMP_PORT}...", m.origin));
    let (child, job) = start_laragon_mysql(m, layout)?;
    let dump = layout.dir(&["tmp", "laragon-mysql.sql"]);
    let result = (|| -> Result<Vec<String>, String> {
        let list = client_output(
            Command::new(lbin.join("mysql.exe"))
                .args(["-u", "root", "-h", "127.0.0.1", "-P", &port, "-N", "-e", "SHOW DATABASES"])
                .env("MYSQL_PWD", password),
            "Listar os bancos da origem",
        )?;
        let dbs = user_databases(&list);
        if dbs.is_empty() {
            return Ok(dbs);
        }
        log(format!("Exportando {} bancos: {}...", dbs.len(), dbs.join(", ")));
        let _ = fs::create_dir_all(dump.parent().unwrap_or(Path::new(".")));
        client_output(
            Command::new(lbin.join("mysqldump.exe"))
                .args(["-u", "root", "-h", "127.0.0.1", "-P", &port, "--max-allowed-packet=512M"])
                .args(["--single-transaction", "--routines", "--events", "--triggers", "--databases"])
                .args(&dbs)
                .arg(format!("--result-file={}", dump.display()))
                .env("MYSQL_PWD", password),
            "Exportar os bancos da origem (mysqldump)",
        )?;
        Ok(dbs)
    })();
    log(format!("Desligando o {banco} do {}...", m.origin));
    stop_laragon_mysql(m, child, job, password);
    let dbs = result?;
    if dbs.is_empty() {
        return Ok(format!("{banco}: o {} não tem bancos do usuário para importar.", m.origin));
    }
    strip_mariadb_sandbox(&dump)?;

    log("Importando no MySQL do JJPHPMY...".into());
    manager.start(cfg, layout, "mysql")?;
    let file = File::open(&dump).map_err(|e| format!("Não foi possível abrir {}: {e}", dump.display()))?;
    let jport = cfg.service("mysql").map(|s| s.port).unwrap_or(3306).to_string();
    client_output(
        Command::new(Path::new(&jj.path).join("bin").join("mysql.exe"))
            .args(["-u", "root", "-h", "127.0.0.1", "-P", &jport, "--max-allowed-packet=512M"])
            .stdin(file),
        "Importar no MySQL do JJPHPMY",
    )?;
    let _ = fs::remove_file(&dump);
    Ok(format!(
        "{banco}: {} bancos importados do {} ({}). Usuários e senhas não são copiados nesse modo; o root do JJPHPMY continua sem senha.",
        dbs.len(),
        m.origin,
        dbs.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_laragon_folder_names() {
        assert_eq!(version_from_dir("mysql-8.4.3-winx64").as_deref(), Some("8.4.3"));
        assert_eq!(version_from_dir("mysql-8.0.30-winx64").as_deref(), Some("8.0.30"));
        assert_eq!(version_from_dir("mariadb-10.6"), None);
        assert_eq!(version_from_dir("mysql-x"), None);
    }

    #[test]
    fn filters_system_databases() {
        let list = "information_schema\nloja\nmysql\nperformance_schema\nsys\nblog\n";
        assert_eq!(user_databases(list), ["loja", "blog"]);
    }

    #[test]
    fn finds_mysql_with_matching_data() {
        let root = std::env::temp_dir().join(format!("jjphpmy-lar-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for v in ["mysql-8.0.30-winx64", "mysql-8.4.3-winx64"] {
            fs::create_dir_all(root.join("bin").join("mysql").join(v).join("bin")).unwrap();
            fs::write(root.join("bin").join("mysql").join(v).join("bin").join("mysqld.exe"), "").unwrap();
        }
        fs::create_dir_all(root.join("data").join("mysql-8.4")).unwrap();
        fs::write(root.join("data").join("mysql-8.4").join("ibdata1"), vec![0u8; 2 * 1024 * 1024]).unwrap();
        let m = find_mysql(&root).unwrap();
        assert_eq!(m.version, "8.4.3");
        assert!(m.data.ends_with("mysql-8.4"));
        assert_eq!(m.data_mb, 2);
        let _ = fs::remove_dir_all(&root);
    }

    /// Laragon "de mentira" numa pasta temporária (o de verdade não é tocado):
    /// MySQL 8.4 com o banco `loja`, importado nos dois modos (cópia da pasta
    /// com JJPHPMY 8.4; dump com a versão mais nova) e cópia de www.
    /// `cargo test live_laragon_import -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_laragon_import() {
        use std::sync::Arc;
        let t = std::env::temp_dir().join("jjphpmy-live-lar");
        let _ = fs::remove_dir_all(&t);
        let catalog = crate::catalog::Catalog::embedded();
        let pkg = catalog.get("mysql").unwrap();
        let emit: crate::installer::Emit = Arc::new(|_| {});
        let list = tauri::async_runtime::block_on(crate::versions::versions_for(pkg, false)).versions;
        let v84 = list.iter().find(|v| v.version.starts_with("8.4.")).unwrap().version.clone();

        // JJPHPMY: MySQL 8.4 e o mais novo.
        let jj = Layout { base: t.join("jj") };
        let (bin, tmp) = (jj.dir(&["bin"]), jj.dir(&["tmp"]));
        let install = |v: Option<String>| {
            let p = tauri::async_runtime::block_on(crate::installer::install_in(emit.clone(), pkg, v, None, &bin, &tmp)).unwrap().unwrap();
            InstalledPackage { active: true, ..p }
        };
        let my84 = install(Some(v84.clone()));
        let newest = install(None);
        println!("MySQL do JJPHPMY: {} e {}", my84.version, newest.version);

        // Laragon falso: cópia do 8.4 em bin\mysql\mysql-X-winx64 e data\mysql-8.4 com o banco loja.
        let lar = t.join("laragon");
        let lar_dir = lar.join("bin").join("mysql").join(format!("mysql-{v84}-winx64"));
        robocopy(Path::new(&my84.path), &lar_dir, false).unwrap();
        let lar_data = lar.join("data").join("mysql-8.4");
        fs::create_dir_all(lar.join("data")).unwrap();
        let st = Command::new(lar_dir.join("bin").join("mysqld.exe"))
            .args(["--initialize-insecure", "--console"])
            .arg(format!("--basedir={}", lar_dir.display()))
            .arg(format!("--datadir={}", lar_data.display()))
            .output()
            .unwrap();
        assert!(st.status.success(), "{}", String::from_utf8_lossy(&st.stderr));
        let m = find_mysql(&lar).unwrap();
        let (child, job) = start_laragon_mysql(&m, &jj).unwrap();
        let ok = Command::new(lar_dir.join("bin").join("mysql.exe"))
            .args(["-u", "root", "-h", "127.0.0.1", "-P", "13399", "-e",
                "CREATE DATABASE loja; CREATE TABLE loja.produto (nome VARCHAR(50)); INSERT INTO loja.produto VALUES ('cafe-laragon');"])
            .status()
            .unwrap();
        assert!(ok.success());
        stop_laragon_mysql(&m, child, job, "");
        println!("Laragon falso: MySQL {} em {}", m.version, m.data);

        let query = |p: &InstalledPackage, port: &str| {
            let out = Command::new(Path::new(&p.path).join("bin").join("mysql.exe"))
                .args(["-u", "root", "-h", "127.0.0.1", "-P", port, "-N", "-e", "SELECT nome FROM loja.produto"])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let log = |s: String| println!("  | {s}");

        for (label, pkg_jj, port) in [("cópia da pasta", my84.clone(), 13310u16), ("dump e importação", newest.clone(), 13311)] {
            let mut cfg = Config::default();
            cfg.settings.document_root = jj.dir(&["www"]).display().to_string();
            cfg.settings.data_dir = jj.dir(&["data"]).display().to_string();
            cfg.services.iter_mut().find(|s| s.id == "mysql").unwrap().port = port;
            cfg.installed = vec![pkg_jj.clone()];
            confgen::generate(&cfg, &jj, true).unwrap();
            confgen::init_data(&cfg, &jj).unwrap();
            let mgr = Manager::default();
            println!("== {label} (JJPHPMY {})", pkg_jj.version);
            let summary = import_mysql(&m, &cfg, &jj, &mgr, "", &log).unwrap_or_else(|e| panic!("{e}"));
            println!("  {summary}");
            let got = query(&pkg_jj, &port.to_string());
            println!("  loja.produto no JJPHPMY: {got}");
            assert_eq!(got, "cafe-laragon");
            mgr.stop("mysql");
        }

        // www: copia o novo, não sobrescreve o que já existe.
        fs::create_dir_all(lar.join("www").join("site-a")).unwrap();
        fs::create_dir_all(lar.join("www").join("site-b")).unwrap();
        fs::write(lar.join("www").join("site-a").join("index.php"), "laragon-a").unwrap();
        fs::write(lar.join("www").join("site-b").join("index.php"), "laragon-b").unwrap();
        let dst = jj.dir(&["www"]);
        fs::create_dir_all(dst.join("site-b")).unwrap();
        fs::write(dst.join("site-b").join("index.php"), "meu-b").unwrap();
        println!("www:\n{}", robocopy(&lar.join("www"), &dst, true).unwrap());
        assert_eq!(fs::read_to_string(dst.join("site-a").join("index.php")).unwrap(), "laragon-a");
        assert_eq!(fs::read_to_string(dst.join("site-b").join("index.php")).unwrap(), "meu-b");
        assert!(lar_data.join("loja").is_dir(), "o Laragon continua intacto");
        let _ = fs::remove_dir_all(&t);
    }

    /// XAMPP "de mentira" com MariaDB 10.4.32 de verdade (o mesmo do XAMPP 8.2):
    /// banco `loja`, htdocs com um site e as páginas de exemplo do XAMPP,
    /// importado no MySQL 8.4 do JJPHPMY.
    /// `cargo test live_xampp_import -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_xampp_import() {
        use std::sync::Arc;
        let t = std::env::temp_dir().join("jjphpmy-live-xampp");
        let _ = fs::remove_dir_all(&t);
        fs::create_dir_all(&t).unwrap();
        let xampp = t.join("xampp");
        fs::create_dir_all(&xampp).unwrap();
        fs::write(xampp.join("xampp-control.exe"), "").unwrap();

        // MariaDB oficial em xampp\mysql.
        let zip = t.join("mariadb.zip");
        let ok = Command::new("curl")
            .args(["-sSL", "-A", "jjphpmy-test", "-o"])
            .arg(&zip)
            .arg("https://archive.mariadb.org/mariadb-10.4.32/winx64-packages/mariadb-10.4.32-winx64.zip")
            .status()
            .unwrap();
        assert!(ok.success());
        assert!(Command::new("tar").arg("-xf").arg(&zip).arg("-C").arg(&t).status().unwrap().success());
        fs::rename(t.join("mariadb-10.4.32-winx64"), xampp.join("mysql")).unwrap();
        let mbin = xampp.join("mysql").join("bin");
        let init = Command::new(mbin.join("mysql_install_db.exe"))
            .arg(format!("--datadir={}", xampp.join("mysql").join("data").display()))
            .output()
            .unwrap();
        assert!(init.status.success(), "{}", String::from_utf8_lossy(&init.stderr));

        // htdocs: um site do usuário e as páginas de exemplo do XAMPP.
        let htdocs = xampp.join("htdocs");
        fs::create_dir_all(htdocs.join("loja")).unwrap();
        fs::create_dir_all(htdocs.join("dashboard")).unwrap();
        fs::write(htdocs.join("loja").join("index.php"), "loja-xampp").unwrap();
        fs::write(htdocs.join("dashboard").join("index.html"), "xampp").unwrap();
        fs::write(htdocs.join("index.php"), "<?php header('Location: '.$uri.'/dashboard/');").unwrap();
        fs::write(htdocs.join("bitnami.css"), "x").unwrap();

        let found = detect(&xampp, None);
        let m = found.mysql.clone().expect("MariaDB do XAMPP");
        println!("detectado: {} {} | {} {} | {} itens em htdocs", found.name, found.kind, if m.mariadb { "MariaDB" } else { "MySQL" }, m.version, found.www_items);
        assert!(m.mariadb && found.kind == "xampp");

        let layout = Layout { base: t.join("jj") };
        let (child, job) = start_laragon_mysql(&m, &layout).unwrap_or_else(|e| panic!("{e}"));
        let ok = Command::new(mbin.join("mysql.exe"))
            .args(["-u", "root", "-h", "127.0.0.1", "-P", "13399", "-e",
                "CREATE DATABASE loja; CREATE TABLE loja.produto (nome VARCHAR(50)) DEFAULT CHARSET=utf8mb4; INSERT INTO loja.produto VALUES ('cafe-xampp');"])
            .status()
            .unwrap();
        assert!(ok.success());
        stop_laragon_mysql(&m, child, job, "");

        // JJPHPMY com MySQL 8.4.
        let catalog = crate::catalog::Catalog::embedded();
        let pkg = catalog.get("mysql").unwrap();
        let list = tauri::async_runtime::block_on(crate::versions::versions_for(pkg, false)).versions;
        let v84 = list.iter().find(|v| v.version.starts_with("8.4.")).unwrap().version.clone();
        let emit: crate::installer::Emit = Arc::new(|_| {});
        let (bin, tmp) = (layout.dir(&["bin"]), layout.dir(&["tmp"]));
        let jj = tauri::async_runtime::block_on(crate::installer::install_in(emit, pkg, Some(v84), None, &bin, &tmp)).unwrap().unwrap();
        let jj = InstalledPackage { active: true, ..jj };
        let mut cfg = Config::default();
        cfg.settings.document_root = layout.dir(&["www"]).display().to_string();
        cfg.settings.data_dir = layout.dir(&["data"]).display().to_string();
        cfg.services.iter_mut().find(|s| s.id == "mysql").unwrap().port = 13312;
        cfg.installed = vec![jj.clone()];
        confgen::generate(&cfg, &layout, true).unwrap();
        confgen::init_data(&cfg, &layout).unwrap();

        let mgr = Manager::default();
        let log = |s: String| println!("  | {s}");
        let summary = import_mysql(&m, &cfg, &layout, &mgr, "", &log).unwrap_or_else(|e| panic!("{e}"));
        println!("  {summary}");
        let out = Command::new(Path::new(&jj.path).join("bin").join("mysql.exe"))
            .args(["-u", "root", "-h", "127.0.0.1", "-P", "13312", "-N", "-e", "SELECT nome FROM loja.produto"])
            .output()
            .unwrap();
        let got = String::from_utf8_lossy(&out.stdout).trim().to_string();
        println!("  loja.produto no MySQL do JJPHPMY: {got}");
        assert_eq!(got, "cafe-xampp");
        mgr.stop("mysql");

        let www = layout.dir(&["www"]);
        fs::create_dir_all(&www).unwrap();
        println!("htdocs:\n{}", copy_www(&found, &www).unwrap());
        assert_eq!(fs::read_to_string(www.join("loja").join("index.php")).unwrap(), "loja-xampp");
        assert!(!www.join("dashboard").exists(), "páginas de exemplo do XAMPP ficam de fora");
        assert!(!www.join("bitnami.css").exists());
        assert!(!www.join("index.php").exists(), "index.php que redireciona para /dashboard/ não é copiado");
        assert!(xampp.join("mysql").join("data").join("loja").is_dir(), "o XAMPP continua intacto");
        let _ = fs::remove_dir_all(&t);
    }

    /// Só lê o Laragon deste PC e mostra o que encontrou (não altera nada).
    /// `cargo test real_laragon -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_laragon_detection() {
        for (kind, root) in DEFAULT_ROOTS {
            println!("{:#?}", detect(Path::new(root), Some(kind)));
        }
    }

    #[test]
    fn parses_mysqld_version_lines() {
        assert_eq!(
            parse_mysqld_version("mysqld  Ver 10.4.32-MariaDB for Win64 on AMD64 (mariadb.org binary distribution)"),
            Some(("10.4.32".into(), true))
        );
        assert_eq!(
            parse_mysqld_version(r"C:\x\mysqld.exe  Ver 8.4.3 for Win64 on x86_64 (MySQL Community Server - GPL)"),
            Some(("8.4.3".into(), false))
        );
        assert_eq!(parse_mysqld_version("lixo"), None);
    }

    #[test]
    fn strips_mariadb_sandbox_line() {
        let f = std::env::temp_dir().join(format!("jjphpmy-sandbox-{}.sql", std::process::id()));
        fs::write(&f, "/*M!999999\\- enable the sandbox mode */ \nCREATE DATABASE a;\n").unwrap();
        strip_mariadb_sandbox(&f).unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "CREATE DATABASE a;\n");
        strip_mariadb_sandbox(&f).unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "CREATE DATABASE a;\n");
        let _ = fs::remove_file(&f);
    }

    #[test]
    fn identifies_folder_kind() {
        let base = std::env::temp_dir().join(format!("jjphpmy-kind-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("x").join("htdocs")).unwrap();
        fs::create_dir_all(base.join("x").join("mysql").join("bin")).unwrap();
        fs::create_dir_all(base.join("l").join("www")).unwrap();
        fs::create_dir_all(base.join("l").join("bin").join("mysql")).unwrap();
        assert_eq!(kind_of(&base.join("x")), Some("xampp"));
        assert_eq!(kind_of(&base.join("l")), Some("laragon"));
        assert_eq!(kind_of(&base), None);
        let d = detect(&base.join("nada"), Some("xampp"));
        assert!(!d.found);
        assert_eq!(d.name, "XAMPP");
        let _ = fs::remove_dir_all(&base);
    }
}
