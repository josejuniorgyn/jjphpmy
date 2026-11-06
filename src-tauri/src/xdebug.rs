//! Xdebug (depuração do PHP): a DLL certa para cada PHP instalado
//! (versão maior.menor, TS/NTS e compilador) vem de https://xdebug.org/download,
//! com o SHA-256 publicado na própria página, e fica em `ext\php_xdebug.dll`.
//! O `confgen` só acrescenta o `zend_extension` quando a opção está ligada.

use crate::config::InstalledPackage;
use regex::Regex;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

const PAGE: &str = "https://xdebug.org/download";

#[derive(Debug, Clone, PartialEq)]
pub struct Dll {
    pub version: String,
    /// "8.3"
    pub php: String,
    /// "ts" | "nts"
    pub variant: String,
    pub url: String,
    pub sha256: String,
}

/// Links das DLLs Windows x64 na página de downloads, na ordem da página
/// (mais nova primeiro). Cada link vem com `title="SHA256:&nbsp;{hash}"`.
pub fn parse_page(html: &str) -> Vec<Dll> {
    let re = Regex::new(
        r#"SHA256:&nbsp;([0-9a-f]{64})"\s*href='(/files/php_xdebug-(\d+\.\d+\.\d+)-(\d+\.\d+)-(ts|nts)-vs\d+-x86_64\.dll)'"#,
    )
    .unwrap();
    re.captures_iter(html)
        .map(|c| Dll {
            version: c[3].to_string(),
            php: c[4].to_string(),
            variant: c[5].to_string(),
            url: format!("https://xdebug.org{}", &c[2]),
            sha256: c[1].to_string(),
        })
        .collect()
}

/// "8.3.36-1" -> "8.3"
fn php_line(version: &str) -> String {
    version.split(['.', '-']).take(2).collect::<Vec<_>>().join(".")
}

/// DLL para um PHP instalado (a mais nova da página para a linha e variante).
pub fn pick<'a>(dlls: &'a [Dll], php: &InstalledPackage) -> Option<&'a Dll> {
    let line = php_line(&php.version);
    let variant = php.variant.as_deref().unwrap_or("ts");
    dlls.iter().find(|d| d.php == line && d.variant == variant)
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(concat!("jjphpmy/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| e.to_string())
}

/// Baixa com até 3 tentativas (a DLL é pequena; falhas de rede passageiras
/// não devem impedir de ligar o Xdebug).
async fn download(c: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    let mut last = String::new();
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
        let r = async { c.get(url).send().await?.error_for_status()?.bytes().await }.await;
        match r {
            Ok(b) => return Ok(b.to_vec()),
            Err(e) => last = e.to_string(),
        }
    }
    Err(format!("Falha ao baixar {url}: {last}. Confira a internet e tente de novo."))
}

/// Garante `ext\php_xdebug.dll` nos PHPs informados. Devolve o que foi feito.
pub async fn ensure(phps: &[InstalledPackage]) -> Result<Vec<String>, String> {
    let missing: Vec<&InstalledPackage> =
        phps.iter().filter(|p| !Path::new(&p.path).join("ext").join("php_xdebug.dll").exists()).collect();
    if missing.is_empty() {
        return Ok(Vec::new());
    }
    let c = client()?;
    let html = c
        .get(PAGE)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("Não foi possível acessar {PAGE}: {e}"))?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    let dlls = parse_page(&html);
    let mut done = Vec::new();
    for p in missing {
        let variant = p.variant.as_deref().unwrap_or("ts").to_uppercase();
        let dll = pick(&dlls, p).ok_or_else(|| {
            format!("O site do Xdebug não tem versão para o PHP {} {variant}.", php_line(&p.version))
        })?;
        let bytes = download(&c, &dll.url).await?;
        let got = format!("{:x}", Sha256::digest(&bytes));
        if got != dll.sha256 {
            return Err(format!("O arquivo do Xdebug {} veio corrompido (SHA-256 não confere). Tente de novo.", dll.version));
        }
        let target = Path::new(&p.path).join("ext").join("php_xdebug.dll");
        fs::write(&target, &bytes).map_err(|e| format!("Não foi possível gravar {}: {e}", target.display()))?;
        done.push(format!("Xdebug {} para o PHP {} {variant}", dll.version, p.version));
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HTML: &str = r#"<a title="SHA256:&nbsp;a3ffd4d5b1f9ca0766b1f3ed321246808a51ce4ba49dc32ae195bfafc4d4d9a2" href='/files/php_xdebug-3.5.3-8.5-ts-vs17-x86_64.dll'>PHP 8.5 TS</a>
<a title="SHA256:&nbsp;48398f54d7bf311728eafeb9999a5a55be5b5feb83b9af8f3115fef4b71dd2f7" href='/files/php_xdebug-3.5.3-8.3-ts-vs16-x86_64.dll'>PHP 8.3 TS</a>
<a title="SHA256:&nbsp;f073de91bea046106abf4d6071c963ea71e58571df6ce58948ceca89d121cb2d" href='/files/php_xdebug-3.5.3-8.3-nts-vs16-x86_64.dll'>PHP 8.3 NTS</a>
<a title="SHA256:&nbsp;0000000000000000000000000000000000000000000000000000000000000000" href='/files/php_xdebug-3.4.0-8.3-ts-vs16-x86_64.dll'>antiga</a>"#;

    fn php(version: &str, variant: &str) -> InstalledPackage {
        InstalledPackage { id: "php".into(), version: version.into(), active: true, variant: Some(variant.into()), path: String::new() }
    }

    #[test]
    fn picks_newest_for_line_and_variant() {
        let dlls = parse_page(HTML);
        assert_eq!(dlls.len(), 4);
        let d = pick(&dlls, &php("8.3.36-1", "ts")).unwrap();
        assert_eq!((d.version.as_str(), d.variant.as_str()), ("3.5.3", "ts"));
        assert!(d.url.ends_with("php_xdebug-3.5.3-8.3-ts-vs16-x86_64.dll"));
        assert_eq!(pick(&dlls, &php("8.3.36-1", "nts")).unwrap().sha256.len(), 64);
        assert!(pick(&dlls, &php("7.4.33", "ts")).is_none());
    }
}
