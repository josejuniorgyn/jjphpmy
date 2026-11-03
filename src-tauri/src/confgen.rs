//! Etapa 4: arquivos de configuração gerados em `etc\` a partir de
//! `templates/`, com as portas, pastas e versões ativas, e a criação dos
//! dados iniciais dos bancos.
//!
//! Arquivos editados pelo usuário não são sobrescritos: o hash do último
//! conteúdo gerado fica em `etc\.gerados.json`, e um arquivo cujo hash não
//! confere é mantido (a menos que `force` seja usado).

use crate::config::{base_path, Config, InstalledPackage};
use crate::installer::hidden;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Porta do php-cgi (FastCGI) usado pelo Nginx.
pub const PHP_CGI_PORT: u16 = 9000;

const HTTPD: &str = include_str!("../templates/httpd.conf");
const HTTPD_PHP: &str = include_str!("../templates/httpd-php.conf");
const HTTPD_PMA: &str = include_str!("../templates/httpd-phpmyadmin.conf");
const HTTPD_SSL: &str = include_str!("../templates/httpd-ssl.conf");
const NGINX: &str = include_str!("../templates/nginx.conf");
const NGINX_PHP: &str = include_str!("../templates/nginx-php.conf");
const NGINX_PMA: &str = include_str!("../templates/nginx-phpmyadmin.conf");
const MY_INI: &str = include_str!("../templates/my.ini");
const POSTGRESQL: &str = include_str!("../templates/postgresql.conf");
const MONGOD: &str = include_str!("../templates/mongod.cfg");
const REDIS: &str = include_str!("../templates/redis.conf");
const PMA_CONFIG: &str = include_str!("../templates/config.inc.php");

/// Extensões ligadas no php.ini, quando a DLL existe na versão instalada.
const PHP_EXTENSIONS: &[&str] = &[
    "bz2", "curl", "exif", "fileinfo", "gd", "gettext", "intl", "mbstring", "mysqli", "openssl", "pdo_mysql",
    "pdo_pgsql", "pdo_sqlite", "pgsql", "soap", "sockets", "sodium", "sqlite3", "xsl", "zip",
];

/// Raiz de tudo (`C:\jjphpmy`); nos testes, uma pasta temporária.
#[derive(Debug, Clone)]
pub struct Layout {
    pub base: PathBuf,
}

impl Layout {
    pub fn system() -> Self {
        Self { base: base_path(&[]) }
    }

    pub fn dir(&self, parts: &[&str]) -> PathBuf {
        parts.iter().fold(self.base.clone(), |p, part| p.join(part))
    }
}

/// Caminho com barras normais, como Apache, Nginx, MySQL e PHP preferem.
pub fn fwd(p: impl AsRef<Path>) -> String {
    p.as_ref().display().to_string().replace('\\', "/")
}

/// Caminho no formato do runtime msys2 (`/cygdrive/c/...`), o único que o
/// Redis para Windows entende: ele trata `C:/...` como caminho relativo.
pub fn msys(p: impl AsRef<Path>) -> String {
    let f = fwd(p);
    match f.split_once(":/") {
        Some((drive, rest)) if drive.len() == 1 => format!("/cygdrive/{}/{rest}", drive.to_lowercase()),
        _ => f,
    }
}

/// Troca `{{nome}}` pelos valores. Placeholders sem valor ficam como estão.
pub fn render(tpl: &str, vars: &[(&str, String)]) -> String {
    vars.iter().fold(tpl.to_string(), |s, (k, v)| s.replace(&format!("{{{{{k}}}}}"), v))
}

/* ---------- Versões ativas ---------- */

#[derive(Default)]
struct Active<'a> {
    apache: Option<&'a InstalledPackage>,
    nginx: Option<&'a InstalledPackage>,
    php_ts: Option<&'a InstalledPackage>,
    php_nts: Option<&'a InstalledPackage>,
    mysql: Option<&'a InstalledPackage>,
    postgresql: Option<&'a InstalledPackage>,
    mongodb: Option<&'a InstalledPackage>,
    redis: Option<&'a InstalledPackage>,
    phpmyadmin: Option<&'a InstalledPackage>,
}

