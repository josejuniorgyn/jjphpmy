//! Pastas dos programas instalados no PATH do usuário (`HKCU\Environment`),
//! sem administrador. Só entram as versões ativas; as entradas do JJPHPMY
//! são reconhecidas por estarem dentro de `C:\jjphpmy\bin`, e as outras
//! entradas do usuário nunca são alteradas.

use crate::config::{base_path, Config, InstalledPackage};
use std::path::{Path, PathBuf};

/// Subpastas de cada pacote que vão para o PATH. Apache, Nginx, Redis,
/// Memcached e Mailpit ficam de fora: quem os inicia é o próprio JJPHPMY.
fn subdirs(id: &str) -> &'static [&'static str] {
    match id {
        "php" | "nodejs" | "composer" | "wpcli" => &[""],
        "python" => &["", "Scripts"],
        "git" => &["cmd"],
        "mysql" | "postgresql" | "mongodb" => &["bin"],
        _ => &[],
    }
}

/// Executável usado para avisar quando outra pasta do PATH tem o mesmo programa.
fn main_exe(id: &str) -> Option<&'static str> {
    match id {
        "php" => Some("php.exe"),
        "nodejs" => Some("node.exe"),
        "python" => Some("python.exe"),
        "mysql" => Some("mysql.exe"),
        "postgresql" => Some("psql.exe"),
        "mongodb" => Some("mongod.exe"),
        _ => None,
    }
}

/// Versões ativas que entram no PATH. No PHP, se TS e NTS estiverem ativas, usa a TS.
fn active(installed: &[InstalledPackage]) -> Vec<&InstalledPackage> {
    let mut out: Vec<&InstalledPackage> = Vec::new();
    for p in installed.iter().filter(|p| p.active && !p.path.is_empty() && !subdirs(&p.id).is_empty()) {
        match out.iter().position(|o| o.id == p.id) {
            Some(i) if p.variant.as_deref() == Some("ts") => out[i] = p,
            Some(_) => {}
            None => out.push(p),
        }
    }
    out
}

/// Pastas desejadas no PATH, na ordem.
pub fn wanted_dirs(installed: &[InstalledPackage]) -> Vec<String> {
    active(installed)
        .into_iter()
        .flat_map(|p| subdirs(&p.id).iter().map(move |sub| join(&p.path, sub)))
        .collect()
}

fn join(base: &str, sub: &str) -> String {
    if sub.is_empty() { base.to_string() } else { format!(r"{base}\{sub}") }
}

