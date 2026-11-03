//! Etapa 5: gerenciador de processos. Cada serviço roda como processo filho
//! (nunca como serviço do Windows), sem janela de console, dentro de um Job
//! Object com `KILL_ON_JOB_CLOSE`: se o JJPHPMY fechar ou travar, o Windows
//! encerra tudo que ele iniciou, inclusive os processos netos (Apache).

use crate::confgen::{self, Layout, PHP_CGI_PORT};
use crate::config::{Config, InstalledPackage};
use crate::envpath;
use crate::installer::hidden;
use serde::Serialize;
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Ordem de início (bancos antes dos servidores web); a parada é na ordem inversa.
pub const ORDER: &[&str] =
    &["mysql", "postgresql", "mongodb", "redis", "memcached", "mailpit", "php-cgi", "apache", "nginx"];

/// Porta da interface web do Mailpit (a SMTP é a configurada na tela).
pub const MAILPIT_WEB_PORT: u16 = 8025;

pub fn display_name(id: &str) -> &str {
    match id {
        "apache" => "Apache",
        "nginx" => "Nginx",
        "php-cgi" => "PHP (php-cgi)",
        "mysql" => "MySQL",
        "postgresql" => "PostgreSQL",
        "mongodb" => "MongoDB",
        "redis" => "Redis",
        "memcached" => "Memcached",
        "mailpit" => "Mailpit",
        other => other,
    }
}

/* ---------- Job Object ---------- */

#[cfg(windows)]
mod job {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    /// Grupo de processos de um serviço: fechar o handle encerra todos.
    pub struct Job(HANDLE);

    // O handle é só um número do kernel; usá-lo de outra thread é seguro.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub fn new() -> Result<Self, String> {
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return Err("Não foi possível criar o Job Object do Windows.".into());
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                Ok(Job(handle))
            }
        }

        pub fn assign(&self, child: &std::process::Child) {
            use std::os::windows::io::AsRawHandle;
            unsafe {
                AssignProcessToJobObject(self.0, child.as_raw_handle() as HANDLE);
            }
        }

        pub fn terminate(&self) {
            unsafe {
                TerminateJobObject(self.0, 1);
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

#[cfg(not(windows))]
mod job {
    pub struct Job;
    impl Job {
        pub fn new() -> Result<Self, String> {
            Ok(Job)
        }
        pub fn assign(&self, _: &std::process::Child) {}
        pub fn terminate(&self) {}
    }
}

pub use job::Job;

/* ---------- Como iniciar cada serviço ---------- */

#[derive(Debug, Clone)]
pub struct Spec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    /// Portas que precisam estar livres; a primeira indica que o serviço está pronto.
    pub ports: Vec<u16>,
    /// Log principal do serviço (mostrado quando ele falha).
    pub log: PathBuf,
    /// Comando de parada suave; sem ele, o processo é encerrado.
    pub stop: Option<(PathBuf, Vec<String>)>,
    pub ready_secs: u64,
}

fn active<'a>(cfg: &'a Config, id: &str, variant: Option<&str>) -> Option<&'a InstalledPackage> {
    cfg.installed
        .iter()
        .filter(|p| p.id == id && p.active && Path::new(&p.path).is_dir())
        .find(|p| variant.is_none() || p.variant.as_deref() == variant)
}

fn port_of(cfg: &Config, id: &str) -> u16 {
    cfg.service(id).map(|s| s.port).unwrap_or(0)
}

fn s(p: impl AsRef<Path>) -> String {
    p.as_ref().display().to_string()
}

/// Exige que o arquivo de configuração gerado na Etapa 4 exista.
fn need(file: PathBuf) -> Result<PathBuf, String> {
    if file.exists() {
        Ok(file)
    } else {
        Err(format!(
            "Arquivo de configuração ausente: {}. Use \"Recriar arquivos de configuração\" na aba Serviços e Portas.",
            file.display()
        ))
    }
}

