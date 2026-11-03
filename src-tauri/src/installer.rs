//! Etapa 3: download com progresso, verificação SHA-256 e extração dos
//! pacotes em `C:\jjphpmy\bin\{id}\{versão}`.
//!
//! As funções puras (`parse_checksum`, `zip_root`, `install_dir`) têm testes;
//! o resto faz I/O e emite eventos `install-progress` para a interface.

use crate::catalog::Package;
use crate::config::{base_path, InstalledPackage};
use crate::versions::{self, Download, VersionInfo};
use regex::Regex;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

/// Evento `install-progress` enviado à interface.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub id: String,
    pub version: String,
    /// "download" | "verify" | "extract" | "done" | "skipped" | "error"
    pub stage: String,
    pub received: u64,
    pub total: Option<u64>,
    pub message: Option<String>,
}

/// Destino dos eventos de progresso (a janela, ou nada nos testes).
pub type Emit = Arc<dyn Fn(Progress) + Send + Sync>;

#[derive(Clone)]
struct Reporter {
    emit: Emit,
    id: String,
    version: String,
}

impl Reporter {
    fn send(&self, stage: &str, received: u64, total: Option<u64>, message: Option<String>) {
        (self.emit)(Progress {
            id: self.id.clone(),
            version: self.version.clone(),
            stage: stage.into(),
            received,
            total,
            message,
        });
    }

    fn stage(&self, stage: &str, message: impl Into<String>) {
        self.send(stage, 0, None, Some(message.into()));
    }
}

/* ---------- Funções puras ---------- */

/// Pasta de instalação. O PHP guarda a variante no nome, porque TS e NTS
/// da mesma versão podem conviver.
pub fn install_dir(id: &str, version: &str, variant: Option<&str>) -> PathBuf {
    install_dir_in(&base_path(&["bin"]), id, version, variant)
}

fn install_dir_in(bin: &Path, id: &str, version: &str, variant: Option<&str>) -> PathBuf {
    match variant {
        Some(v) => bin.join(id).join(format!("{version}-{v}")),
        None => bin.join(id).join(version),
    }
}

/// Lê o hash SHA-256 de um arquivo de checksums. Formatos aceitos:
/// `sha256sum` (`<hash>  arquivo`, Node.js e phpMyAdmin) e o do
/// Apache Lounge (`SHA256-Checksum for: arquivo:` e o hash na linha seguinte).
pub fn parse_checksum(text: &str, file_name: &str) -> Option<String> {
    let hex = Regex::new(r"\b[0-9a-fA-F]{64}\b").unwrap();
    let lower = text.to_lowercase();
    if let Some(pos) = lower.find("sha256-checksum") {
        return hex.find(&text[pos..]).map(|m| m.as_str().to_lowercase());
    }
    let name = file_name.to_lowercase();
    for line in text.lines() {
        if line.to_lowercase().contains(&name) {
            if let Some(m) = hex.find(line) {
                return Some(m.as_str().to_lowercase());
            }
        }
    }
    // Arquivo com um único hash e sem nome.
    let all: Vec<_> = hex.find_iter(text).collect();
    (all.len() == 1).then(|| all[0].as_str().to_lowercase())
}

/// Pasta raiz a remover dos caminhos do zip: quando há exatamente uma pasta
/// no primeiro nível (ex.: `node-v22.11.0-win-x64/`, `Apache24/`). Arquivos
/// soltos no primeiro nível (ReadMe do Apache Lounge) vão para a raiz também.
pub fn zip_root<'a>(names: impl IntoIterator<Item = &'a Path>) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for name in names {
        let mut parts = name.components();
        let (Some(first), Some(_)) = (parts.next(), parts.next()) else { continue };
        let first = PathBuf::from(first.as_os_str());
        if !dirs.contains(&first) {
            dirs.push(first);
        }
    }
    (dirs.len() == 1).then(|| dirs.remove(0))
}

/// Nome do arquivo no fim da URL.
fn file_name_of(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or("download.bin").to_string()
}