/// A entrada foi criada pelo JJPHPMY?
fn is_ours(entry: &str, bin: &str) -> bool {
    let e = entry.trim().trim_end_matches('\\').to_lowercase();
    let b = bin.trim_end_matches('\\').to_lowercase();
    e == b || e.starts_with(&format!(r"{b}\"))
}

/// Novo valor do PATH: tira as entradas antigas do JJPHPMY e põe `wanted`
/// no início (para valerem mais que as do Laragon). Entradas vazias somem.
pub fn merge(current: &str, wanted: &[String], bin: &str) -> String {
    let mut out: Vec<String> = wanted.to_vec();
    for entry in current.split(';').map(str::trim).filter(|e| !e.is_empty()) {
        if !is_ours(entry, bin) && !out.iter().any(|o| o.eq_ignore_ascii_case(entry)) {
            out.push(entry.to_string());
        }
    }
    out.join(";")
}

/// Avisos para pastas do PATH do sistema que têm o mesmo programa. O Windows
/// põe o PATH do sistema antes do PATH do usuário, então elas ganham. (Pastas
/// do usuário, como as do Laragon, ficam depois das do JJPHPMY e não atrapalham.)
pub fn conflicts(installed: &[InstalledPackage], machine: &str) -> Vec<String> {
    let mut out = Vec::new();
    for p in active(installed) {
        let Some(exe) = main_exe(&p.id) else { continue };
        for entry in machine.split(';').map(str::trim).filter(|e| !e.is_empty()) {
            if Path::new(&expand(entry)).join(exe).exists() {
                out.push(format!(
                    "{exe} também está em {entry}, no PATH do sistema, que tem prioridade. Remova essa pasta em Variáveis de Ambiente do Windows para usar a versão do JJPHPMY."
                ));
            }
        }
    }
    out
}

/// Expande %VARIAVEIS% de uma entrada do PATH.
fn expand(entry: &str) -> String {
    let mut out = entry.to_string();
    while let Some(start) = out.find('%') {
        let Some(len) = out[start + 1..].find('%') else { break };
        let name = &out[start + 1..start + 1 + len];
        let Ok(value) = std::env::var(name) else { break };
        out.replace_range(start..start + len + 2, &value);
    }
    out
}

fn bin_dir() -> String {
    base_path(&["bin"]).display().to_string()
}

/* ---------- Registro do Windows ---------- */

#[cfg(windows)]
mod reg {
    use winreg::enums::{RegType, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE};
    use winreg::{RegKey, RegValue};

    const USER_KEY: &str = "Environment";
    const MACHINE_KEY: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";

    /// Lê sem expandir %VARIAVEIS%, para regravar igual.
    pub fn user_path() -> Result<String, String> {
        let key = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(USER_KEY, KEY_READ)
            .map_err(|e| format!("Não foi possível ler as variáveis de ambiente do usuário: {e}"))?;
        Ok(key.get_value::<String, _>("Path").unwrap_or_default())
    }

    pub fn machine_path() -> String {
        RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey_with_flags(MACHINE_KEY, KEY_READ)
            .and_then(|k| k.get_value::<String, _>("Path"))
            .unwrap_or_default()
    }

    /// Grava como REG_EXPAND_SZ (o tipo padrão do PATH, que mantém as %VARIAVEIS%).
    pub fn set_user_path(value: &str) -> Result<(), String> {
        let key = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(USER_KEY, KEY_READ | KEY_WRITE)
            .map_err(|e| format!("Não foi possível abrir as variáveis de ambiente do usuário: {e}"))?;
        let bytes: Vec<u8> = value.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect();
        key.set_raw_value("Path", &RegValue { bytes: bytes.into(), vtype: RegType::REG_EXPAND_SZ })
            .map_err(|e| format!("Não foi possível gravar o PATH do usuário: {e}"))?;
        broadcast();
        Ok(())
    }

    pub fn user_var(name: &str) -> Option<String> {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(USER_KEY, KEY_READ)
            .and_then(|k| k.get_value::<String, _>(name))
            .ok()
    }

    /// Grava (`Some`) ou apaga (`None`) uma variável de ambiente do usuário.
    pub fn set_user_var(name: &str, value: Option<&str>) -> Result<(), String> {
        let key = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(USER_KEY, KEY_READ | KEY_WRITE)
            .map_err(|e| format!("Não foi possível abrir as variáveis de ambiente do usuário: {e}"))?;
        match value {
            Some(v) => key.set_value(name, &v),
            None => key.delete_value(name),
        }
        .map_err(|e| format!("Não foi possível gravar a variável {name}: {e}"))?;
        broadcast();
        Ok(())
    }

    /// Avisa o Explorer e os outros programas que o ambiente mudou.
    fn broadcast() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
        };
        let param: Vec<u16> = "Environment".encode_utf16().chain([0]).collect();
        let mut result = 0usize;
        unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                param.as_ptr() as isize,
                SMTO_ABORTIFHUNG,
                3000,
                &mut result,
            );
        }
    }
}

#[cfg(not(windows))]
mod reg {
    pub fn user_path() -> Result<String, String> {
        Err("Disponível só no Windows.".into())
    }
    pub fn machine_path() -> String {
        String::new()
    }
    pub fn set_user_path(_: &str) -> Result<(), String> {
        Err("Disponível só no Windows.".into())
    }
    pub fn user_var(_: &str) -> Option<String> {
        None
    }
    pub fn set_user_var(_: &str, _: Option<&str>) -> Result<(), String> {
        Err("Disponível só no Windows.".into())
    }
}

/// Cópia do PATH original, feita antes da primeira alteração.
fn backup_file() -> PathBuf {
    base_path(&["config", "path-backup.txt"])
}

/// Deixa o PATH do usuário de acordo com a configuração. Só grava se mudar.
/// Devolve avisos (conflitos com outras instalações) para mostrar na interface.
pub fn sync(cfg: &Config) -> Result<Vec<String>, String> {
    let bin = bin_dir();
    let current = reg::user_path()?;
    let wanted: Vec<String> = if cfg.settings.add_to_path {
        wanted_dirs(&cfg.installed).into_iter().filter(|d| Path::new(d).is_dir()).collect()
    } else {
        Vec::new()
    };
    let new = merge(&current, &wanted, &bin);
    if new != current {
        let backup = backup_file();
        if !backup.exists() {
            let _ = std::fs::write(&backup, &current);
        }
        reg::set_user_path(&new)?;
    }
    let mut warnings = if cfg.settings.add_to_path { conflicts(&cfg.installed, &reg::machine_path()) } else { Vec::new() };
    warnings.extend(sync_phprc(cfg)?);
    Ok(warnings)
}

