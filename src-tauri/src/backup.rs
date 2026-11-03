//! Etapa 8: backup dos bancos. Gera `backup\AAAA-MM-DD_HHMM.zip` com:
//! `mysql.sql` (mysqldump --all-databases), `postgresql.sql` (pg_dumpall),
//! `mongodb.archive` (mongodump, se as Database Tools estiverem na pasta do
//! MongoDB) e uma cópia de `etc\`. Só entram os bancos que estão rodando.

use crate::confgen::Layout;
use crate::config::Config;
use crate::installer::hidden;
use serde::Serialize;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};
use zip::write::SimpleFileOptions;

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    /// Arquivo criado (vazio se nenhum banco estava rodando).
    pub file: String,
    pub included: Vec<String>,
    /// Bancos instalados que ficaram de fora, com o motivo.
    pub skipped: Vec<String>,
    /// Backups antigos apagados pela retenção.
    pub removed: Vec<String>,
}

pub fn dir(layout: &Layout) -> PathBuf {
    layout.dir(&["backup"])
}

/// "2026-11-03_1405" na hora local do Windows.
pub fn stamp() -> String {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::GetLocalTime;
        let mut t = unsafe { std::mem::zeroed() };
        unsafe { GetLocalTime(&mut t) };
        format!("{:04}-{:02}-{:02}_{:02}{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute)
    }
    #[cfg(not(windows))]
    {
        "0000-00-00_0000".into()
    }
}

/// Backups no padrão `AAAA-MM-DD_HHMM*.zip`, do mais novo para o mais antigo.
pub fn list(layout: &Layout) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(dir(layout))
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    out.retain(|p| {
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        name.ends_with(".zip") && name.len() >= 19 && name.as_bytes()[4] == b'-' && name.as_bytes()[10] == b'_'
    });
    out.sort();
    out.reverse();
    out
}

/// Apaga os backups além dos `keep` mais novos. Devolve os nomes apagados.
pub fn prune(layout: &Layout, keep: usize) -> Vec<String> {
    list(layout)
        .into_iter()
        .skip(keep.max(1))
        .filter(|p| fs::remove_file(p).is_ok())
        .map(|p| p.file_name().unwrap_or_default().to_string_lossy().to_string())
        .collect()
}

/// Hora do backup mais recente (data de modificação do arquivo).
pub fn last_time(layout: &Layout) -> Option<SystemTime> {
    list(layout).first().and_then(|p| fs::metadata(p).ok()).and_then(|m| m.modified().ok())
}

/// Já passou o intervalo desde o último backup?
pub fn due(layout: &Layout, interval_hours: u32) -> bool {
    match last_time(layout) {
        Some(t) => t.elapsed().unwrap_or_default() >= Duration::from_secs(u64::from(interval_hours.max(1)) * 3600),
        None => true,
    }
}

fn active_bin(cfg: &Config, id: &str, exe: &str) -> Option<PathBuf> {
    cfg.installed
        .iter()
        .find(|p| p.id == id && p.active)
        .map(|p| Path::new(&p.path).join("bin").join(exe))
        .filter(|p| p.exists())
}

fn port(cfg: &Config, id: &str) -> String {
    cfg.service(id).map(|s| s.port).unwrap_or(0).to_string()
}

fn dump(cmd: &mut Command, what: &str, out: &Path) -> Result<(), String> {
    let res = hidden(cmd).output().map_err(|e| format!("{what}: não foi possível executar ({e})"))?;
    let size = fs::metadata(out).map(|m| m.len()).unwrap_or(0);
    if res.status.success() && size > 0 {
        Ok(())
    } else {
        let msg = String::from_utf8_lossy(&res.stderr);
        Err(format!("{what} falhou: {}", msg.lines().last().unwrap_or("sem detalhes").trim()))
    }
}