pub fn spec(cfg: &Config, layout: &Layout, id: &str) -> Result<Spec, String> {
    let not_installed = || format!("{} não está instalado. Instale na aba Pacotes.", display_name(id));
    let etc = |parts: &[&str]| layout.dir(&[&["etc"], parts].concat());
    let logs = |file: &str| layout.dir(&["logs", if id == "php-cgi" { "php" } else { id }, file]);
    let pkg_id = if id == "php-cgi" { "php" } else { id };
    let variant = (id == "php-cgi").then_some("nts");
    let p = active(cfg, pkg_id, variant).ok_or_else(not_installed)?;
    let dir = PathBuf::from(&p.path);
    let console = logs(&format!("{id}-console.log"));

    let mut spec = Spec {
        program: PathBuf::new(),
        args: Vec::new(),
        cwd: dir.clone(),
        env: Vec::new(),
        ports: vec![port_of(cfg, id)],
        log: console.clone(),
        stop: None,
        ready_secs: 15,
    };
    match id {
        "apache" => {
            spec.ports.extend(crate::ssl::port(cfg, "apache"));
            spec.program = dir.join("bin").join("httpd.exe");
            spec.args = vec!["-f".into(), s(need(etc(&["apache", "httpd.conf"]))?)];
            spec.log = logs("error.log");
        }
        "nginx" => {
            spec.ports.extend(crate::ssl::port(cfg, "nginx"));
            let conf = need(etc(&["nginx", "nginx.conf"]))?;
            let prefix = format!("{}/", s(&dir).replace('\\', "/"));
            spec.program = dir.join("nginx.exe");
            spec.args = vec!["-p".into(), prefix.clone(), "-c".into(), s(&conf)];
            spec.stop = Some((spec.program.clone(), vec!["-p".into(), prefix, "-c".into(), s(&conf), "-s".into(), "quit".into()]));
            spec.log = logs("error.log");
        }
        "php-cgi" => {
            let both = active(cfg, "php", Some("ts")).is_some();
            let ini = need(etc(&["php", if both { "php-cgi.ini" } else { "php.ini" }]))?;
            spec.program = dir.join("php-cgi.exe");
            spec.args = vec!["-b".into(), format!("127.0.0.1:{PHP_CGI_PORT}"), "-c".into(), s(ini)];
            spec.env.push(("PHP_FCGI_MAX_REQUESTS".into(), "0".into()));
            spec.ports = vec![PHP_CGI_PORT];
            spec.log = logs("php_errors.log");
        }
        "mysql" => {
            let ini = need(etc(&["mysql", "my.ini"]))?;
            spec.program = dir.join("bin").join("mysqld.exe");
            spec.args = vec![format!("--defaults-file={}", s(&ini))];
            spec.stop = Some((
                dir.join("bin").join("mysqladmin.exe"),
                vec![
                    "-u".into(),
                    "root".into(),
                    "-h".into(),
                    "127.0.0.1".into(),
                    "-P".into(),
                    port_of(cfg, "mysql").to_string(),
                    "shutdown".into(),
                ],
            ));
            spec.log = logs("mysql.err");
            spec.ready_secs = 60;
        }
        "postgresql" => {
            let data = confgen::data_dir(cfg, p).unwrap_or_default();
            need(etc(&["postgresql", "postgresql.conf"]))?;
            spec.program = dir.join("bin").join("postgres.exe");
            spec.args = vec!["-D".into(), s(&data)];
            spec.stop = Some((
                dir.join("bin").join("pg_ctl.exe"),
                vec!["stop".into(), "-D".into(), s(&data), "-m".into(), "fast".into(), "-w".into()],
            ));
            spec.ready_secs = 30;
        }
        "mongodb" => {
            spec.program = dir.join("bin").join("mongod.exe");
            spec.args = vec!["--config".into(), s(need(etc(&["mongodb", "mongod.cfg"]))?)];
            spec.log = logs("mongod.log");
            spec.ready_secs = 30;
        }
        "redis" => {
            let conf = need(etc(&["redis", "redis.conf"]))?;
            spec.program = dir.join("redis-server.exe");
            spec.args = vec![confgen::msys(&conf)];
            let cli = dir.join("redis-cli.exe");
            if cli.exists() {
                spec.stop = Some((cli, vec!["-p".into(), port_of(cfg, "redis").to_string(), "shutdown".into()]));
            }
            spec.log = logs("redis.log");
        }
        "memcached" => {
            spec.program = dir.join("memcached.exe");
            spec.args = vec!["-p".into(), port_of(cfg, id).to_string(), "-l".into(), "127.0.0.1".into(), "-m".into(), "64".into()];
        }
        "mailpit" => {
            let data = PathBuf::from(&cfg.settings.data_dir).join("mailpit");
            let _ = fs::create_dir_all(&data);
            spec.program = dir.join("mailpit.exe");
            spec.args = vec![
                "--smtp".into(),
                format!("127.0.0.1:{}", port_of(cfg, id)),
                "--listen".into(),
                format!("127.0.0.1:{MAILPIT_WEB_PORT}"),
                "--database".into(),
                s(data.join("mailpit.db")),
            ];
            spec.ports.push(MAILPIT_WEB_PORT);
        }
        _ => return Err(format!("Serviço desconhecido: {id}")),
    }
    if !spec.program.exists() {
        return Err(format!("{} não encontrado. Reinstale o pacote.", spec.program.display()));
    }
    Ok(spec)
}