/// PHPRC aponta o `php` da linha de comando para o `etc\php\php.ini` gerado.
/// Um PHPRC criado pelo usuário (fora de `C:\jjphpmy`) não é alterado.
fn sync_phprc(cfg: &Config) -> Result<Vec<String>, String> {
    let dir = base_path(&["etc", "php"]);
    let has_php = cfg.installed.iter().any(|p| p.id == "php" && p.active);
    let wanted = (cfg.settings.add_to_path && has_php && dir.join("php.ini").exists()).then(|| dir.display().to_string());
    let current = reg::user_var("PHPRC");
    let ours = |v: &str| is_ours(v, &base_path(&[]).display().to_string());
    match (&wanted, &current) {
        (Some(w), Some(c)) if w.eq_ignore_ascii_case(c) => Ok(vec![]),
        (Some(_), Some(c)) if !ours(c) => Ok(vec![format!(
            "A variável PHPRC já aponta para {c}; o php da linha de comando não usará o php.ini do JJPHPMY."
        )]),
        (Some(w), _) => reg::set_user_var("PHPRC", Some(w)).map(|_| vec![]),
        (None, Some(c)) if ours(c) => reg::set_user_var("PHPRC", None).map(|_| vec![]),
        (None, _) => Ok(vec![]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BIN: &str = r"C:\jjphpmy\bin";

    fn pkg(id: &str, version: &str, variant: Option<&str>, active: bool) -> InstalledPackage {
        let dir = match variant {
            Some(v) => format!(r"{BIN}\{id}\{version}-{v}"),
            None => format!(r"{BIN}\{id}\{version}"),
        };
        InstalledPackage { id: id.into(), version: version.into(), active, variant: variant.map(Into::into), path: dir }
    }

    #[test]
    fn only_active_versions_and_right_subdirs() {
        let installed = vec![
            pkg("php", "8.4.27-1", Some("ts"), false),
            pkg("php", "8.5.12-1", Some("nts"), true),
            pkg("php", "8.5.12-1", Some("ts"), true),
            pkg("python", "3.14.10", None, true),
            pkg("mysql", "8.4.13", None, true),
            pkg("apache", "2.4.69", None, true),
        ];
        assert_eq!(
            wanted_dirs(&installed),
            vec![
                format!(r"{BIN}\php\8.5.12-1-ts"),
                format!(r"{BIN}\python\3.14.10"),
                format!(r"{BIN}\python\3.14.10\Scripts"),
                format!(r"{BIN}\mysql\8.4.13\bin"),
            ]
        );
    }

    #[test]
    fn merge_replaces_only_our_entries() {
        let current = format!(
            r"C:\Users\x\.cargo\bin;{BIN}\php\8.3.0-ts;%USERPROFILE%\AppData\Local\Microsoft\WindowsApps;;C:\laragon\bin\nodejs\node-v22"
        );
        let wanted = vec![format!(r"{BIN}\php\8.5.12-1-ts")];
        assert_eq!(
            merge(&current, &wanted, BIN),
            format!(
                r"{BIN}\php\8.5.12-1-ts;C:\Users\x\.cargo\bin;%USERPROFILE%\AppData\Local\Microsoft\WindowsApps;C:\laragon\bin\nodejs\node-v22"
            )
        );
        // Desligar a opção remove só as entradas do JJPHPMY.
        assert_eq!(merge(&current, &[], BIN).matches("jjphpmy").count(), 0);
        assert!(merge(&current, &[], BIN).starts_with(r"C:\Users\x\.cargo\bin;"));
        // Pasta parecida, mas de fora, fica.
        assert!(merge(r"C:\jjphpmy\binarios", &[], BIN).contains("binarios"));
    }

    #[test]
    fn merge_is_stable() {
        let wanted = vec![format!(r"{BIN}\nodejs\26.13.0")];
        let once = merge(r"C:\a;C:\b", &wanted, BIN);
        assert_eq!(merge(&once, &wanted, BIN), once);
    }

    /// Usa o registro real: adiciona uma pasta de teste, confere e devolve o
    /// PATH exatamente como estava. `cargo test live_path -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_path_roundtrip() {
        let original = reg::user_path().unwrap();
        let dir = base_path(&["bin", "php", "0.0.0-teste"]);
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = Config::default();
        cfg.installed.push(InstalledPackage {
            id: "php".into(),
            version: "0.0.0".into(),
            active: true,
            variant: Some("teste".into()),
            path: dir.display().to_string(),
        });

        let warnings = sync(&cfg).unwrap();
        let added = reg::user_path().unwrap();
        println!("avisos: {warnings:?}");
        println!("com o JJPHPMY: {}", added.split(';').take(3).collect::<Vec<_>>().join(" ; "));
        assert!(added.starts_with(&dir.display().to_string()));
        assert!(added.contains(r"%USERPROFILE%") == original.contains(r"%USERPROFILE%"));

        cfg.settings.add_to_path = false;
        sync(&cfg).unwrap();
        let _ = std::fs::remove_dir_all(base_path(&["bin", "php"]));
        assert_eq!(reg::user_path().unwrap(), original);
        println!("PATH restaurado igual ao original.");
    }

    #[test]
    fn expands_variables() {
        std::env::set_var("JJ_TESTE", r"C:\x");
        assert_eq!(expand(r"%JJ_TESTE%\bin"), r"C:\x\bin");
        assert_eq!(expand(r"C:\sem\variavel"), r"C:\sem\variavel");
    }
}