fn active(installed: &[InstalledPackage]) -> Active<'_> {
    let mut a = Active::default();
    for p in installed.iter().filter(|p| p.active && !p.path.is_empty()) {
        let slot = match (p.id.as_str(), p.variant.as_deref()) {
            ("apache", _) => &mut a.apache,
            ("nginx", _) => &mut a.nginx,
            ("php", Some("nts")) => &mut a.php_nts,
            ("php", _) => &mut a.php_ts,
            ("mysql", _) => &mut a.mysql,
            ("postgresql", _) => &mut a.postgresql,
            ("mongodb", _) => &mut a.mongodb,
            ("redis", _) => &mut a.redis,
            ("phpmyadmin", _) => &mut a.phpmyadmin,
            _ => continue,
        };
        *slot = Some(p);
    }
    a
}

/// "8.4.13" -> "8.4"
pub fn major_minor(v: &str) -> String {
    v.split(['.', '-']).take(2).collect::<Vec<_>>().join(".")
}

fn major(v: &str) -> String {
    v.split(['.', '-']).next().unwrap_or(v).to_string()
}

/// Pasta de dados de cada banco. Separada por versão (como no Laragon),
/// porque versões maiores diferentes não usam os mesmos arquivos.
pub fn data_dir(cfg: &Config, p: &InstalledPackage) -> Option<PathBuf> {
    let root = PathBuf::from(&cfg.settings.data_dir);
    match p.id.as_str() {
        "mysql" => Some(root.join(format!("mysql-{}", major_minor(&p.version)))),
        "postgresql" => Some(root.join(format!("postgresql-{}", major(&p.version)))),
        "mongodb" => Some(root.join(format!("mongodb-{}", major(&p.version)))),
        "redis" => Some(root.join("redis")),
        _ => None,
    }
}

fn port(cfg: &Config, id: &str, default: u16) -> String {
    cfg.service(id).map(|s| s.port).unwrap_or(default).to_string()
}

/// Pasta da versão ativa de um pacote (no PHP, `ts` ou `nts`).
pub fn active_path(cfg: &Config, id: &str, variant: Option<&str>) -> Option<PathBuf> {
    let a = active(&cfg.installed);
    let p = match (id, variant) {
        ("apache", _) => a.apache,
        ("nginx", _) => a.nginx,
        ("php", Some("nts")) => a.php_nts,
        ("php", _) => a.php_ts,
        _ => cfg.installed.iter().find(|p| p.id == id && p.active),
    }?;
    Some(PathBuf::from(&p.path))
}

/// Bloco `location ~ \.php$` do Nginx (vazio sem PHP NTS), usado também nos hosts virtuais.
pub fn nginx_php_block(cfg: &Config) -> String {
    let a = active(&cfg.installed);
    match (a.nginx, a.php_nts) {
        (Some(n), Some(p)) => render(
            NGINX_PHP,
            &[("nginx", fwd(&n.path)), ("php_version", p.version.clone()), ("fastcgi_port", PHP_CGI_PORT.to_string())],
        ),
        _ => String::new(),
    }
}

/* ---------- php.ini ---------- */

/// Troca a primeira linha `chave = ...` (ativa ou comentada com `;` colado)
/// por `chave = valor`; se não houver, acrescenta no fim.
pub fn set_ini(text: &mut String, key: &str, value: &str) {
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    let matches = |l: &str| {
        let l = l.strip_prefix(';').unwrap_or(l);
        l.strip_prefix(key).is_some_and(|rest| rest.trim_start().starts_with('='))
    };
    match lines.iter().position(|l| matches(l)) {
        Some(i) => lines[i] = format!("{key} = {value}"),
        None => lines.push(format!("{key} = {value}")),
    }
    *text = lines.join("\n") + "\n";
}

/// Liga `extension=nome` (descomenta a linha ou acrescenta).
fn enable_extension(text: &mut String, name: &str) {
    let commented = format!(";extension={name}");
    let active = format!("extension={name}");
    if text.lines().any(|l| l.trim() == active) {
        return;
    }
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    match lines.iter().position(|l| l.trim() == commented) {
        Some(i) => lines[i] = active,
        None => lines.push(active),
    }
    *text = lines.join("\n") + "\n";
}