/// Faz o backup. `running` = ids dos serviços rodando agora.
pub fn run(cfg: &Config, layout: &Layout, running: &[String], keep: usize) -> Result<Report, String> {
    let mut report = Report::default();
    let work = layout.dir(&["tmp", "backup"]);
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).map_err(|e| format!("Não foi possível criar {}: {e}", work.display()))?;
    let is_running = |id: &str| running.iter().any(|r| r == id);
    let installed = |id: &str| cfg.installed.iter().any(|p| p.id == id && p.active);
    let mut files: Vec<(String, PathBuf)> = Vec::new();

    if installed("mysql") {
        let out = work.join("mysql.sql");
        match (is_running("mysql"), active_bin(cfg, "mysql", "mysqldump.exe")) {
            (false, _) => report.skipped.push("MySQL (parado)".into()),
            (_, None) => report.skipped.push("MySQL (mysqldump.exe não encontrado)".into()),
            (true, Some(exe)) => {
                let mut cmd = Command::new(exe);
                cmd.args(["-u", "root", "-h", "127.0.0.1", "-P", &port(cfg, "mysql")])
                    .args(["--all-databases", "--single-transaction", "--routines", "--events", "--triggers"])
                    .arg(format!("--result-file={}", out.display()));
                match dump(&mut cmd, "mysqldump", &out) {
                    Ok(()) => {
                        files.push(("mysql.sql".into(), out));
                        report.included.push("MySQL".into());
                    }
                    Err(e) => report.skipped.push(e),
                }
            }
        }
    }

    if installed("postgresql") {
        let out = work.join("postgresql.sql");
        match (is_running("postgresql"), active_bin(cfg, "postgresql", "pg_dumpall.exe")) {
            (false, _) => report.skipped.push("PostgreSQL (parado)".into()),
            (_, None) => report.skipped.push("PostgreSQL (pg_dumpall.exe não encontrado)".into()),
            (true, Some(exe)) => {
                let mut cmd = Command::new(exe);
                cmd.args(["-U", "postgres", "-h", "127.0.0.1", "-p", &port(cfg, "postgresql"), "-f"]).arg(&out);
                match dump(&mut cmd, "pg_dumpall", &out) {
                    Ok(()) => {
                        files.push(("postgresql.sql".into(), out));
                        report.included.push("PostgreSQL".into());
                    }
                    Err(e) => report.skipped.push(e),
                }
            }
        }
    }

    if installed("mongodb") {
        let out = work.join("mongodb.archive");
        match (is_running("mongodb"), active_bin(cfg, "mongodb", "mongodump.exe")) {
            (false, _) => report.skipped.push("MongoDB (parado)".into()),
            // O zip do servidor não traz o mongodump (vem nas "Database Tools").
            (_, None) => report.skipped.push("MongoDB (mongodump.exe não encontrado; instale as MongoDB Database Tools na pasta bin)".into()),
            (true, Some(exe)) => {
                let mut cmd = Command::new(exe);
                cmd.args(["--host", "127.0.0.1", "--port", &port(cfg, "mongodb"), "--gzip"])
                    .arg(format!("--archive={}", out.display()));
                match dump(&mut cmd, "mongodump", &out) {
                    Ok(()) => {
                        files.push(("mongodb.archive".into(), out));
                        report.included.push("MongoDB".into());
                    }
                    Err(e) => report.skipped.push(e),
                }
            }
        }
    }

    if files.is_empty() {
        let _ = fs::remove_dir_all(&work);
        return Ok(report);
    }

    fs::create_dir_all(dir(layout)).map_err(|e| format!("Não foi possível criar {}: {e}", dir(layout).display()))?;
    let mut target = dir(layout).join(format!("{}.zip", stamp()));
    let mut n = 2;
    while target.exists() {
        target = dir(layout).join(format!("{}-{n}.zip", stamp()));
        n += 1;
    }
    write_zip(&target, &files, &layout.dir(&["etc"]))?;
    let _ = fs::remove_dir_all(&work);
    report.file = target.display().to_string();
    report.removed = prune(layout, keep);
    Ok(report)
}