/// Escolhe a versão pedida (ou a mais recente) e o download da variante.
pub fn pick<'a>(
    list: &'a [VersionInfo],
    version: Option<&str>,
    variant: Option<&str>,
) -> Result<(&'a VersionInfo, &'a Download), String> {
    let info = match version {
        Some(v) => list.iter().find(|i| i.version == v).ok_or_else(|| format!("Versão {v} não encontrada."))?,
        None => list.first().ok_or("Nenhuma versão disponível.")?,
    };
    let download = match variant {
        Some(var) => info.downloads.iter().find(|d| d.variant == var),
        None => info.downloads.first(),
    }
    .ok_or_else(|| format!("A versão {} não tem download para Windows x64.", info.version))?;
    Ok((info, download))
}

/* ---------- Download ---------- */

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(concat!("jjphpmy/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())
}

/// Baixa `url` para `dest` emitindo progresso; devolve o SHA-256 calculado.
async fn download(rep: &Reporter, url: &str, dest: &Path) -> Result<String, String> {
    let mut resp = client()?
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Falha ao baixar {url}: {e}. Verifique a conexão com a internet."))?
        .error_for_status()
        .map_err(|e| format!("O servidor recusou o download de {url}: {e}"))?;
    let total = resp.content_length();
    let part = dest.with_extension("part");
    let mut file = File::create(&part).map_err(|e| format!("Não foi possível criar {}: {e}", part.display()))?;
    let mut hasher = Sha256::new();
    let mut received = 0u64;
    let mut last = Instant::now();
    rep.send("download", 0, total, None);
    loop {
        let chunk = match resp.chunk().await {
            Ok(Some(c)) => c,
            Ok(None) => break,
            Err(e) => {
                drop(file);
                let _ = fs::remove_file(&part);
                return Err(format!("O download de {url} foi interrompido: {e}"));
            }
        };
        file.write_all(&chunk).map_err(|e| format!("Erro ao gravar {}: {e} (disco cheio?)", part.display()))?;
        hasher.update(&chunk);
        received += chunk.len() as u64;
        if last.elapsed() >= Duration::from_millis(150) {
            rep.send("download", received, total, None);
            last = Instant::now();
        }
    }
    rep.send("download", received, total, None);
    drop(file);
    fs::rename(&part, dest).map_err(|e| format!("Não foi possível mover {}: {e}", part.display()))?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Hash esperado: o da listagem ou o lido em `sha256_url`. `None` = a fonte não publica.
async fn expected_sha256(rep: &Reporter, d: &Download) -> Option<String> {
    if let Some(sha) = &d.sha256 {
        return Some(sha.to_lowercase());
    }
    let url = d.sha256_url.as_ref()?;
    match versions::get_text(url).await {
        Ok(text) => parse_checksum(&text, &file_name_of(&d.url)),
        Err(e) => {
            rep.stage("verify", format!("Não foi possível ler o checksum ({e}); seguindo sem verificação."));
            None
        }
    }
}

/* ---------- Extração ---------- */

/// Extrai o zip para `dest`, removendo a pasta raiz única, se houver.
/// Grava em `{dest}.partial` e só renomeia no fim, para não deixar meia instalação.
fn extract(zip_path: &Path, dest: &Path, on_progress: impl Fn(u64, u64)) -> Result<(), String> {
    let file = File::open(zip_path).map_err(|e| format!("Não foi possível abrir {}: {e}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("Arquivo zip inválido ({}): {e}", zip_path.display()))?;

    let names: Vec<Option<PathBuf>> = (0..archive.len())
        .map(|i| archive.by_index(i).ok().and_then(|f| f.enclosed_name()))
        .collect();
    let root = zip_root(names.iter().flatten().map(PathBuf::as_path));

    let partial = dest.with_extension("partial");
    let _ = fs::remove_dir_all(&partial);
    fs::create_dir_all(&partial).map_err(|e| format!("Não foi possível criar {}: {e}", partial.display()))?;

    let total = archive.len() as u64;
    for i in 0..archive.len() {
        // Caminhos com ".." ou absolutos são ignorados (enclosed_name = None).
        let Some(name) = names[i].as_ref() else { continue };
        let rel = match &root {
            Some(r) => name.strip_prefix(r).unwrap_or(name),
            None => name.as_path(),
        };
        if rel.components().all(|c| matches!(c, Component::CurDir)) || rel.as_os_str().is_empty() {
            continue;
        }
        let out = partial.join(rel);
        let mut entry = archive.by_index(i).map_err(|e| format!("Erro ao ler o zip: {e}"))?;
        if entry.is_dir() {
            fs::create_dir_all(&out).map_err(|e| format!("Não foi possível criar {}: {e}", out.display()))?;
        } else {
            if let Some(parent) = out.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("Não foi possível criar {}: {e}", parent.display()))?;
            }
            let mut f = File::create(&out).map_err(|e| format!("Não foi possível criar {}: {e}", out.display()))?;
            io::copy(&mut entry, &mut f).map_err(|e| format!("Erro ao extrair {}: {e}", rel.display()))?;
        }
        if i % 50 == 0 {
            on_progress(i as u64, total);
        }
    }
    on_progress(total, total);

    if dest.exists() {
        fs::remove_dir_all(dest).map_err(|e| format!("Não foi possível substituir {}: {e}", dest.display()))?;
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Não foi possível criar {}: {e}", parent.display()))?;
    }
    fs::rename(&partial, dest).map_err(|e| format!("Não foi possível mover para {}: {e}", dest.display()))
}

/// Ajustes depois de extrair.
fn post_install(id: &str, dir: &Path) -> Result<(), String> {
    if id == "python" {
        // O Python "embeddable" vem com `#import site` comentado; sem isso o pip não funciona.
        for entry in fs::read_dir(dir).map_err(|e| e.to_string())?.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "_pth") {
                let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
                fs::write(&path, text.replace("#import site", "import site")).map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}

/* ---------- Visual C++ Redistributable ---------- */

#[cfg(windows)]
pub(crate) fn hidden(cmd: &mut std::process::Command) -> &mut std::process::Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW)
}

#[cfg(not(windows))]
pub(crate) fn hidden(cmd: &mut std::process::Command) -> &mut std::process::Command {
    cmd
}

/// O runtime 2015-2026 x64 já está instalado?
pub fn vcredist_installed() -> bool {
    hidden(std::process::Command::new("reg").args([
        "query",
        r"HKLM\SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64",
        "/v",
        "Installed",
    ]))
    .output()
    .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains("0x1"))
    .unwrap_or(false)
}