/// php.ini a partir do `php.ini-development` da própria versão instalada.
pub fn php_ini(cfg: &Config, layout: &Layout, php: &InstalledPackage) -> String {
    let dir = Path::new(&php.path);
    let mut ini = fs::read_to_string(dir.join("php.ini-development")).unwrap_or_else(|_| "[PHP]\n".into());
    let (logs, tmp) = (layout.dir(&["logs"]), layout.dir(&["tmp"]));
    let quoted = |p: PathBuf| format!("\"{}\"", fwd(p));
    for (key, value) in [
        ("extension_dir", quoted(dir.join("ext"))),
        ("memory_limit", "512M".into()),
        ("max_execution_time", "300".into()),
        ("upload_max_filesize", "100M".into()),
        ("post_max_size", "100M".into()),
        ("date.timezone", "America/Sao_Paulo".into()),
        ("error_log", quoted(logs.join("php").join("php_errors.log"))),
        ("sys_temp_dir", quoted(tmp.join("php"))),
        ("upload_tmp_dir", quoted(tmp.join("php").join("uploads"))),
        ("session.save_path", quoted(tmp.join("php").join("sessions"))),
        // E-mails do PHP vão para o Mailpit.
        ("SMTP", "localhost".into()),
        ("smtp_port", port(cfg, "mailpit", 1025)),
    ] {
        set_ini(&mut ini, key, &value);
    }
    for ext in PHP_EXTENSIONS {
        if dir.join("ext").join(format!("php_{ext}.dll")).exists() {
            enable_extension(&mut ini, ext);
        }
    }
    let variant = if php.variant.as_deref() == Some("nts") { "Non-Thread Safe" } else { "Thread Safe" };
    format!(
        "; Gerado pelo JJPHPMY para o PHP {} ({variant}). Se você editar este arquivo,\n\
         ; o JJPHPMY deixa de sobrescrevê-lo (use \"Recriar arquivos de configuração\").\n\n{ini}",
        php.version
    )
}

/// Nome da DLL do módulo Apache (`php8apache2_4.dll`, `php7apache2_4.dll`).
fn php_apache_module(php: &InstalledPackage) -> Option<String> {
    fs::read_dir(&php.path).ok()?.flatten().map(|e| e.file_name().to_string_lossy().to_string()).find(|n| {
        let n = n.to_lowercase();
        n.starts_with("php") && n.ends_with("apache2_4.dll")
    })
}

/* ---------- Plano de arquivos ---------- */

#[derive(Debug, Clone)]
pub struct GenFile {
    pub path: PathBuf,
    pub content: String,
}

