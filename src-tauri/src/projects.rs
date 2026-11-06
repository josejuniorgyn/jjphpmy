//! "Novo projeto" (como o Quick app do Laragon): cria um projeto em
//! `www\{nome}` a partir de um modelo, usando o Composer e o npm com o PATH
//! do JJPHPMY. O vigia da Etapa 6 cria o site `{nome}.test` sozinho.

use crate::confgen::Layout;
use crate::config::Config;
use crate::installer::hidden;
use crate::services::child_env;
use serde::Serialize;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Template {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    /// Comandos que precisam existir no PATH: "php", "composer", "npm".
    pub needs: &'static [&'static str],
}

pub const TEMPLATES: &[Template] = &[
    Template {
        id: "laravel-vue",
        name: "Laravel + Vue (Inertia)",
        description: "Kit oficial do Laravel com Vue 3, TypeScript, Tailwind e login pronto.",
        needs: &["php", "composer", "npm"],
    },
    Template {
        id: "laravel-react",
        name: "Laravel + React (Inertia)",
        description: "Kit oficial do Laravel com React, TypeScript, Tailwind e login pronto.",
        needs: &["php", "composer", "npm"],
    },
    Template {
        id: "laravel-livewire",
        name: "Laravel + Livewire",
        description: "Kit oficial do Laravel com Livewire (telas interativas em PHP) e login pronto.",
        needs: &["php", "composer", "npm"],
    },
    Template {
        id: "laravel",
        name: "Laravel",
        description: "Laravel sem frontend: API ou projeto que você monta do seu jeito.",
        needs: &["php", "composer"],
    },
    Template {
        id: "symfony",
        name: "Symfony",
        description: "Esqueleto mínimo do Symfony.",
        needs: &["php", "composer"],
    },
    Template { id: "vite-vue", name: "Vite + Vue", description: "Frontend Vue 3 com Vite.", needs: &["npm"] },
    Template { id: "vite-react", name: "Vite + React", description: "Frontend React com Vite.", needs: &["npm"] },
    Template { id: "vite-svelte", name: "Vite + Svelte", description: "Frontend Svelte com Vite.", needs: &["npm"] },
    Template {
        id: "wordpress",
        name: "WordPress",
        description: "WordPress em português, com o banco de dados criado no MySQL/MariaDB e o wp-config.php pronto.",
        needs: &[],
    },
    Template { id: "php", name: "PHP puro", description: "Só a pasta com um index.php.", needs: &[] },
];

/// Nome da pasta: letras minúsculas, números, hífen e sublinhado.
pub fn validate_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 60
        && name.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err("Use só letras minúsculas sem acento, números, hífen ou sublinhado no nome (ex.: minha-loja).".into())
    }
}

/// Passos de cada modelo: (pasta onde rodar: `None` = www, `Some` = projeto; comando).
pub fn steps(template: &str, name: &str) -> Vec<(bool, String)> {
    let kit = |pkg: &str| {
        vec![
            (false, format!("composer create-project {pkg} {name}")),
            (true, "npm install".to_string()),
            (true, "npm run build".to_string()),
        ]
    };
    let vite = |t: &str| {
        vec![
            (false, format!("npm create vite@latest {name} -- --template {t}")),
            (true, "npm install".to_string()),
        ]
    };
    match template {
        "laravel-vue" => kit("laravel/vue-starter-kit"),
        "laravel-react" => kit("laravel/react-starter-kit"),
        "laravel-livewire" => kit("laravel/livewire-starter-kit"),
        "laravel" => vec![(false, format!("composer create-project laravel/laravel {name}"))],
        "symfony" => vec![(false, format!("composer create-project symfony/skeleton {name}"))],
        "vite-vue" => vite("vue"),
        "vite-react" => vite("react"),
        "vite-svelte" => vite("svelte"),
        _ => Vec::new(),
    }
}