async fn install_vcredist(rep: &Reporter, d: &Download, tmp: &Path) -> Result<(), String> {
    if vcredist_installed() {
        rep.stage("skipped", "Já instalado no Windows.");
        return Ok(());
    }
    let dest = tmp.join("vc_redist.x64.exe");
    download(rep, &d.url, &dest).await?;
    rep.stage("extract", "Instalando (confirme a permissão de administrador do Windows)...");
    // O instalador da Microsoft pede elevação; só ele roda como administrador.
    let script = format!(
        "$p = Start-Process -FilePath '{}' -ArgumentList '/install','/quiet','/norestart' -Verb RunAs -Wait -PassThru; exit $p.ExitCode",
        dest.display()
    );
    let status = hidden(std::process::Command::new("powershell").args(["-NoProfile", "-Command", &script]))
        .status()
        .map_err(|e| format!("Não foi possível executar o instalador do Visual C++: {e}"))?;
    let _ = fs::remove_file(&dest);
    // 3010 = precisa reiniciar; 1638 = já existe uma versão mais nova.
    match status.code() {
        Some(0 | 3010 | 1638) => Ok(()),
        Some(code) => Err(format!(
            "O instalador do Visual C++ terminou com o código {code}. Se você recusou a permissão de administrador, tente de novo."
        )),
        None => Err("O instalador do Visual C++ foi interrompido.".into()),
    }
}

/* ---------- Instalação de um pacote ---------- */

/// Instala `pkg` (versão pedida ou a mais recente). Devolve o registro para
/// `config.installed`, ou `None` para o `vcredist` (que é do Windows).
pub async fn install(
    app: &AppHandle,
    pkg: &Package,
    version: Option<String>,
    variant: Option<&str>,
) -> Result<Option<InstalledPackage>, String> {
    let app = app.clone();
    let emit: Emit = Arc::new(move |p| {
        let _ = app.emit("install-progress", p);
    });
    install_in(emit, pkg, version, variant, &base_path(&["bin"]), &base_path(&["tmp"])).await
}