/// Todos os arquivos que devem existir para as versões ativas.
pub fn plan(cfg: &Config, layout: &Layout) -> Vec<GenFile> {
    let a = active(&cfg.installed);
    let etc = layout.dir(&["etc"]);
    let base_vars = vec![
        ("etc", fwd(&etc)),
        ("logs", fwd(layout.dir(&["logs"]))),
        ("tmp", fwd(layout.dir(&["tmp"]))),
        ("www", fwd(&cfg.settings.document_root)),
        ("fastcgi_port", PHP_CGI_PORT.to_string()),
    ];
    let with = |extra: Vec<(&'static str, String)>| -> Vec<(&str, String)> {
        // Os extras vêm primeiro: `render` usa o primeiro valor de cada nome.
        extra.into_iter().chain(base_vars.iter().cloned()).collect()
    };
    let pma_vars = |p: &InstalledPackage| vec![("phpmyadmin", fwd(&p.path)), ("pma_version", p.version.clone())];
    let mut out = Vec::new();

    // PHP: o php.ini principal vem da TS (Apache e linha de comando); se só
    // houver a NTS, vem dela. Com as duas, a NTS ganha o php-cgi.ini (Nginx).
    if let Some(main) = a.php_ts.or(a.php_nts) {
        out.push(GenFile { path: etc.join("php").join("php.ini"), content: php_ini(cfg, layout, main) });
    }
    if let (Some(_), Some(nts)) = (a.php_ts, a.php_nts) {
        out.push(GenFile { path: etc.join("php").join("php-cgi.ini"), content: php_ini(cfg, layout, nts) });
    }

    if let Some(apache) = a.apache {
        let php_block = match a.php_ts.and_then(|p| Some((p, php_apache_module(p)?))) {
            Some((p, module)) => render(
                HTTPD_PHP,
                &with(vec![("php", fwd(&p.path)), ("php_module", module), ("php_version", p.version.clone())]),
            ),
            None => String::new(),
        };
        let pma_block = match (a.phpmyadmin, a.php_ts) {
            (Some(p), Some(_)) => render(HTTPD_PMA, &pma_vars(p)),
            _ => String::new(),
        };
        let ssl_block = match crate::ssl::port(cfg, "apache") {
            Some(p) => render(
                HTTPD_SSL,
                &with(vec![
                    ("ssl_port", p.to_string()),
                    ("cert", fwd(crate::ssl::server_cert(layout))),
                    ("key", fwd(crate::ssl::server_key(layout))),
                ]),
            ),
            None => String::new(),
        };
        let vars = with(vec![
            ("apache", fwd(&apache.path)),
            ("port", port(cfg, "apache", 80)),
            ("php_block", php_block),
            ("phpmyadmin_block", pma_block),
            ("ssl_block", ssl_block),
        ]);
        out.push(GenFile { path: etc.join("apache").join("httpd.conf"), content: render(HTTPD, &vars) });
    }

    if let Some(nginx) = a.nginx {
        let nginx_path = fwd(&nginx.path);
        let php_block = match a.php_nts {
            Some(p) => render(
                NGINX_PHP,
                &with(vec![("nginx", nginx_path.clone()), ("php_version", p.version.clone())]),
            ),
            None => String::new(),
        };
        let pma_block = match (a.phpmyadmin, a.php_nts) {
            (Some(p), Some(_)) => render(
                NGINX_PMA,
                &with(pma_vars(p).into_iter().chain([("nginx", nginx_path.clone())]).collect()),
            ),
            _ => String::new(),
        };
        let vars = with(vec![
            ("nginx", nginx_path),
            ("port", port(cfg, "nginx", 8080)),
            ("ssl_listen", crate::ssl::nginx_listen(cfg, layout, "        ")),
            ("php_block", php_block),
            ("phpmyadmin_block", pma_block),
        ]);
        out.push(GenFile { path: etc.join("nginx").join("nginx.conf"), content: render(NGINX, &vars) });
    }

    if let Some(p) = a.mysql {
        let vars = with(vec![
            ("mysql", fwd(&p.path)),
            ("port", port(cfg, "mysql", 3306)),
            ("datadir", fwd(data_dir(cfg, p).unwrap_or_default())),
        ]);
        out.push(GenFile { path: etc.join("mysql").join("my.ini"), content: render(MY_INI, &vars) });
    }

    if a.postgresql.is_some() {
        let vars = with(vec![("port", port(cfg, "postgresql", 5432))]);
        out.push(GenFile { path: etc.join("postgresql").join("postgresql.conf"), content: render(POSTGRESQL, &vars) });
    }

    if let Some(p) = a.mongodb {
        let vars = with(vec![("port", port(cfg, "mongodb", 27017)), ("datadir", fwd(data_dir(cfg, p).unwrap_or_default()))]);
        out.push(GenFile { path: etc.join("mongodb").join("mongod.cfg"), content: render(MONGOD, &vars) });
    }

    if let Some(p) = a.redis {
        let vars = with(vec![
            ("port", port(cfg, "redis", 6379)),
            ("datadir", msys(data_dir(cfg, p).unwrap_or_default())),
            ("logs", msys(layout.dir(&["logs"]))),
        ]);
        out.push(GenFile { path: etc.join("redis").join("redis.conf"), content: render(REDIS, &vars) });
    }

    if let Some(p) = a.phpmyadmin {
        let main = etc.join("phpmyadmin").join("config.inc.php");
        let vars = with(vec![("secret", cfg.pma_secret.clone()), ("mysql_port", port(cfg, "mysql", 3306))]);
        // O phpMyAdmin lê o config.inc.php da própria pasta; ele só inclui o de etc\.
        let stub = format!(
            "<?php\n// Gerado pelo JJPHPMY: a configuração fica em {}\nrequire '{}';\n",
            main.display(),
            fwd(&main)
        );
        out.push(GenFile { path: main, content: render(PMA_CONFIG, &vars) });
        out.push(GenFile { path: Path::new(&p.path).join("config.inc.php"), content: stub });
    }
    out
}

/// Pastas de logs e temporárias que os serviços esperam encontrar.
fn ensure_dirs(cfg: &Config, layout: &Layout) {
    let mut dirs: Vec<PathBuf> = ["apache", "nginx", "mysql", "postgresql", "mongodb", "redis", "php"]
        .iter()
        .map(|id| layout.dir(&["logs", id]))
        .collect();
    for sub in ["client_body", "proxy", "fastcgi", "uwsgi", "scgi"] {
        dirs.push(layout.dir(&["tmp", "nginx", sub]));
    }
    dirs.extend([
        layout.dir(&["tmp", "php", "uploads"]),
        layout.dir(&["tmp", "php", "sessions"]),
        layout.dir(&["tmp", "phpmyadmin"]),
        layout.dir(&["etc", "apache", "sites-enabled"]),
        layout.dir(&["etc", "nginx", "sites-enabled"]),
        PathBuf::from(&cfg.settings.document_root),
    ]);
    if let Some(redis) = active(&cfg.installed).redis {
        dirs.extend(data_dir(cfg, redis));
    }
    for d in dirs {
        let _ = fs::create_dir_all(d);
    }
}