/// Restaura `mysql.sql` e `postgresql.sql` de um backup (pelo nome do arquivo
/// em `backup\`) nos bancos que estão rodando. Devolve o que foi feito.
pub fn restore(cfg: &Config, layout: &Layout, name: &str, running: &[String]) -> Result<Vec<String>, String> {
    if name.contains(['/', '\\']) || !name.ends_with(".zip") {
        return Err(format!("Nome de backup inválido: {name}"));
    }
    let path = dir(layout).join(name);
    let mut zip = zip::ZipArchive::new(File::open(&path).map_err(|e| format!("Não foi possível abrir {}: {e}", path.display()))?)
        .map_err(|e| format!("Backup inválido: {e}"))?;
    let work = layout.dir(&["tmp", "restore"]);
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let mut done = Vec::new();

    for (entry, id, name_pt) in [("mysql.sql", "mysql", "MySQL"), ("postgresql.sql", "postgresql", "PostgreSQL")] {
        let Ok(mut f) = zip.by_name(entry) else { continue };
        if !running.iter().any(|r| r == id) {
            done.push(format!("{name_pt}: não restaurado, o serviço está parado."));
            continue;
        }
        let file = work.join(entry);
        io::copy(&mut f, &mut File::create(&file).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let mut cmd = match id {
            "mysql" => {
                let exe = active_bin(cfg, "mysql", "mysql.exe").ok_or("mysql.exe não encontrado.")?;
                let mut c = Command::new(exe);
                c.args(["-u", "root", "-h", "127.0.0.1", "-P", &port(cfg, "mysql"), "--max-allowed-packet=512M"])
                    .stdin(File::open(&file).map_err(|e| e.to_string())?);
                c
            }
            _ => {
                let exe = active_bin(cfg, "postgresql", "psql.exe").ok_or("psql.exe não encontrado.")?;
                let mut c = Command::new(exe);
                c.args(["-U", "postgres", "-h", "127.0.0.1", "-p", &port(cfg, "postgresql"), "-d", "postgres", "-q", "-f"])
                    .arg(&file);
                c
            }
        };
        let out = hidden(&mut cmd).output().map_err(|e| format!("{name_pt}: {e}"))?;
        let errors = String::from_utf8_lossy(&out.stderr).lines().filter(|l| l.contains("ERROR")).count();
        done.push(match (out.status.success(), errors) {
            (true, 0) => format!("{name_pt}: restaurado."),
            // pg_dumpall recria o usuário postgres, que já existe: esse erro é esperado.
            (true, n) => format!("{name_pt}: restaurado, com {n} aviso(s) de objetos que já existiam."),
            (false, _) => format!(
                "{name_pt}: falhou ({}).",
                String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("sem detalhes").trim()
            ),
        });
    }
    let _ = fs::remove_dir_all(&work);
    if done.is_empty() {
        return Err("Este backup não tem bancos MySQL nem PostgreSQL.".into());
    }
    Ok(done)
}

/// Grava o zip: os dumps na raiz e a pasta `etc\` em `etc/`.
fn write_zip(target: &Path, files: &[(String, PathBuf)], etc: &Path) -> Result<(), String> {
    let part = target.with_extension("partial");
    let result = (|| -> io::Result<()> {
        let mut zip = zip::ZipWriter::new(File::create(&part)?);
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .large_file(true);
        for (name, path) in files {
            zip.start_file(name.as_str(), opts)?;
            io::copy(&mut File::open(path)?, &mut zip)?;
        }
        let mut stack = vec![etc.to_path_buf()];
        while let Some(d) = stack.pop() {
            for entry in fs::read_dir(&d)?.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if let Ok(rel) = path.strip_prefix(etc) {
                    let name = format!("etc/{}", rel.display().to_string().replace('\\', "/"));
                    zip.start_file(name, opts)?;
                    io::copy(&mut File::open(&path)?, &mut zip)?;
                }
            }
        }
        zip.finish()?.flush()
    })();
    match result.and_then(|_| fs::rename(&part, target)) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&part);
            Err(format!("Não foi possível gravar o backup {}: {e} (disco cheio?)", target.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(name: &str) -> Layout {
        let base = std::env::temp_dir().join(format!("jjphpmy-bkp-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("backup")).unwrap();
        Layout { base }
    }

    /// MySQL e PostgreSQL reais com um banco de teste, backup e conferência
    /// do zip. `cargo test live_backup -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_backup() {
        use std::io::Read;
        use std::sync::Arc;
        let l = Layout { base: std::env::temp_dir().join("jjphpmy-live-bkp") };
        let _ = fs::remove_dir_all(&l.base);
        let catalog = crate::catalog::Catalog::embedded();
        let emit: crate::installer::Emit = Arc::new(|_| {});
        let mut cfg = Config::default();
        cfg.settings.document_root = l.dir(&["www"]).display().to_string();
        cfg.settings.data_dir = l.dir(&["data"]).display().to_string();
        for (id, port) in [("mysql", 13307), ("postgresql", 15433)] {
            cfg.services.iter_mut().find(|s| s.id == id).unwrap().port = port;
        }
        let (bin_dir, tmp_dir) = (l.dir(&["bin"]), l.dir(&["tmp"]));
        for id in ["mysql", "postgresql"] {
            let fut = crate::installer::install_in(emit.clone(), catalog.get(id).unwrap(), None, None, &bin_dir, &tmp_dir);
            let p = tauri::async_runtime::block_on(fut).unwrap_or_else(|e| panic!("{id}: {e}")).unwrap();
            cfg.installed.push(crate::config::InstalledPackage { active: true, ..p });
        }
        crate::confgen::generate(&cfg, &l, true).unwrap();
        crate::confgen::init_data(&cfg, &l).unwrap();
        let m = crate::services::Manager::default();
        m.start(&cfg, &l, "mysql").unwrap_or_else(|e| panic!("{e}"));
        m.start(&cfg, &l, "postgresql").unwrap_or_else(|e| panic!("{e}"));

        let bin = |id: &str, exe: &str| Path::new(&cfg.installed.iter().find(|p| p.id == id).unwrap().path).join("bin").join(exe);
        let ok = |c: &mut Command| assert!(c.status().unwrap().success());
        ok(Command::new(bin("mysql", "mysql.exe")).args(["-u", "root", "-h", "127.0.0.1", "-P", "13307", "-e",
            "CREATE DATABASE loja; CREATE TABLE loja.produto (nome VARCHAR(50)); INSERT INTO loja.produto VALUES ('cafe-mysql');"]));
        ok(Command::new(bin("postgresql", "psql.exe")).args(["-U", "postgres", "-h", "127.0.0.1", "-p", "15433", "-c", "CREATE DATABASE loja;"]));
        ok(Command::new(bin("postgresql", "psql.exe")).args(["-U", "postgres", "-h", "127.0.0.1", "-p", "15433", "-d", "loja", "-c",
            "CREATE TABLE produto (nome text); INSERT INTO produto VALUES ('cafe-pg');"]));

        // Backups antigos para a retenção (manter 2).
        fs::create_dir_all(dir(&l)).unwrap();
        for old in ["2020-01-01_0000.zip", "2020-01-02_0000.zip"] {
            fs::write(dir(&l).join(old), "x").unwrap();
        }
        let running = m.running_ids();
        let r = run(&cfg, &l, &running, 2).unwrap();
        println!("arquivo: {}\nincluído: {:?}\nde fora: {:?}\nremovidos: {:?}", r.file, r.included, r.skipped, r.removed);
        assert_eq!(r.included, ["MySQL", "PostgreSQL"]);
        assert_eq!(r.removed, ["2020-01-01_0000.zip"]);

        let mut zip = zip::ZipArchive::new(File::open(&r.file).unwrap()).unwrap();
        let mut read = |name: &str| {
            let mut s = String::new();
            zip.by_name(name).unwrap().read_to_string(&mut s).unwrap();
            s
        };
        assert!(read("mysql.sql").contains("'cafe-mysql'"));
        assert!(read("postgresql.sql").contains("cafe-pg"));
        assert!(read("etc/mysql/my.ini").contains("13307"));
        println!("zip: {} arquivos, {} KB", zip.len(), fs::metadata(&r.file).unwrap().len() / 1024);
        for id in m.running_ids().iter().rev() {
            m.stop(id);
        }
        let _ = fs::remove_dir_all(&l.base);
    }

    #[test]
    fn stamp_has_expected_shape() {
        let s = stamp();
        assert_eq!(s.len(), 15);
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[10..11], "_");
    }

    #[test]
    fn prunes_oldest_and_ignores_other_files() {
        let l = layout("prune");
        for name in ["2026-11-01_0100.zip", "2026-11-02_0100.zip", "2026-11-03_0100.zip", "2026-11-03_0100-2.zip", "meu.zip", "nota.txt"] {
            fs::write(dir(&l).join(name), "x").unwrap();
        }
        assert_eq!(list(&l).len(), 4);
        let removed = prune(&l, 2);
        assert_eq!(removed, ["2026-11-02_0100.zip", "2026-11-01_0100.zip"]);
        assert!(dir(&l).join("meu.zip").exists(), "arquivos do usuário ficam");
        assert!(dir(&l).join("2026-11-03_0100-2.zip").exists());
        let _ = fs::remove_dir_all(&l.base);
    }

    #[test]
    fn due_when_no_backup_or_old() {
        let l = layout("due");
        assert!(due(&l, 24));
        fs::write(dir(&l).join("2026-11-03_0100.zip"), "x").unwrap();
        assert!(!due(&l, 24));
        let _ = fs::remove_dir_all(&l.base);
    }

    #[test]
    fn nothing_running_creates_no_file() {
        let l = layout("empty");
        let mut cfg = Config::default();
        cfg.installed.push(crate::config::InstalledPackage {
            id: "mysql".into(),
            version: "8.4.13".into(),
            active: true,
            variant: None,
            path: l.dir(&["bin", "mysql"]).display().to_string(),
        });
        let r = run(&cfg, &l, &[], 10).unwrap();
        assert!(r.file.is_empty());
        assert_eq!(r.skipped, ["MySQL (parado)"]);
        let _ = fs::remove_dir_all(&l.base);
    }
}