pub(crate) async fn install_in(
    emit: Emit,
    pkg: &Package,
    version: Option<String>,
    variant: Option<&str>,
    bin: &Path,
    tmp: &Path,
) -> Result<Option<InstalledPackage>, String> {
    let list = versions::versions_for(pkg, false).await;
    let (info, d) = pick(&list.versions, version.as_deref(), variant).map_err(|e| format!("{}: {e}", pkg.name))?;
    let rep = Reporter { emit, id: pkg.id.clone(), version: info.version.clone() };
    fs::create_dir_all(tmp).map_err(|e| format!("Não foi possível criar {}: {e}", tmp.display()))?;

    if pkg.id == "vcredist" {
        install_vcredist(&rep, d, tmp).await?;
        rep.stage("done", "Pronto.");
        return Ok(None);
    }

    let dest = install_dir_in(bin, &pkg.id, &info.version, variant);
    let zip_path = tmp.join(file_name_of(&d.url));
    let actual = download(&rep, &d.url, &zip_path).await?;

    rep.stage("verify", "Verificando SHA-256...");
    match expected_sha256(&rep, d).await {
        Some(expected) if expected != actual => {
            let _ = fs::remove_file(&zip_path);
            return Err(format!(
                "{} {}: o arquivo baixado está corrompido ou foi alterado (SHA-256 não confere). Tente instalar de novo.",
                pkg.name, info.version
            ));
        }
        Some(_) => {}
        None => rep.stage("verify", "A fonte não publica SHA-256 para este arquivo."),
    }

    rep.stage("extract", "Extraindo...");
    let (zp, dp, r) = (zip_path.clone(), dest.clone(), rep.clone());
    tauri::async_runtime::spawn_blocking(move || {
        extract(&zp, &dp, |done, total| r.send("extract", done, Some(total), None))?;
        post_install(&r.id, &dp)
    })
    .await
    .map_err(|e| format!("Falha na extração: {e}"))??;
    let _ = fs::remove_file(&zip_path);

    rep.stage("done", format!("Instalado em {}", dest.display()));
    Ok(Some(InstalledPackage {
        id: pkg.id.clone(),
        version: info.version.clone(),
        active: false,
        variant: variant.map(String::from),
        path: dest.display().to_string(),
    }))
}

