//! Etapa 6: hosts virtuais. Cada subpasta de `www` vira `{name}.test`
//! (padrão `hostnamePattern`) no Apache e no Nginx, e ganha uma linha no
//! arquivo `hosts` do Windows entre `# jjphpmy begin` e `# jjphpmy end`.
//!
//! Os arquivos gerados se chamam `auto.*.conf` e são recriados sempre;
//! outros `.conf` em `sites-enabled` (do usuário) nunca são tocados.

use crate::confgen::{self, fwd, render, Layout};
use crate::config::Config;
use crate::installer::hidden;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

pub const HOSTS_FILE: &str = r"C:\Windows\System32\drivers\etc\hosts";
pub const BEGIN: &str = "# jjphpmy begin";
pub const END: &str = "# jjphpmy end";

const APACHE: &str = include_str!("../templates/vhost-apache.conf");
const APACHE_DEFAULT: &str = include_str!("../templates/vhost-apache-default.conf");
const APACHE_SSL: &str = include_str!("../templates/vhost-apache-ssl.conf");
const NGINX: &str = include_str!("../templates/vhost-nginx.conf");

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Site {
    /// Nome da pasta dentro de www.
    pub folder: String,
    pub host: String,
    /// Raiz servida: a pasta, ou `public` dentro dela (Laravel e afins).
    pub root: String,
    pub url: String,
    /// Link https, quando o SSL está ligado no servidor do link.
    pub https_url: Option<String>,
}

/// Letra sem acento ("ç" -> "c"), para nomes de pasta em português.
fn plain(c: char) -> char {
    match c {
        'á' | 'à' | 'â' | 'ã' | 'ä' => 'a',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'í' | 'ì' | 'î' | 'ï' => 'i',
        'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
        'ú' | 'ù' | 'û' | 'ü' => 'u',
        'ç' => 'c',
        'ñ' => 'n',
        other => other,
    }
}

/// Nome de pasta -> parte válida de hostname ("Ação_Social" -> "acao-social").
pub fn slug(folder: &str) -> String {
    let s: String = folder
        .to_lowercase()
        .chars()
        .map(plain)
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect();
    let mut out = String::new();
    for c in s.trim_matches('-').chars() {
        if !(c == '-' && out.ends_with('-')) {
            out.push(c);
        }
    }
    out
}

/// Servidor usado no link: o Apache, se instalado; senão, o Nginx.
fn web_server(cfg: &Config) -> &'static str {
    let has = |id| confgen::active_path(cfg, id, None).is_some();
    if has("apache") || !has("nginx") { "apache" } else { "nginx" }
}

/// Sites atuais (subpastas de www), em ordem alfabética, sem repetir host.
pub fn sites(cfg: &Config) -> Vec<Site> {
    let www = PathBuf::from(&cfg.settings.document_root);
    let server = web_server(cfg);
    let port = cfg.service(server).map(|s| s.port).unwrap_or(80);
    let ssl = crate::ssl::port(cfg, server);
    let mut by_host: BTreeMap<String, Site> = BTreeMap::new();
    let Ok(entries) = fs::read_dir(&www) else { return Vec::new() };
    for entry in entries.flatten().filter(|e| e.path().is_dir()) {
        let folder = entry.file_name().to_string_lossy().to_string();
        let name = slug(&folder);
        if folder.starts_with('.') || name.is_empty() {
            continue;
        }
        let host = cfg.settings.hostname_pattern.replace("{name}", &name).to_lowercase();
        let dir = entry.path();
        let root = if dir.join("public").is_dir() { dir.join("public") } else { dir };
        let url = if port == 80 { format!("http://{host}/") } else { format!("http://{host}:{port}/") };
        let https_url = ssl.map(|p| if p == 443 { format!("https://{host}/") } else { format!("https://{host}:{p}/") });
        by_host.entry(host.clone()).or_insert(Site { folder, host, root: fwd(&root), url, https_url });
    }
    by_host.into_values().collect()
}