/* ---------- Gravação ---------- */

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub written: Vec<String>,
    /// Arquivos editados pelo usuário, mantidos como estavam.
    pub kept: Vec<String>,
}

fn sha(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn state_file(layout: &Layout) -> PathBuf {
    layout.dir(&["etc", ".gerados.json"])
}

/// Grava os arquivos do `plan`. Sem `force`, mantém os que o usuário editou.
pub fn generate(cfg: &Config, layout: &Layout, force: bool) -> Result<Report, String> {
    ensure_dirs(cfg, layout);
    let state_path = state_file(layout);
    let mut state: BTreeMap<String, String> =
        fs::read_to_string(&state_path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let mut report = Report::default();

    for file in plan(cfg, layout) {
        let key = file.path.display().to_string();
        let new_hash = sha(&file.content);
        if let Ok(current) = fs::read_to_string(&file.path) {
            let current_hash = sha(&current);
            if current_hash == new_hash {
                state.insert(key, new_hash);
                continue;
            }
            if !force && state.get(&key) != Some(&current_hash) {
                report.kept.push(key);
                continue;
            }
        }
        if let Some(dir) = file.path.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("Não foi possível criar {}: {e}", dir.display()))?;
        }
        fs::write(&file.path, &file.content).map_err(|e| format!("Não foi possível gravar {key}: {e}"))?;
        state.insert(key.clone(), new_hash);
        report.written.push(key);
    }

    if let Ok(json) = serde_json::to_string_pretty(&state) {
        let _ = fs::write(&state_path, json);
    }
    Ok(report)
}

/// Chave do cookie do phpMyAdmin, criada uma vez e guardada na configuração.
pub fn ensure_secret(cfg: &mut Config) -> bool {
    if cfg.pma_secret.len() >= 32 {
        return false;
    }
    use std::hash::{BuildHasher, Hasher};
    // RandomState é semeado pelo sistema operacional a cada instância.
    let part = || {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
        format!("{:016x}", h.finish())
    };
    cfg.pma_secret = format!("{}{}", part(), part());
    true
}

/* ---------- Dados iniciais ---------- */

