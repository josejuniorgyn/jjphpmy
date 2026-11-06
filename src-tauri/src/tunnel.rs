//! "Compartilhar site": link público temporário `https://*.trycloudflare.com`
//! para um site local, com o `cloudflared` (Cloudflare Tunnel, sem conta).
//! Cada túnel roda num Job Object e para junto com o JJPHPMY.

use crate::config::Config;
use crate::installer::hidden;
use crate::services::Job;
use regex::Regex;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::Duration;

#[derive(Default)]
pub struct Tunnels {
    map: Mutex<HashMap<String, (Child, Job, String)>>,
}

/// Procura o link do túnel numa linha da saída do cloudflared.
pub fn find_url(line: &str) -> Option<String> {
    Regex::new(r"https://[a-z0-9-]+\.trycloudflare\.com").unwrap().find(line).map(|m| m.as_str().to_string())
}

impl Tunnels {
    /// Links ativos: host -> URL pública.
    pub fn list(&self) -> HashMap<String, String> {
        let mut map = self.map.lock().unwrap();
        // Túneis que pararam sozinhos saem da lista.
        map.retain(|_, (child, _, _)| matches!(child.try_wait(), Ok(None)));
        map.iter().map(|(h, (_, _, u))| (h.clone(), u.clone())).collect()
    }

    /// Abre um túnel para `host` (servido pelo Apache/Nginx em `port`). Devolve o link.
    pub fn share(&self, cfg: &Config, host: &str, port: u16) -> Result<String, String> {
        if let Some(url) = self.list().get(host) {
            return Ok(url.clone());
        }
        let exe = cfg
            .installed
            .iter()
            .find(|p| p.id == "cloudflared" && p.active)
            .map(|p| Path::new(&p.path).join("cloudflared.exe"))
            .filter(|p| p.exists())
            .ok_or("Instale o Cloudflare Tunnel na aba Pacotes (Ferramentas) para compartilhar sites.")?;
        let mut child = hidden(
            Command::new(exe)
                .args(["tunnel", "--no-autoupdate", "--url", &format!("http://127.0.0.1:{port}"), "--http-host-header", host])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped()),
        )
        .spawn()
        .map_err(|e| format!("Não foi possível iniciar o cloudflared: {e}"))?;
        let job = Job::new()?;
        job.assign(&child);

        // O link aparece na saída de erro; a thread continua lendo até o fim
        // para o cloudflared não travar com a saída cheia.
        let (tx, rx) = mpsc::channel();
        let stderr = child.stderr.take().ok_or("Sem saída do cloudflared.")?;
        std::thread::spawn(move || {
            let mut sent = false;
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if !sent {
                    if let Some(url) = find_url(&line) {
                        let _ = tx.send(url);
                        sent = true;
                    }
                }
            }
        });
        match rx.recv_timeout(Duration::from_secs(40)) {
            Ok(url) => {
                self.map.lock().unwrap().insert(host.to_string(), (child, job, url.clone()));
                Ok(url)
            }
            Err(_) => {
                job.terminate();
                let _ = child.wait();
                Err("O Cloudflare não devolveu o link em 40 s. Confira a internet e tente de novo.".into())
            }
        }
    }

    pub fn stop(&self, host: &str) {
        if let Some((mut child, job, _)) = self.map.lock().unwrap().remove(host) {
            job.terminate();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_tunnel_url() {
        let line = "2026-11-05T10:00:00Z INF |  https://clever-fox-12-ab.trycloudflare.com                                 |";
        assert_eq!(find_url(line).as_deref(), Some("https://clever-fox-12-ab.trycloudflare.com"));
        assert_eq!(find_url("INF Requesting new quick Tunnel on trycloudflare.com..."), None);
    }

    /// Túnel de verdade para um servidor PHP local; busca a página pelo link público.
    /// `cargo test live_tunnel -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_tunnel() {
        let cfg = Config::load();
        let php = cfg.installed.iter().find(|p| p.id == "php" && p.active).expect("PHP instalado");
        let dir = std::env::temp_dir().join("jjphpmy-live-tunnel");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("index.php"), "<?php echo 'ola-do-tunel-' . $_SERVER['HTTP_HOST'];").unwrap();
        let mut server = Command::new(Path::new(&php.path).join("php.exe"))
            .args(["-S", "127.0.0.1:18777", "-t"])
            .arg(&dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let t = Tunnels::default();
        let url = t.share(&cfg, "site-teste.test", 18777).unwrap_or_else(|e| panic!("{e}"));
        println!("link: {url}");
        // O DNS do link novo leva alguns segundos para responder.
        let mut body = String::new();
        for _ in 0..20 {
            std::thread::sleep(Duration::from_secs(3));
            let r = tauri::async_runtime::block_on(async { reqwest::get(&url).await?.text().await });
            if let Ok(b) = r {
                if b.contains("ola-do-tunel") {
                    body = b;
                    break;
                }
            }
        }
        println!("resposta: {body}");
        t.stop("site-teste.test");
        let _ = server.kill();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(body, "ola-do-tunel-site-teste.test");
    }
}