/* ---------- Arquivos dos servidores ---------- */

/// (arquivo, conteúdo) de cada host virtual, para Apache e Nginx instalados.
pub fn plan(cfg: &Config, layout: &Layout, sites: &[Site]) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let port = |id: &str| cfg.service(id).map(|s| s.port).unwrap_or(0).to_string();
    if confgen::active_path(cfg, "apache", None).is_some() {
        let dir = layout.dir(&["etc", "apache", "sites-enabled"]);
        let www = fwd(&cfg.settings.document_root);
        out.push((dir.join("auto.00-default.conf"), render(APACHE_DEFAULT, &[("port", port("apache")), ("www", www)])));
        let ssl = crate::ssl::port(cfg, "apache");
        for s in sites {
            let ssl_vhost = match ssl {
                Some(p) => render(
                    APACHE_SSL,
                    &[
                        ("host", s.host.clone()),
                        ("root", s.root.clone()),
                        ("ssl_port", p.to_string()),
                        ("cert", fwd(crate::ssl::server_cert(layout))),
                        ("key", fwd(crate::ssl::server_key(layout))),
                    ],
                ),
                None => String::new(),
            };
            let vars = [
                ("folder", s.folder.clone()),
                ("host", s.host.clone()),
                ("root", s.root.clone()),
                ("port", port("apache")),
                ("ssl_vhost", ssl_vhost),
            ];
            out.push((dir.join(format!("auto.{}.conf", s.host)), render(APACHE, &vars)));
        }
    }
    if confgen::active_path(cfg, "nginx", None).is_some() {
        let dir = layout.dir(&["etc", "nginx", "sites-enabled"]);
        let php = confgen::nginx_php_block(cfg);
        for s in sites {
            let vars = [
                ("folder", s.folder.clone()),
                ("host", s.host.clone()),
                ("root", s.root.clone()),
                ("port", port("nginx")),
                ("ssl_listen", crate::ssl::nginx_listen(cfg, layout, "    ")),
                ("php_block", php.clone()),
            ];
            out.push((dir.join(format!("auto.{}.conf", s.host)), render(NGINX, &vars)));
        }
    }
    out
}

/// Grava os `auto.*.conf` e apaga os que sobraram. Devolve os servidores
/// ("apache", "nginx") cujos arquivos mudaram e precisam reiniciar.
pub fn write_configs(cfg: &Config, layout: &Layout, sites: &[Site]) -> Result<Vec<&'static str>, String> {
    let files = if cfg.settings.auto_virtual_hosts { plan(cfg, layout, sites) } else { Vec::new() };
    let mut changed = Vec::new();
    for server in ["apache", "nginx"] {
        let dir = layout.dir(&["etc", server, "sites-enabled"]);
        let _ = fs::create_dir_all(&dir);
        let mut touched = false;
        let wanted: Vec<&(PathBuf, String)> = files.iter().filter(|(p, _)| p.starts_with(&dir)).collect();
        for (path, content) in &wanted {
            if fs::read_to_string(path).ok().as_deref() != Some(content.as_str()) {
                fs::write(path, content).map_err(|e| format!("Não foi possível gravar {}: {e}", path.display()))?;
                touched = true;
            }
        }
        for entry in fs::read_dir(&dir).map_err(|e| e.to_string())?.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let stale = name.starts_with("auto.") && name.ends_with(".conf") && !wanted.iter().any(|(p, _)| *p == entry.path());
            if stale {
                let _ = fs::remove_file(entry.path());
                touched = true;
            }
        }
        if touched {
            changed.push(server);
        }
    }
    Ok(changed)
}

/* ---------- Arquivo hosts ---------- */