/// PATH dos processos filhos e do Terminal: versões ativas antes do PATH atual.
pub fn child_env(cfg: &Config, layout: &Layout) -> Vec<(String, String)> {
    let mut dirs: Vec<String> = envpath::wanted_dirs(&cfg.installed).into_iter().filter(|d| Path::new(d).is_dir()).collect();
    dirs.push(std::env::var("PATH").unwrap_or_default());
    let mut env = vec![("PATH".to_string(), dirs.join(";"))];
    let php_dir = layout.dir(&["etc", "php"]);
    if php_dir.join("php.ini").exists() {
        env.push(("PHPRC".into(), s(php_dir)));
    }
    env
}

/* ---------- Portas ---------- */

/// Quem está escutando na porta: (PID, nome do programa). Usa o `netstat`,
/// reconhecendo a escuta pelo endereço remoto zerado (vale em qualquer idioma).
pub fn port_owner(port: u16) -> Option<(u32, String)> {
    let out = hidden(Command::new("netstat").args(["-ano", "-p", "TCP"])).output().ok()?;
    let out6 = hidden(Command::new("netstat").args(["-ano", "-p", "TCPv6"])).output().ok();
    let text = String::from_utf8_lossy(&out.stdout).to_string()
        + &out6.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
    let pid = parse_listener(&text, port)?;
    Some((pid, process_name(pid).unwrap_or_else(|| "programa desconhecido".into())))
}

/// Acha o PID que escuta em `port` na saída do `netstat -ano`.
pub fn parse_listener(netstat: &str, port: u16) -> Option<u32> {
    let suffix = format!(":{port}");
    netstat.lines().find_map(|line| {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 5 || !cols[0].starts_with("TCP") {
            return None;
        }
        let listening = cols[2] == "0.0.0.0:0" || cols[2] == "[::]:0";
        (listening && cols[1].ends_with(&suffix)).then(|| cols[4].parse().ok()).flatten()
    })
}

fn process_name(pid: u32) -> Option<String> {
    let out = hidden(Command::new("tasklist").args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"]))
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let name = text.split(',').next()?.trim().trim_matches('"');
    (!name.is_empty() && name.contains('.')).then(|| name.to_string())
}

fn port_open(port: u16) -> bool {
    TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_millis(300)).is_ok()
}