/// Remove a pasta de uma versão instalada (só dentro de `bin\`).
pub fn remove(pkg: &InstalledPackage) -> Result<(), String> {
    let dir = install_dir(&pkg.id, &pkg.version, pkg.variant.as_deref());
    if !dir.starts_with(base_path(&["bin"])) {
        return Err("Caminho de instalação inválido.".into());
    }
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|e| {
            format!("Não foi possível remover {}: {e}. Pare o serviço e feche programas que usem a pasta.", dir.display())
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "77d715d0d9cee731e2251e7ddc2090a7a8e6d41fedef2e9827cd918593c5a652";

    #[test]
    fn reads_apache_lounge_checksums() {
        let text = "Checksums created with GPGHash\n\nSHA1-Checksum for: httpd.zip:\nB7372A7DE3E05FD0D11C4E72FF80A11AF0EF4FA5\n\n\
                    SHA224-Checksum for: httpd.zip:\nD11FEDED507E52CF8A347E13D94F97A32A4CCD79F2DD9AEC37051688\n\n\
                    SHA256-Checksum for: httpd.zip:\n77D715D0D9CEE731E2251E7DDC2090A7A8E6D41FEDEF2E9827CD918593C5A652\n\n\
                    SHA384-Checksum for: httpd.zip:\n0123";
        assert_eq!(parse_checksum(text, "httpd.zip").as_deref(), Some(HASH));
    }

    #[test]
    fn reads_shasums_file() {
        let other = "a".repeat(64);
        let text = format!("{other}  node-v22.11.0-win-x86.zip\n{HASH}  node-v22.11.0-win-x64.zip\n");
        assert_eq!(parse_checksum(&text, "node-v22.11.0-win-x64.zip").as_deref(), Some(HASH));
        assert_eq!(parse_checksum(&format!("{HASH}  phpMyAdmin.zip"), "outro.zip").as_deref(), Some(HASH));
        assert_eq!(parse_checksum(&text, "nao-existe.zip"), None);
    }

    #[test]
    fn detects_single_zip_root() {
        let p = |s: &str| PathBuf::from(s);
        let node = [p("node-v22/node.exe"), p("node-v22/npm"), p("node-v22/node_modules/x.js")];
        assert_eq!(zip_root(node.iter().map(PathBuf::as_path)), Some(p("node-v22")));
        let apache = [p("Apache24/bin/httpd.exe"), p("ReadMe.txt"), p("Apache24/conf/httpd.conf")];
        assert_eq!(zip_root(apache.iter().map(PathBuf::as_path)), Some(p("Apache24")));
        let php = [p("php.exe"), p("ext/php_curl.dll"), p("dev/php8.lib")];
        assert_eq!(zip_root(php.iter().map(PathBuf::as_path)), None);
        let mailpit = [p("mailpit.exe"), p("LICENSE")];
        assert_eq!(zip_root(mailpit.iter().map(PathBuf::as_path)), None);
    }

    #[test]
    fn php_dir_has_variant() {
        assert!(install_dir("php", "8.5.12-1", Some("nts")).ends_with(r"bin\php\8.5.12-1-nts"));
        assert!(install_dir("nginx", "1.31.8", None).ends_with(r"bin\nginx\1.31.8"));
    }

    #[test]
    fn extracts_and_strips_root() {
        let tmp = std::env::temp_dir().join(format!("jjphpmy-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let zip_path = tmp.join("a.zip");
        {
            let mut w = zip::ZipWriter::new(File::create(&zip_path).unwrap());
            let opts = zip::write::SimpleFileOptions::default();
            w.start_file("app-1.0/bin/app.exe", opts).unwrap();
            w.write_all(b"exe").unwrap();
            w.start_file("ReadMe.txt", opts).unwrap();
            w.write_all(b"leia").unwrap();
            w.start_file("../fora.txt", opts).unwrap();
            w.write_all(b"x").unwrap();
            w.finish().unwrap();
        }
        let dest = tmp.join("out");
        extract(&zip_path, &dest, |_, _| {}).unwrap();
        assert_eq!(fs::read_to_string(dest.join(r"bin\app.exe")).unwrap(), "exe");
        assert!(dest.join("ReadMe.txt").exists());
        assert!(!tmp.join("fora.txt").exists());
        let _ = fs::remove_dir_all(&tmp);
    }

    /// Instala de verdade alguns pacotes pequenos numa pasta temporária:
    /// `cargo test live_install -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_install() {
        let catalog = crate::catalog::Catalog::embedded();
        let root = std::env::temp_dir().join("jjphpmy-live");
        let _ = fs::remove_dir_all(&root);
        let (bin, tmp) = (root.join("bin"), root.join("tmp"));
        let emit: Emit = Arc::new(|p: Progress| {
            if p.stage != "download" && p.stage != "extract" || p.message.is_some() {
                println!("  [{} {}] {}: {}", p.id, p.version, p.stage, p.message.unwrap_or_default());
            }
        });
        for (id, variant, exe) in [
            ("nginx", None, "nginx.exe"),
            ("mailpit", None, "mailpit.exe"),
            ("apache", None, r"bin\httpd.exe"),
            ("phpmyadmin", None, "index.php"),
            ("php", Some("nts"), "php-cgi.exe"),
        ] {
            let pkg = catalog.get(id).unwrap();
            let res = tauri::async_runtime::block_on(install_in(emit.clone(), pkg, None, variant, &bin, &tmp));
            let installed = res.unwrap_or_else(|e| panic!("{id}: {e}")).unwrap();
            let file = Path::new(&installed.path).join(exe);
            println!("{id} {} -> {}", installed.version, file.display());
            assert!(file.exists(), "{} não existe", file.display());
        }
    }

    #[test]
    fn picks_variant_and_latest() {
        let list = vec![VersionInfo {
            version: "8.5.12-1".into(),
            label: None,
            downloads: vec![
                Download { variant: "ts".into(), url: "ts.zip".into(), sha256: None, sha256_url: None },
                Download { variant: "nts".into(), url: "nts.zip".into(), sha256: None, sha256_url: None },
            ],
        }];
        assert_eq!(pick(&list, None, Some("nts")).unwrap().1.url, "nts.zip");
        assert!(pick(&list, Some("7.0.0"), None).is_err());
        assert_eq!(file_name_of("https://x/y/php-8.5.zip?a=1"), "php-8.5.zip");
    }
}