/// Tira as cores do terminal (sequências ANSI) das linhas do log.
pub fn strip_ansi(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for n in chars.by_ref() {
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

fn env(cfg: &Config, layout: &Layout) -> Vec<(String, String)> {
    let mut env = child_env(cfg, layout);
    // Sem perguntas e sem cores: a saída vai para o log da janela.
    for (k, v) in [("COMPOSER_NO_INTERACTION", "1"), ("CI", "1"), ("npm_config_yes", "true"), ("NO_COLOR", "1")] {
        env.push((k.into(), v.into()));
    }
    env
}

/// O comando existe no PATH que o projeto vai usar?
fn available(program: &str, env: &[(String, String)]) -> bool {
    hidden(Command::new("cmd").args(["/D", "/C", "where", program]).envs(env.iter().cloned()))
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Roda um comando (via cmd, por causa do npm.cmd e do composer.bat) e
/// manda cada linha da saída para `log`.
fn run(cmdline: &str, cwd: &Path, env: &[(String, String)], log: &dyn Fn(String)) -> Result<(), String> {
    log(format!("> {cmdline}"));
    let mut child = hidden(
        Command::new("cmd")
            .args(["/D", "/C", cmdline])
            .current_dir(cwd)
            .envs(env.iter().cloned())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .spawn()
    .map_err(|e| format!("Não foi possível executar \"{cmdline}\": {e}"))?;
    // stderr numa thread, stdout aqui: os dois aparecem no log enquanto rodam.
    let stderr = child.stderr.take().map(|e| {
        std::thread::spawn(move || {
            let mut lines = Vec::new();
            for line in BufReader::new(e).lines().map_while(Result::ok) {
                lines.push(line);
            }
            lines
        })
    });
    if let Some(out) = child.stdout.take() {
        let mut reader = BufReader::new(out);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.by_ref().take(64 * 1024).read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let line = strip_ansi(String::from_utf8_lossy(&buf).trim_end());
                    if !line.trim().is_empty() {
                        log(line);
                    }
                }
            }
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let err_lines = stderr.and_then(|h| h.join().ok()).unwrap_or_default();
    for line in &err_lines {
        let line = strip_ansi(line);
        if !line.trim().is_empty() {
            log(line);
        }
    }
    if status.success() {
        Ok(())
    } else {
        Err(format!("\"{cmdline}\" terminou com erro ({status}). Veja o log acima."))
    }
}

/* ---------- WordPress ---------- */

const WORDPRESS_ZIP: &str = "https://br.wordpress.org/latest-pt_BR.zip";

/// Nome do banco a partir da pasta ("minha-loja" -> "minha_loja").
pub fn db_name(name: &str) -> String {
    name.replace('-', "_")
}

/// 64 caracteres aleatórios para as chaves do wp-config.php.
fn random_key() -> String {
    use std::hash::{BuildHasher, Hasher};
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!@#%^&*()-_=+[]{}<>~;:,.?|";
    let mut out = String::new();
    while out.len() < 64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_usize(out.len());
        for b in h.finish().to_le_bytes() {
            out.push(CHARS[b as usize % CHARS.len()] as char);
        }
    }
    out.truncate(64);
    out
}

/// wp-config.php a partir do wp-config-sample.php (em português ou inglês).
pub fn wp_config(sample: &str, db: &str, host: &str) -> String {
    let mut s = sample
        .replace("'nome_do_banco_de_dados_aqui'", &format!("'{db}'"))
        .replace("'database_name_here'", &format!("'{db}'"))
        .replace("'nome_de_usuario_aqui'", "'root'")
        .replace("'username_here'", "'root'")
        .replace("'senha_aqui'", "''")
        .replace("'password_here'", "''")
        .replace("define( 'DB_HOST', 'localhost' );", &format!("define( 'DB_HOST', '{host}' );"))
        .replace("define( 'DB_CHARSET', 'utf8' );", "define( 'DB_CHARSET', 'utf8mb4' );");
    for phrase in ["'coloque a sua frase única aqui'", "'put your unique phrase here'"] {
        while let Some(pos) = s.find(phrase) {
            s.replace_range(pos..pos + phrase.len(), &format!("'{}'", random_key()));
        }
    }
    s
}

/// Banco ativo (MySQL ou MariaDB): (cliente mysql/mariadb.exe, porta).
fn db_client(cfg: &Config) -> Result<(PathBuf, String), String> {
    let port = crate::confgen::db_port(cfg);
    let maria_port = cfg.service("mariadb").map(|s| s.port.to_string());
    let id = if maria_port.as_deref() == Some(port.as_str()) && cfg.installed.iter().any(|p| p.id == "mariadb" && p.active) {
        "mariadb"
    } else {
        "mysql"
    };
    let pkg = cfg
        .installed
        .iter()
        .find(|p| p.id == id && p.active)
        .ok_or("Instale o MySQL ou o MariaDB na aba Pacotes para criar um WordPress.")?;
    let bin = Path::new(&pkg.path).join("bin");
    let exe = ["mariadb.exe", "mysql.exe"].iter().map(|n| bin.join(n)).find(|p| p.exists()).ok_or("Cliente do banco (mysql.exe) não encontrado.")?;
    Ok((exe, port))
}

/// Baixa o WordPress em português, extrai em `dir`, cria o banco e o wp-config.php.
fn wordpress(cfg: &Config, dir: &Path, name: &str, log: &dyn Fn(String)) -> Result<(), String> {
    let (client, port) = db_client(cfg)?;
    let up = std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port.parse::<u16>().unwrap_or(3306))),
        std::time::Duration::from_millis(500),
    )
    .is_ok();
    if !up {
        return Err("Inicie o MySQL (ou o MariaDB) em Serviços e Portas antes de criar um WordPress.".into());
    }

    log(format!("Baixando {WORDPRESS_ZIP}..."));
    let tmp = std::env::temp_dir().join(format!("jjphpmy-wordpress-{}.zip", std::process::id()));
    let bytes = tauri::async_runtime::block_on(async {
        let c = reqwest::Client::builder()
            .user_agent(concat!("jjphpmy/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .map_err(|e| e.to_string())?;
        c.get(WORDPRESS_ZIP)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| format!("Falha ao baixar o WordPress: {e}"))?
            .bytes()
            .await
            .map_err(|e| e.to_string())
    })?;
    fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
    log(format!("Extraindo em {}...", dir.display()));
    let result = crate::installer::extract(&tmp, dir, |_, _| {});
    let _ = fs::remove_file(&tmp);
    result?;

    let db = db_name(name);
    log(format!("Criando o banco {db}..."));
    let out = hidden(
        Command::new(&client)
            .args(["-u", "root", "-h", "127.0.0.1", "-P", &port, "-e"])
            .arg(format!("CREATE DATABASE IF NOT EXISTS `{db}` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci")),
    )
    .output()
    .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("Não foi possível criar o banco {db}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }

    let sample = fs::read_to_string(dir.join("wp-config-sample.php")).map_err(|e| format!("wp-config-sample.php: {e}"))?;
    fs::write(dir.join("wp-config.php"), wp_config(&sample, &db, &format!("127.0.0.1:{port}"))).map_err(|e| e.to_string())?;
    log("wp-config.php criado (banco, usuário root sem senha e chaves de segurança).".into());
    Ok(())
}

/// Cria o projeto `name` com o modelo `template` em www. Devolve a mensagem final.
pub fn create(cfg: &Config, layout: &Layout, template: &str, name: &str, log: &dyn Fn(String)) -> Result<String, String> {
    validate_name(name)?;
    let t = TEMPLATES.iter().find(|t| t.id == template).ok_or_else(|| format!("Modelo desconhecido: {template}"))?;
    let www = PathBuf::from(&cfg.settings.document_root);
    let dir = www.join(name);
    if dir.exists() {
        return Err(format!("Já existe a pasta {}. Escolha outro nome.", dir.display()));
    }
    let env = env(cfg, layout);
    let missing: Vec<&str> = t
        .needs
        .iter()
        .copied()
        .filter(|p| !available(p, &env))
        .map(|p| match p {
            "npm" => "Node.js",
            "composer" => "Composer",
            _ => "PHP",
        })
        .collect();
    if !missing.is_empty() {
        return Err(format!("Para o modelo {}, instale antes na aba Pacotes: {}.", t.name, missing.join(", ")));
    }
    fs::create_dir_all(&www).map_err(|e| format!("Não foi possível criar {}: {e}", www.display()))?;

    if template == "wordpress" {
        wordpress(cfg, &dir, name, log)?;
        let host = cfg.settings.hostname_pattern.replace("{name}", &crate::vhosts::slug(name));
        return Ok(format!(
            "WordPress {name} criado em {}, com o banco {} no MySQL/MariaDB. Abra http://{host}/ para escolher o título do site e o usuário administrador.",
            dir.display(),
            db_name(name)
        ));
    }
    if template == "php" {
        fs::create_dir_all(&dir).map_err(|e| format!("Não foi possível criar {}: {e}", dir.display()))?;
        fs::write(dir.join("index.php"), format!("<?php\n// Projeto {name}, criado pelo JJPHPMY.\necho 'Olá, {name}!';\n"))
            .map_err(|e| e.to_string())?;
    } else {
        log(format!("Criando {} em {}...", t.name, dir.display()));
        for (in_project, cmdline) in steps(template, name) {
            let cwd = if in_project { &dir } else { &www };
            if let Err(e) = run(&cmdline, cwd, &env, log) {
                return Err(format!("{e} A pasta {} pode ter ficado incompleta: apague-a antes de tentar de novo.", dir.display()));
            }
        }
    }
    let host = cfg.settings.hostname_pattern.replace("{name}", &crate::vhosts::slug(name));
    let dev = if template.starts_with("vite") {
        format!(" Para desenvolver, rode \"npm run dev\" no Terminal, dentro de {}.", dir.display())
    } else {
        String::new()
    };
    Ok(format!("Projeto {name} ({}) criado em {}. Site: http://{host}/{dev}", t.name, dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_names() {
        assert!(validate_name("minha-loja").is_ok());
        assert!(validate_name("api_2").is_ok());
        for bad in ["", "Minha", "-x", "loja nova", "ação", "a/b", "..", "x&y"] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn every_template_has_steps() {
        for t in TEMPLATES {
            assert_eq!(steps(t.id, "x").is_empty(), t.id == "php" || t.id == "wordpress", "{}", t.id);
        }
        assert_eq!(steps("vite-svelte", "app")[0].1, "npm create vite@latest app -- --template svelte");
    }

    #[test]
    fn builds_wp_config() {
        let sample = "define( 'DB_NAME', 'nome_do_banco_de_dados_aqui' );\ndefine( 'DB_USER', 'nome_de_usuario_aqui' );\n\
                      define( 'DB_PASSWORD', 'senha_aqui' );\ndefine( 'DB_HOST', 'localhost' );\ndefine( 'DB_CHARSET', 'utf8' );\n\
                      define( 'AUTH_KEY',         'coloque a sua frase única aqui' );\ndefine( 'NONCE_SALT',       'coloque a sua frase única aqui' );\n";
        let c = wp_config(sample, "minha_loja", "127.0.0.1:3306");
        assert!(c.contains("'DB_NAME', 'minha_loja'") && c.contains("'DB_USER', 'root'") && c.contains("'DB_PASSWORD', ''"));
        assert!(c.contains("'DB_HOST', '127.0.0.1:3306'") && c.contains("'utf8mb4'"));
        assert!(!c.contains("frase única"));
        assert_eq!(db_name("minha-loja"), "minha_loja");
        assert_eq!(random_key().len(), 64);
    }

    #[test]
    fn strips_terminal_colors() {
        assert_eq!(strip_ansi("\u{1b}[32mOK\u{1b}[0m pronto"), "OK pronto");
    }

    /// Cria projetos de verdade (Laravel + Vue, Vite + React e PHP puro) numa
    /// pasta temporária, com o PHP, o Composer e o Node.js que estiverem no PATH.
    /// `cargo test live_projects -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_projects() {
        let base = std::env::temp_dir().join("jjphpmy-live-proj");
        let _ = fs::remove_dir_all(&base);
        // PHP e php.ini (PHPRC) do JJPHPMY deste PC; Composer novo numa pasta temporária.
        let layout = Layout::system();
        let mut cfg = Config::default();
        cfg.settings.document_root = base.join("www").display().to_string();
        cfg.installed = crate::config::Config::load().installed;
        cfg.installed.retain(|p| p.id != "composer");
        let catalog = crate::catalog::Catalog::embedded();
        let emit: crate::installer::Emit = std::sync::Arc::new(|_| {});
        let (bin, tmp) = (base.join("bin"), base.join("tmp"));
        let composer = tauri::async_runtime::block_on(crate::installer::install_in(emit, catalog.get("composer").unwrap(), None, None, &bin, &tmp))
            .unwrap()
            .unwrap();
        println!("Composer {} em {}", composer.version, composer.path);
        cfg.installed.push(crate::config::InstalledPackage { active: true, ..composer });
        let log = |s: String| println!("  | {s}");
        for (t, name, check) in [
            ("php", "simples", "index.php"),
            ("vite-react", "painel", "node_modules"),
            ("laravel-vue", "loja", "public/build/manifest.json"),
        ] {
            let r = create(&cfg, &layout, t, name, &log).unwrap_or_else(|e| panic!("{t}: {e}"));
            println!("{r}");
            assert!(base.join("www").join(name).join(check).exists(), "{t}: falta {check}");
        }
        let _ = fs::remove_dir_all(&base);
    }
}