/* ---------- Logs ---------- */

/// Últimas linhas de um arquivo de log (sem ler o arquivo inteiro).
pub fn tail(path: &Path, lines: usize) -> String {
    let Ok(mut f) = File::open(path) else { return String::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(8192)));
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf);
    let all: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// Log do console (stdout/stderr) com cabeçalho; zera se passar de 5 MB.
fn open_console_log(path: &Path, spec: &Spec) -> Result<File, String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("Não foi possível criar {}: {e}", dir.display()))?;
    }
    if fs::metadata(path).map(|m| m.len() > 5 * 1024 * 1024).unwrap_or(false) {
        let _ = fs::remove_file(path);
    }
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("Não foi possível abrir {}: {e}", path.display()))?;
    let _ = writeln!(f, "\n===== JJPHPMY: {} {} =====", spec.program.display(), spec.args.join(" "));
    Ok(f)
}

/* ---------- Gerenciador ---------- */

struct Running {
    child: Child,
    job: Job,
    stop: Option<(PathBuf, Vec<String>)>,
    log: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub id: String,
    /// "running" | "stopped" | "error"
    pub state: String,
    pub pid: Option<u32>,
    pub installed: bool,
    pub version: Option<String>,
    pub message: Option<String>,
}

#[derive(Default)]
pub struct Manager {
    running: Mutex<HashMap<String, Running>>,
    /// Último erro de cada serviço (falha ao iniciar ou parada inesperada).
    errors: Mutex<HashMap<String, String>>,
}

impl Manager {
    pub fn is_running(&self, id: &str) -> bool {
        self.running.lock().unwrap().contains_key(id)
    }

    pub fn running_ids(&self) -> Vec<String> {
        let running = self.running.lock().unwrap();
        ORDER.iter().filter(|id| running.contains_key(**id)).map(|s| s.to_string()).collect()
    }

    /// Inicia e espera a porta responder. Com Nginx, inicia também o php-cgi.
    pub fn start(&self, cfg: &Config, layout: &Layout, id: &str) -> Result<(), String> {
        if self.is_running(id) {
            return Ok(());
        }
        if id == "nginx" && active(cfg, "php", Some("nts")).is_some() {
            self.start(cfg, layout, "php-cgi")?;
        }
        let result = self.spawn_and_wait(cfg, layout, id);
        let mut errors = self.errors.lock().unwrap();
        match &result {
            Ok(()) => errors.remove(id),
            Err(e) => errors.insert(id.to_string(), e.clone()),
        };
        result
    }