/// Troca o bloco do JJPHPMY no conteúdo do `hosts`. Sem hosts, remove o bloco.
/// O resto do arquivo fica igual; usa CRLF, como o Windows.
pub fn merge_hosts(current: &str, hosts: &[String]) -> String {
    let mut lines: Vec<&str> = Vec::new();
    let mut inside = false;
    for line in current.lines() {
        match line.trim() {
            l if l.eq_ignore_ascii_case(BEGIN) => inside = true,
            l if l.eq_ignore_ascii_case(END) => inside = false,
            _ if !inside => lines.push(line),
            _ => {}
        }
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let mut out: Vec<String> = lines.into_iter().map(String::from).collect();
    if !hosts.is_empty() {
        out.push(String::new());
        out.push(BEGIN.into());
        out.extend(hosts.iter().map(|h| format!("127.0.0.1\t{h}")));
        out.push(END.into());
    }
    out.join("\r\n") + "\r\n"
}

/// Grava o `hosts` com uma única elevação (UAC): o conteúdo novo vai para
/// `tmp\hosts.jjphpmy` e um PowerShell elevado só copia o arquivo.
pub fn write_hosts(content: &str, layout: &Layout) -> Result<(), String> {
    let staged = layout.dir(&["tmp", "hosts.jjphpmy"]);
    fs::write(&staged, content).map_err(|e| format!("Não foi possível preparar o arquivo hosts: {e}"))?;
    let inner = format!("Copy-Item -LiteralPath '{}' -Destination '{HOSTS_FILE}' -Force", staged.display());
    let script = format!(
        "$p = Start-Process powershell -Verb RunAs -WindowStyle Hidden -Wait -PassThru \
         -ArgumentList '-NoProfile','-Command',\"{inner}\"; exit $p.ExitCode"
    );
    let status = hidden(Command::new("powershell").args(["-NoProfile", "-Command", &script]))
        .status()
        .map_err(|e| format!("Não foi possível atualizar o arquivo hosts: {e}"))?;
    let _ = fs::remove_file(&staged);
    if status.success() && fs::read_to_string(HOSTS_FILE).is_ok_and(|now| now == content) {
        Ok(())
    } else {
        Err("O arquivo hosts não foi atualizado (a permissão de administrador foi recusada?). \
             Os sites {name}.test só abrem depois disso."
            .into())
    }
}

/// Hosts que devem estar no arquivo `hosts`.
pub fn wanted_hosts(cfg: &Config, sites: &[Site]) -> Vec<String> {
    if cfg.settings.auto_virtual_hosts {
        sites.iter().map(|s| s.host.clone()).collect()
    } else {
        Vec::new()
    }
}

/// Conteúdo novo do `hosts`, ou `None` se já está certo.
pub fn hosts_update(wanted: &[String]) -> Option<String> {
    let current = fs::read_to_string(HOSTS_FILE).unwrap_or_default();
    let new = merge_hosts(&current, wanted);
    (new.replace("\r\n", "\n").trim_end() != current.replace("\r\n", "\n").trim_end()).then_some(new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::InstalledPackage;

    fn get(port: u16, host: &str) -> String {
        use std::io::{Read, Write};
        let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
        write!(s, "GET / HTTP/1.0\r\nHost: {host}\r\n\r\n").unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out.lines().last().unwrap_or("").to_string()
    }

    /// Apache e Nginx de verdade servindo dois sites e o localhost, com o
    /// nome do site no cabeçalho Host (não mexe no arquivo hosts).
    /// `cargo test live_vhosts -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_vhosts() {
        use std::sync::Arc;
        let layout = Layout { base: std::env::temp_dir().join("jjphpmy-live-vh") };
        let _ = fs::remove_dir_all(&layout.base);
        let catalog = crate::catalog::Catalog::embedded();
        let emit: crate::installer::Emit = Arc::new(|_| {});
        let mut cfg = Config::default();
        cfg.settings.document_root = layout.dir(&["www"]).display().to_string();
        cfg.settings.data_dir = layout.dir(&["data"]).display().to_string();
        for (id, port) in [("apache", 18090), ("nginx", 18091)] {
            cfg.services.iter_mut().find(|s| s.id == id).unwrap().port = port;
        }
        for (id, variant) in [("apache", None), ("php", Some("ts")), ("php", Some("nts")), ("nginx", None)] {
            let (bin, tmp) = (layout.dir(&["bin"]), layout.dir(&["tmp"]));
            let fut = crate::installer::install_in(emit.clone(), catalog.get(id).unwrap(), None, variant, &bin, &tmp);
            let p = tauri::async_runtime::block_on(fut).unwrap_or_else(|e| panic!("{id}: {e}")).unwrap();
            cfg.installed.push(InstalledPackage { active: true, ..p });
        }
        let www = layout.dir(&["www"]);
        fs::create_dir_all(www.join("blog")).unwrap();
        fs::create_dir_all(www.join("Loja_Nova").join("public")).unwrap();
        fs::write(www.join("index.php"), "<?php echo 'raiz';").unwrap();
        fs::write(www.join("blog").join("index.php"), "<?php echo 'blog-' . PHP_SAPI;").unwrap();
        fs::write(www.join("Loja_Nova").join("public").join("index.php"), "<?php echo 'loja-' . PHP_SAPI;").unwrap();

        confgen::generate(&cfg, &layout, true).unwrap();
        let list = sites(&cfg);
        println!("sites: {:?}", list.iter().map(|s| &s.url).collect::<Vec<_>>());
        write_configs(&cfg, &layout, &list).unwrap();
        println!("hosts (não gravado): {:?}", wanted_hosts(&cfg, &list));

        let m = crate::services::Manager::default();
        m.start(&cfg, &layout, "apache").unwrap_or_else(|e| panic!("{e}"));
        m.start(&cfg, &layout, "nginx").unwrap_or_else(|e| panic!("{e}"));
        for (port, server, sapi) in [(18090, "Apache", "apache2handler"), (18091, "Nginx", "cgi-fcgi")] {
            let blog = get(port, "blog.test");
            let loja = get(port, "loja-nova.test");
            let sub = get(port, "www.blog.test");
            let root = get(port, "localhost");
            println!("{server}: blog.test={blog}  loja-nova.test={loja}  www.blog.test={sub}  localhost={root}");
            assert_eq!(blog, format!("blog-{sapi}"));
            assert_eq!(loja, format!("loja-{sapi}"));
            assert_eq!(sub, format!("blog-{sapi}"));
            assert_eq!(root, "raiz");
        }
        for id in m.running_ids().iter().rev() {
            m.stop(id);
        }
        let _ = fs::remove_dir_all(&layout.base);
    }

    /// HTTPS de verdade no Apache e no Nginx. O cliente confia só na CA do
    /// JJPHPMY (sem instalar nada no Windows) e verifica cadeia, nomes e as
    /// name constraints. `cargo test live_https -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_https() {
        use std::sync::Arc;
        let layout = Layout { base: std::env::temp_dir().join("jjphpmy-live-https") };
        let _ = fs::remove_dir_all(&layout.base);
        let catalog = crate::catalog::Catalog::embedded();
        let emit: crate::installer::Emit = Arc::new(|_| {});
        let mut cfg = Config::default();
        cfg.settings.document_root = layout.dir(&["www"]).display().to_string();
        cfg.settings.data_dir = layout.dir(&["data"]).display().to_string();
        for (id, port, ssl) in [("apache", 18100, 18443), ("nginx", 18101, 18444)] {
            let s = cfg.services.iter_mut().find(|s| s.id == id).unwrap();
            s.port = port;
            s.ssl_port = Some(ssl);
            s.ssl_enabled = true;
        }
        for (id, variant) in [("apache", None), ("php", Some("ts")), ("php", Some("nts")), ("nginx", None)] {
            let (bin, tmp) = (layout.dir(&["bin"]), layout.dir(&["tmp"]));
            let fut = crate::installer::install_in(emit.clone(), catalog.get(id).unwrap(), None, variant, &bin, &tmp);
            let p = tauri::async_runtime::block_on(fut).unwrap_or_else(|e| panic!("{id}: {e}")).unwrap();
            cfg.installed.push(InstalledPackage { active: true, ..p });
        }
        let www = layout.dir(&["www"]);
        fs::create_dir_all(www.join("blog")).unwrap();
        fs::write(www.join("index.php"), "<?php echo 'raiz-' . ($_SERVER['HTTPS'] ?? 'off');").unwrap();
        fs::write(www.join("blog").join("index.php"), "<?php echo 'blog-' . PHP_SAPI . '-' . ($_SERVER['HTTPS'] ?? 'off');").unwrap();

        let list = sites(&cfg);
        crate::ssl::ensure_server(&layout, &wanted_hosts(&cfg, &list)).unwrap();
        confgen::generate(&cfg, &layout, true).unwrap();
        write_configs(&cfg, &layout, &list).unwrap();
        println!("sites: {:?}", list.iter().map(|s| s.https_url.clone()).collect::<Vec<_>>());

        let m = crate::services::Manager::default();
        m.start(&cfg, &layout, "apache").unwrap_or_else(|e| panic!("{e}"));
        m.start(&cfg, &layout, "nginx").unwrap_or_else(|e| panic!("{e}"));

        let ca = reqwest::Certificate::from_pem(&fs::read(crate::ssl::ca_cert(&layout)).unwrap()).unwrap();
        let local: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
        let client = reqwest::Client::builder()
            .tls_built_in_root_certs(false)
            .add_root_certificate(ca)
            .resolve("blog.test", local)
            .resolve("www.blog.test", local)
            .build()
            .unwrap();
        let fetch = |url: String| {
            tauri::async_runtime::block_on(async { client.get(&url).send().await?.text().await })
                .unwrap_or_else(|e| panic!("{url}: {e:?}"))
        };
        for (server, port, sapi) in [("Apache", 18443, "apache2handler"), ("Nginx", 18444, "cgi-fcgi")] {
            let blog = fetch(format!("https://blog.test:{port}/"));
            let sub = fetch(format!("https://www.blog.test:{port}/"));
            let root = fetch(format!("https://localhost:{port}/"));
            println!("{server}: https://blog.test={blog}  https://www.blog.test={sub}  https://localhost={root}");
            assert!(blog.starts_with(&format!("blog-{sapi}")));
            assert_eq!(sub, blog);
            assert!(root.starts_with("raiz"));
        }
        // Apache informa HTTPS=on ao PHP.
        assert!(fetch("https://blog.test:18443/".into()).ends_with("-on"));
        for id in m.running_ids().iter().rev() {
            m.stop(id);
        }
        let _ = fs::remove_dir_all(&layout.base);
    }

    #[test]
    fn slugs_folder_names() {
        assert_eq!(slug("Meu_Site"), "meu-site");
        assert_eq!(slug("loja  nova"), "loja-nova");
        assert_eq!(slug("--x--"), "x");
        assert_eq!(slug("Ação Social"), "acao-social");
        assert_eq!(slug("日本"), "");
    }

    #[test]
    fn merges_hosts_block() {
        let original = "# Copyright (c) Microsoft\r\n127.0.0.1 localhost\r\n";
        let with = merge_hosts(original, &["blog.test".into(), "loja.test".into()]);
        assert_eq!(
            with,
            "# Copyright (c) Microsoft\r\n127.0.0.1 localhost\r\n\r\n# jjphpmy begin\r\n127.0.0.1\tblog.test\r\n127.0.0.1\tloja.test\r\n# jjphpmy end\r\n"
        );
        // Trocar a lista mantém o resto; esvaziar remove o bloco.
        let changed = merge_hosts(&with, &["api.test".into()]);
        assert!(changed.contains("api.test") && !changed.contains("blog.test"));
        assert_eq!(merge_hosts(&with, &[]), original);
        // Linhas de outros programas depois do bloco continuam lá.
        let other = format!("{with}10.0.0.5 servidor\r\n");
        assert!(merge_hosts(&other, &[]).contains("10.0.0.5 servidor"));
    }

    fn temp_cfg(name: &str) -> (Config, Layout) {
        let base = std::env::temp_dir().join(format!("jjphpmy-vh-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let layout = Layout { base: base.clone() };
        let mut cfg = Config::default();
        cfg.settings.document_root = layout.dir(&["www"]).display().to_string();
        for (id, variant) in [("apache", None), ("nginx", None), ("php", Some("nts"))] {
            let path = layout.dir(&["bin", id, "1"]);
            fs::create_dir_all(&path).unwrap();
            cfg.installed.push(InstalledPackage {
                id: id.into(),
                version: "1".into(),
                active: true,
                variant: variant.map(Into::into),
                path: path.display().to_string(),
            });
        }
        fs::create_dir_all(layout.dir(&["www", "blog"])).unwrap();
        fs::create_dir_all(layout.dir(&["www", "Loja_Nova", "public"])).unwrap();
        fs::create_dir_all(layout.dir(&["www", ".git"])).unwrap();
        (cfg, layout)
    }

    #[test]
    fn lists_sites_and_uses_public() {
        let (mut cfg, layout) = temp_cfg("sites");
        cfg.services.iter_mut().find(|s| s.id == "apache").unwrap().port = 8888;
        let list = sites(&cfg);
        assert_eq!(list.iter().map(|s| s.host.as_str()).collect::<Vec<_>>(), ["blog.test", "loja-nova.test"]);
        assert!(list[1].root.ends_with("Loja_Nova/public"));
        assert_eq!(list[0].url, "http://blog.test:8888/");
        let _ = fs::remove_dir_all(&layout.base);
    }

    #[test]
    fn writes_and_cleans_auto_files() {
        let (mut cfg, layout) = temp_cfg("files");
        let list = sites(&cfg);
        let user_file = layout.dir(&["etc", "apache", "sites-enabled", "meu.conf"]);
        fs::create_dir_all(user_file.parent().unwrap()).unwrap();
        fs::write(&user_file, "# do usuário").unwrap();

        assert_eq!(write_configs(&cfg, &layout, &list).unwrap(), ["apache", "nginx"]);
        let apache = fs::read_to_string(layout.dir(&["etc", "apache", "sites-enabled", "auto.blog.test.conf"])).unwrap();
        assert!(apache.contains("ServerName blog.test") && apache.contains("<VirtualHost *:80>"));
        let nginx = fs::read_to_string(layout.dir(&["etc", "nginx", "sites-enabled", "auto.blog.test.conf"])).unwrap();
        assert!(nginx.contains("server_name  blog.test *.blog.test;") && nginx.contains("fastcgi_pass"));
        assert!(!nginx.contains("{{"));
        // Sem mudanças: nada a reiniciar.
        assert!(write_configs(&cfg, &layout, &list).unwrap().is_empty());
        // Pasta apagada: o arquivo dela some; o do usuário fica.
        fs::remove_dir_all(layout.dir(&["www", "blog"])).unwrap();
        write_configs(&cfg, &layout, &sites(&cfg)).unwrap();
        assert!(!layout.dir(&["etc", "apache", "sites-enabled", "auto.blog.test.conf"]).exists());
        assert!(user_file.exists());
        // Opção desligada: remove todos os auto.*.
        cfg.settings.auto_virtual_hosts = false;
        write_configs(&cfg, &layout, &sites(&cfg)).unwrap();
        assert!(!layout.dir(&["etc", "apache", "sites-enabled", "auto.00-default.conf"]).exists());
        let _ = fs::remove_dir_all(&layout.base);
    }
}