fn run(cmd: &mut Command, what: &str) -> Result<(), String> {
    let out = hidden(cmd).output().map_err(|e| format!("Não foi possível executar {what}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let text = String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    let tail: Vec<&str> = text.lines().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect();
    Err(format!("{what} falhou ({}):\n{}", out.status, tail.join("\n")))
}

fn is_empty_dir(dir: &Path) -> bool {
    fs::read_dir(dir).map(|mut d| d.next().is_none()).unwrap_or(true)
}

/// Cria os dados dos bancos ativos que ainda não têm pasta de dados.
/// MySQL: root sem senha (`--initialize-insecure`), como no Laragon.
/// PostgreSQL: usuário `postgres`, autenticação `trust` em localhost.
/// Devolve os nomes dos bancos inicializados.
pub fn init_data(cfg: &Config, layout: &Layout) -> Result<Vec<String>, String> {
    let a = active(&cfg.installed);
    let mut done = Vec::new();
    // O MySQL recusa a pasta de dados ("unusable") se a pasta-mãe não existir.
    let root = PathBuf::from(&cfg.settings.data_dir);
    fs::create_dir_all(&root).map_err(|e| format!("Não foi possível criar {}: {e}", root.display()))?;

    if let Some(p) = a.mysql {
        let data = data_dir(cfg, p).unwrap_or_default();
        if is_empty_dir(&data) {
            let ini = layout.dir(&["etc", "mysql", "my.ini"]);
            let mysqld = Path::new(&p.path).join("bin").join("mysqld.exe");
            let _ = fs::remove_dir(&data); // o MySQL exige que a pasta não exista ou esteja vazia
            run(
                Command::new(&mysqld).arg(format!("--defaults-file={}", ini.display())).args(["--initialize-insecure", "--console"]),
                "A inicialização do MySQL",
            )?;
            done.push(format!("MySQL {}", p.version));
        }
    }

    if let Some(p) = a.postgresql {
        let data = data_dir(cfg, p).unwrap_or_default();
        if is_empty_dir(&data) {
            let initdb = Path::new(&p.path).join("bin").join("initdb.exe");
            run(
                Command::new(&initdb).arg("-D").arg(&data).args(["-U", "postgres", "-A", "trust", "-E", "UTF8", "--no-locale"]),
                "A inicialização do PostgreSQL",
            )?;
            let conf = data.join("postgresql.conf");
            let include = format!(
                "\n# Configuração do JJPHPMY (porta, logs)\ninclude_if_exists = '{}'\n",
                fwd(layout.dir(&["etc", "postgresql", "postgresql.conf"]))
            );
            let mut text = fs::read_to_string(&conf).map_err(|e| format!("Não foi possível ler {}: {e}", conf.display()))?;
            text.push_str(&include);
            fs::write(&conf, text).map_err(|e| format!("Não foi possível gravar {}: {e}", conf.display()))?;
            done.push(format!("PostgreSQL {}", p.version));
        }
    }

    for p in [a.mongodb, a.redis].into_iter().flatten() {
        if let Some(data) = data_dir(cfg, p) {
            if !data.exists() {
                fs::create_dir_all(&data).map_err(|e| format!("Não foi possível criar {}: {e}", data.display()))?;
            }
        }
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_layout(name: &str) -> Layout {
        let base = std::env::temp_dir().join(format!("jjphpmy-conf-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        Layout { base }
    }

    fn pkg(layout: &Layout, id: &str, version: &str, variant: Option<&str>) -> InstalledPackage {
        let name = match variant {
            Some(v) => format!("{version}-{v}"),
            None => version.to_string(),
        };
        let path = layout.dir(&["bin", id, &name]);
        fs::create_dir_all(&path).unwrap();
        InstalledPackage { id: id.into(), version: version.into(), active: true, variant: variant.map(Into::into), path: path.display().to_string() }
    }

    fn config(layout: &Layout, installed: Vec<InstalledPackage>) -> Config {
        let mut cfg = Config::default();
        cfg.settings.document_root = layout.dir(&["www"]).display().to_string();
        cfg.settings.data_dir = layout.dir(&["data"]).display().to_string();
        cfg.installed = installed;
        cfg.pma_secret = "x".repeat(32);
        cfg
    }

    fn file<'a>(files: &'a [GenFile], name: &str) -> &'a str {
        &files.iter().find(|f| f.path.ends_with(name)).unwrap_or_else(|| panic!("{name} não gerado")).content
    }

    #[test]
    fn msys_paths_for_redis() {
        assert_eq!(msys(r"C:\jjphpmy\data\redis"), "/cygdrive/c/jjphpmy/data/redis");
        assert_eq!(msys("relativo/x"), "relativo/x");
    }

    #[test]
    fn renders_placeholders() {
        assert_eq!(render("a {{x}} b {{y}} {{z}}", &[("x", "1".into()), ("y", "2".into())]), "a 1 b 2 {{z}}");
    }

    #[test]
    fn set_ini_replaces_first_real_key() {
        let mut ini = ";     session.save_path = \"N;/path\"\n;session.save_path = \"/tmp\"\nmemory_limit = 128M\n".to_string();
        set_ini(&mut ini, "session.save_path", "\"C:/t\"");
        set_ini(&mut ini, "memory_limit", "512M");
        set_ini(&mut ini, "date.timezone", "America/Sao_Paulo");
        assert_eq!(
            ini,
            ";     session.save_path = \"N;/path\"\nsession.save_path = \"C:/t\"\nmemory_limit = 512M\ndate.timezone = America/Sao_Paulo\n"
        );
        // "memory_limit_x" não é "memory_limit".
        let mut ini = "memory_limit_x = 1\n".to_string();
        set_ini(&mut ini, "memory_limit", "2");
        assert!(ini.ends_with("memory_limit = 2\n"));
    }

    #[test]
    fn php_ini_uses_installed_extensions() {
        let layout = tmp_layout("phpini");
        let php = pkg(&layout, "php", "8.5.12-1", Some("ts"));
        let dir = Path::new(&php.path);
        fs::write(dir.join("php.ini-development"), "[PHP]\n;extension_dir = \"ext\"\n;extension=curl\n;extension=mysqli\n;extension=ldap\nSMTP = localhost\nsmtp_port = 25\n").unwrap();
        fs::create_dir_all(dir.join("ext")).unwrap();
        fs::write(dir.join("ext").join("php_curl.dll"), "").unwrap();
        fs::write(dir.join("ext").join("php_mysqli.dll"), "").unwrap();
        let ini = php_ini(&config(&layout, vec![php.clone()]), &layout, &php);
        assert!(ini.contains(&format!("extension_dir = \"{}/ext\"", fwd(&php.path))));
        assert!(ini.contains("\nextension=curl\n") && ini.contains("\nextension=mysqli\n"));
        assert!(ini.contains(";extension=ldap"));
        assert!(ini.contains("smtp_port = 1025"));
        let _ = fs::remove_dir_all(&layout.base);
    }

    #[test]
    fn apache_with_php_and_phpmyadmin() {
        let layout = tmp_layout("apache");
        let php = pkg(&layout, "php", "8.5.12-1", Some("ts"));
        fs::write(Path::new(&php.path).join("php8apache2_4.dll"), "").unwrap();
        let cfg = config(
            &layout,
            vec![pkg(&layout, "apache", "2.4.69", None), php, pkg(&layout, "phpmyadmin", "5.2.3", None)],
        );
        let files = plan(&cfg, &layout);
        let httpd = file(&files, r"apache\httpd.conf");
        assert!(httpd.contains("Listen 80\n"));
        assert!(httpd.contains("php8apache2_4.dll"));
        assert!(httpd.contains("Alias /phpmyadmin"));
        assert!(!httpd.contains("{{"), "placeholder sobrando:\n{httpd}");
        assert!(file(&files, r"5.2.3\config.inc.php").contains("require '"));
        assert!(file(&files, r"phpmyadmin\config.inc.php").contains("'3306'"));
        let _ = fs::remove_dir_all(&layout.base);
    }

    #[test]
    fn nginx_without_php_has_no_fastcgi() {
        let layout = tmp_layout("nginx");
        let mut cfg = config(&layout, vec![pkg(&layout, "nginx", "1.31.8", None)]);
        cfg.services.iter_mut().find(|s| s.id == "nginx").unwrap().port = 8081;
        let files = plan(&cfg, &layout);
        let conf = file(&files, r"nginx\nginx.conf");
        assert!(conf.contains("listen       8081;"));
        assert!(!conf.contains("fastcgi_pass"));
        assert!(!conf.contains("{{"));
        let _ = fs::remove_dir_all(&layout.base);
    }

    #[test]
    fn data_dirs_by_version() {
        let layout = tmp_layout("data");
        let cfg = config(&layout, vec![]);
        let dir = |id, v| data_dir(&cfg, &pkg(&layout, id, v, None)).unwrap();
        assert!(dir("mysql", "8.4.13").ends_with("mysql-8.4"));
        assert!(dir("postgresql", "18.6").ends_with("postgresql-18"));
        assert!(dir("mongodb", "8.0.34").ends_with("mongodb-8"));
        let _ = fs::remove_dir_all(&layout.base);
    }

    #[test]
    fn keeps_user_edits_unless_forced() {
        let layout = tmp_layout("edits");
        let mut cfg = config(&layout, vec![pkg(&layout, "redis", "8.10.3", None)]);
        let path = layout.dir(&["etc", "redis", "redis.conf"]);

        assert_eq!(generate(&cfg, &layout, false).unwrap().written.len(), 1);
        // Porta mudou e o arquivo não foi editado: regrava.
        cfg.services.iter_mut().find(|s| s.id == "redis").unwrap().port = 6380;
        assert_eq!(generate(&cfg, &layout, false).unwrap().written.len(), 1);
        assert!(fs::read_to_string(&path).unwrap().contains("port 6380"));
        // Usuário editou: mantém.
        fs::write(&path, "port 7000\n").unwrap();
        cfg.services.iter_mut().find(|s| s.id == "redis").unwrap().port = 6381;
        let report = generate(&cfg, &layout, false).unwrap();
        assert_eq!(report.kept.len(), 1);
        assert_eq!(fs::read_to_string(&path).unwrap(), "port 7000\n");
        // "Recriar arquivos de configuração": sobrescreve.
        generate(&cfg, &layout, true).unwrap();
        assert!(fs::read_to_string(&path).unwrap().contains("port 6381"));
        let _ = fs::remove_dir_all(&layout.base);
    }

    /// Instala os pacotes de verdade numa pasta temporária, gera os arquivos e
    /// valida com os próprios programas (httpd -t, nginx -t, php -m) e cria os
    /// bancos MySQL e PostgreSQL. Baixa cerca de 700 MB:
    /// `cargo test live_configs -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_configs() {
        use std::sync::Arc;
        let layout = Layout { base: std::env::temp_dir().join("jjphpmy-live-conf") };
        let _ = fs::remove_dir_all(&layout.base);
        let catalog = crate::catalog::Catalog::embedded();
        let emit: crate::installer::Emit = Arc::new(|_| {});
        let mut cfg = Config::default();
        cfg.settings.document_root = layout.dir(&["www"]).display().to_string();
        cfg.settings.data_dir = layout.dir(&["data"]).display().to_string();
        ensure_secret(&mut cfg);
        for (id, variant) in [
            ("apache", None),
            ("php", Some("ts")),
            ("php", Some("nts")),
            ("nginx", None),
            ("phpmyadmin", None),
            ("redis", None),
            ("mysql", None),
            ("postgresql", None),
        ] {
            let pkg = catalog.get(id).unwrap();
            let (bin, tmp) = (layout.dir(&["bin"]), layout.dir(&["tmp"]));
            let fut = crate::installer::install_in(emit.clone(), pkg, None, variant, &bin, &tmp);
            let mut p = tauri::async_runtime::block_on(fut).unwrap_or_else(|e| panic!("{id}: {e}")).unwrap();
            println!("instalado: {id} {} {:?}", p.version, p.variant);
            p.active = true;
            cfg.installed.push(p);
        }

        let report = generate(&cfg, &layout, false).unwrap();
        for f in &report.written {
            println!("gerado: {f}");
        }
        let path = |id: &str, variant: Option<&str>| {
            cfg.installed.iter().find(|p| p.id == id && p.variant.as_deref() == variant).unwrap().path.clone()
        };
        let check = |cmd: &mut Command, what: &str| {
            let out = cmd.output().unwrap();
            let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
            println!("--- {what}: {}\n{}", out.status, text.trim());
            assert!(out.status.success(), "{what} falhou");
            text
        };
        let etc = layout.dir(&["etc"]);
        check(
            Command::new(Path::new(&path("apache", None)).join("bin").join("httpd.exe"))
                .args(["-t", "-f"])
                .arg(etc.join("apache").join("httpd.conf")),
            "httpd -t",
        );
        let nginx = path("nginx", None);
        check(
            Command::new(Path::new(&nginx).join("nginx.exe"))
                .arg("-t")
                .arg("-p")
                .arg(format!("{}/", fwd(&nginx)))
                .arg("-c")
                .arg(etc.join("nginx").join("nginx.conf")),
            "nginx -t",
        );
        let modules = check(
            Command::new(Path::new(&path("php", Some("ts"))).join("php.exe"))
                .arg("-c")
                .arg(etc.join("php").join("php.ini"))
                .arg("-m"),
            "php -m",
        );
        for m in ["curl", "mbstring", "mysqli", "openssl", "pdo_mysql", "pdo_pgsql", "intl", "zip"] {
            assert!(modules.lines().any(|l| l.trim() == m), "extensão {m} não carregou");
        }
        assert!(!modules.contains("Warning"), "o PHP avisou algo ao carregar");
        check(
            Command::new(Path::new(&path("php", Some("nts"))).join("php-cgi.exe"))
                .arg("-c")
                .arg(etc.join("php").join("php-cgi.ini"))
                .arg("-v"),
            "php-cgi -v",
        );

        let done = init_data(&cfg, &layout).unwrap();
        println!("bancos criados: {done:?}");
        let mysql = cfg.installed.iter().find(|p| p.id == "mysql").unwrap();
        let pg = cfg.installed.iter().find(|p| p.id == "postgresql").unwrap();
        assert!(data_dir(&cfg, mysql).unwrap().join("mysql.ibd").exists());
        let pg_data = data_dir(&cfg, pg).unwrap();
        assert!(pg_data.join("PG_VERSION").exists());
        assert!(fs::read_to_string(pg_data.join("postgresql.conf")).unwrap().contains("include_if_exists"));
        let _ = fs::remove_dir_all(&layout.base);
    }

    #[test]
    fn secret_is_created_once() {
        let mut cfg = Config::default();
        assert!(ensure_secret(&mut cfg));
        assert_eq!(cfg.pma_secret.len(), 32);
        let first = cfg.pma_secret.clone();
        assert!(!ensure_secret(&mut cfg));
        assert_eq!(cfg.pma_secret, first);
    }
}