    fn spawn_and_wait(&self, cfg: &Config, layout: &Layout, id: &str) -> Result<(), String> {
        let name = display_name(id);
        let spec = spec(cfg, layout, id)?;
        for &port in &spec.ports {
            if let Some((pid, program)) = port_owner(port) {
                return Err(format!(
                    "{name}: a porta {port} já está em uso por {program} (PID {pid}). \
                     Feche esse programa (o Laragon, por exemplo) ou troque a porta em Serviços e Portas."
                ));
            }
        }

        let console = layout.dir(&["logs", if id == "php-cgi" { "php" } else { id }, &format!("{id}-console.log")]);
        let out = open_console_log(&console, &spec)?;
        let err = out.try_clone().map_err(|e| e.to_string())?;
        let mut cmd = Command::new(&spec.program);
        cmd.args(&spec.args)
            .current_dir(&spec.cwd)
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .envs(child_env(cfg, layout))
            .envs(spec.env.iter().cloned());
        let job = Job::new()?;
        let child = hidden(&mut cmd).spawn().map_err(|e| format!("Não foi possível iniciar o {name}: {e}"))?;
        job.assign(&child);
        self.running
            .lock()
            .unwrap()
            .insert(id.to_string(), Running { child, job, stop: spec.stop.clone(), log: spec.log.clone() });

        // Pronto quando a porta responde; falha se o processo terminar antes.
        let deadline = Instant::now() + Duration::from_secs(spec.ready_secs);
        loop {
            let exited = {
                let mut running = self.running.lock().unwrap();
                match running.get_mut(id).map(|r| r.child.try_wait()) {
                    Some(Ok(Some(status))) => {
                        running.remove(id);
                        Some(status)
                    }
                    Some(_) => None,
                    None => return Err(format!("{name} foi parado durante a inicialização.")),
                }
            };
            if let Some(status) = exited {
                return Err(failure(name, &format!("parou ao iniciar ({status})"), &spec.log, &console));
            }
            if port_open(spec.ports[0]) {
                return Ok(());
            }
            if Instant::now() > deadline {
                self.stop(id);
                return Err(failure(
                    name,
                    &format!("não respondeu na porta {} em {} s", spec.ports[0], spec.ready_secs),
                    &spec.log,
                    &console,
                ));
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    /// Para o serviço: comando suave (se houver) e, depois de 10 s, encerra o grupo.
    /// Parar o Nginx para também o php-cgi.
    pub fn stop(&self, id: &str) {
        let taken = self.running.lock().unwrap().remove(id);
        if let Some(mut r) = taken {
            if let Some((program, args)) = &r.stop {
                if let Ok(mut c) = hidden(Command::new(program).args(args).stdout(Stdio::null()).stderr(Stdio::null())).spawn() {
                    wait_for(&mut c, Duration::from_secs(10));
                }
                wait_for(&mut r.child, Duration::from_secs(10));
            }
            r.job.terminate();
            let _ = r.child.wait();
        }
        self.errors.lock().unwrap().remove(id);
        if id == "nginx" {
            self.stop("php-cgi");
        }
    }

    /// Verifica processos que pararam sozinhos. Devolve true se algo mudou.
    pub fn reap(&self) -> bool {
        let mut died = Vec::new();
        {
            let mut running = self.running.lock().unwrap();
            running.retain(|id, r| match r.child.try_wait() {
                Ok(Some(status)) => {
                    died.push((id.clone(), status.to_string(), r.log.clone()));
                    false
                }
                _ => true,
            });
        }
        let mut errors = self.errors.lock().unwrap();
        for (id, status, log) in &died {
            let last = tail(log, 5);
            errors.insert(
                id.clone(),
                format!("{} parou sozinho ({status}).{}", display_name(id), if last.is_empty() { String::new() } else { format!("\n{last}") }),
            );
        }
        !died.is_empty()
    }

    pub fn status(&self, cfg: &Config) -> Vec<Status> {
        let running = self.running.lock().unwrap();
        let errors = self.errors.lock().unwrap();
        cfg.services
            .iter()
            .map(|svc| {
                let pkg = active(cfg, &svc.id, None);
                let (state, pid) = match running.get(&svc.id) {
                    Some(r) => ("running", Some(r.child.id())),
                    None if errors.contains_key(&svc.id) => ("error", None),
                    None => ("stopped", None),
                };
                Status {
                    id: svc.id.clone(),
                    state: state.into(),
                    pid,
                    installed: pkg.is_some(),
                    version: pkg.map(|p| p.version.clone()),
                    message: errors.get(&svc.id).cloned(),
                }
            })
            .collect()
    }
}

fn wait_for(child: &mut Child, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn failure(name: &str, what: &str, log: &Path, console: &Path) -> String {
    let mut lines = tail(log, 6);
    if log != console {
        let c = tail(console, 4);
        if !c.is_empty() && !c.starts_with("=====") {
            lines = [lines, c].join("\n");
        }
    }
    let lines = lines.trim();
    if lines.is_empty() {
        format!("{name} {what}.")
    } else {
        format!("{name} {what}. Últimas linhas do log ({}):\n{lines}", log.display())
    }
}

/// Abre um Prompt de Comando na pasta www com o PATH dos serviços.
pub fn open_terminal(cfg: &Config, layout: &Layout) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        let root = PathBuf::from(&cfg.settings.document_root);
        let _ = fs::create_dir_all(&root);
        Command::new("cmd.exe")
            .args(["/K", "title JJPHPMY & echo Terminal do JJPHPMY: PHP, Node.js, Python e bancos no PATH."])
            .current_dir(&root)
            .envs(child_env(cfg, layout))
            .creation_flags(CREATE_NEW_CONSOLE)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Não foi possível abrir o terminal: {e}"))
    }
    #[cfg(not(windows))]
    {
        let _ = (cfg, layout);
        Err("Disponível só no Windows.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_listener_in_any_language() {
        let text = "\n  Proto  Endereço local  Endereço externo  Estado  PID\n\
                    \x20 TCP    0.0.0.0:135     0.0.0.0:0     ESCUTANDO     1972\n\
                    \x20 TCP    127.0.0.1:8080  127.0.0.1:5000  ESTABELECIDA  77\n\
                    \x20 TCP    0.0.0.0:80      0.0.0.0:0     ESCUTANDO     4321\n\
                    \x20 TCP    [::]:3306       [::]:0        LISTENING     999\n";
        assert_eq!(parse_listener(text, 80), Some(4321));
        assert_eq!(parse_listener(text, 3306), Some(999));
        assert_eq!(parse_listener(text, 8080), None); // conexão, não escuta
        assert_eq!(parse_listener(text, 35), None); // ":135" não é ":35"
    }

    #[test]
    fn tail_returns_last_lines() {
        let path = std::env::temp_dir().join(format!("jjphpmy-tail-{}.log", std::process::id()));
        fs::write(&path, "a\nb\n\nc\nd\n").unwrap();
        assert_eq!(tail(&path, 2), "c\nd");
        assert_eq!(tail(&path, 10), "a\nb\nc\nd");
        let _ = fs::remove_file(&path);
        assert_eq!(tail(&path, 3), "");
    }

    fn http_get(port: u16, path: &str) -> String {
        use std::io::{Read, Write};
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        write!(s, "GET {path} HTTP/1.0\r\nHost: localhost\r\n\r\n").unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    }

    /// Inicia e para serviços reais numa pasta temporária, em portas
    /// alternativas. Reaproveita os pacotes baixados em execuções anteriores.
    /// `cargo test live_services -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_services() {
        use std::sync::Arc;
        let layout = Layout { base: std::env::temp_dir().join("jjphpmy-live-svc") };
        let catalog = crate::catalog::Catalog::embedded();
        let emit: crate::installer::Emit = Arc::new(|_| {});
        let mut cfg = Config::default();
        cfg.settings.document_root = layout.dir(&["www"]).display().to_string();
        cfg.settings.data_dir = layout.dir(&["data"]).display().to_string();
        confgen::ensure_secret(&mut cfg);
        for (id, port) in [("apache", 18080), ("nginx", 18081), ("mysql", 13306), ("postgresql", 15432), ("redis", 16379), ("mailpit", 11025)] {
            let svc = cfg.services.iter_mut().find(|s| s.id == id).unwrap();
            svc.port = port;
            svc.enabled = true;
        }
        for (id, variant) in [
            ("apache", None),
            ("php", Some("ts")),
            ("php", Some("nts")),
            ("nginx", None),
            ("mysql", None),
            ("postgresql", None),
            ("redis", None),
            ("mailpit", None),
        ] {
            let (bin, tmp) = (layout.dir(&["bin"]), layout.dir(&["tmp"]));
            let suffix = variant.map(|v| format!("-{v}")).unwrap_or_default();
            let existing = fs::read_dir(bin.join(id)).ok().and_then(|d| {
                d.flatten().map(|e| e.file_name().to_string_lossy().to_string()).find(|n| n.ends_with(&suffix) && (variant.is_some() || !n.ends_with("ts")))
            });
            let p = match existing {
                Some(name) => InstalledPackage {
                    id: id.into(),
                    version: name.trim_end_matches(&suffix).to_string(),
                    active: true,
                    variant: variant.map(Into::into),
                    path: bin.join(id).join(&name).display().to_string(),
                },
                None => {
                    let fut = crate::installer::install_in(emit.clone(), catalog.get(id).unwrap(), None, variant, &bin, &tmp);
                    tauri::async_runtime::block_on(fut).unwrap_or_else(|e| panic!("{id}: {e}")).unwrap()
                }
            };
            println!("pacote: {id} {}{suffix}", p.version);
            cfg.installed.push(InstalledPackage { active: true, ..p });
        }
        confgen::generate(&cfg, &layout, true).unwrap();
        confgen::init_data(&cfg, &layout).unwrap();
        fs::write(layout.dir(&["www", "index.php"]), "<?php echo 'ok-' . PHP_VERSION . '-' . PHP_SAPI;").unwrap();

        let m = Manager::default();
        for id in ["mysql", "postgresql", "redis", "mailpit", "apache", "nginx"] {
            let t = Instant::now();
            m.start(&cfg, &layout, id).unwrap_or_else(|e| panic!("{e}"));
            println!("iniciado: {id} em {:.1} s", t.elapsed().as_secs_f32());
        }
        assert!(m.is_running("php-cgi"), "o Nginx deveria ter iniciado o php-cgi");

        let apache = http_get(18080, "/index.php");
        println!("Apache: {}", apache.lines().last().unwrap_or(""));
        assert!(apache.contains("ok-") && apache.contains("apache2handler"));
        let nginx = http_get(18081, "/index.php");
        println!("Nginx:  {}", nginx.lines().last().unwrap_or(""));
        assert!(nginx.contains("ok-") && nginx.contains("cgi-fcgi"));

        let path = |id: &str| cfg.installed.iter().find(|p| p.id == id).unwrap().path.clone();
        let out = Command::new(Path::new(&path("mysql")).join("bin").join("mysql.exe"))
            .args(["-u", "root", "-h", "127.0.0.1", "-P", "13306", "-N", "-e", "select version()"])
            .output()
            .unwrap();
        println!("MySQL: {}", String::from_utf8_lossy(&out.stdout).trim());
        assert!(out.status.success());
        let out = Command::new(Path::new(&path("postgresql")).join("bin").join("psql.exe"))
            .args(["-U", "postgres", "-h", "127.0.0.1", "-p", "15432", "-tAc", "select version()"])
            .output()
            .unwrap();
        println!("PostgreSQL: {}", String::from_utf8_lossy(&out.stdout).trim());
        assert!(out.status.success());

        // Porta ocupada: o segundo Apache tem que recusar dizendo quem ocupa.
        let mut other = cfg.clone();
        other.services.iter_mut().find(|s| s.id == "redis").unwrap().port = 18080;
        let m2 = Manager::default();
        let err = m2.start(&other, &layout, "redis").unwrap_err();
        println!("porta ocupada: {err}");
        assert!(err.contains("18080") && err.contains("httpd.exe"));

        for id in m.running_ids().iter().rev() {
            let t = Instant::now();
            m.stop(id);
            println!("parado: {id} em {:.1} s", t.elapsed().as_secs_f32());
        }
        std::thread::sleep(Duration::from_millis(500));
        for port in [18080, 18081, 13306, 15432, 16379, 11025, 9000] {
            assert!(port_owner(port).is_none(), "a porta {port} continuou ocupada");
        }
        println!("todas as portas liberadas");
    }

    #[test]
    fn spec_requires_installed_package() {
        let err = spec(&Config::default(), &Layout::system(), "mysql").unwrap_err();
        assert!(err.contains("não está instalado"));
    }
}
