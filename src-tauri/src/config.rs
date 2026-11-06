//! Configuração persistida em `C:\jjphpmy\config\jjphpmy.json`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

/// Diretório raiz de tudo que o jjphpmy instala/gera.
pub const BASE_DIR: &str = r"C:\jjphpmy";

pub fn base_path(parts: &[&str]) -> PathBuf {
    let mut p = PathBuf::from(BASE_DIR);
    for part in parts {
        p.push(part);
    }
    p
}

fn config_file() -> PathBuf {
    base_path(&["config", "jjphpmy.json"])
}

/// Tela 1 - Configurações.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub start_with_windows: bool,
    pub run_minimized: bool,
    pub auto_start_all: bool,
    /// Código do idioma: "pt-BR" (BrazilianPortuguese) ou "en".
    pub language: String,
    pub document_root: String,
    pub data_dir: String,
    pub auto_virtual_hosts: bool,
    /// Padrão do hostname; `{name}` é o nome da pasta dentro do document root.
    pub hostname_pattern: String,
    pub auto_backup: bool,
    pub backup_interval_hours: u32,
    /// Quantos backups manter em `backup\` (os mais antigos são apagados).
    pub backup_keep: u32,
    /// Xdebug ligado no php.ini (a DLL é baixada ao ligar).
    pub xdebug: bool,
    /// Extensões do PHP escolhidas na tela; `None` = lista padrão do JJPHPMY.
    pub php_extensions: Option<Vec<String>>,
    pub auto_update: bool,
    /// Editor usado pelo menu "Editar configuração" da bandeja.
    pub editor: String,
    /// Coloca as versões ativas de PHP, Node, Python e bancos no PATH do usuário.
    pub add_to_path: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            start_with_windows: false,
            run_minimized: false,
            auto_start_all: false,
            language: "pt-BR".into(),
            document_root: format!(r"{BASE_DIR}\www"),
            data_dir: format!(r"{BASE_DIR}\data"),
            auto_virtual_hosts: true,
            hostname_pattern: "{name}.test".into(),
            auto_backup: false,
            backup_interval_hours: 24,
            backup_keep: 10,
            xdebug: false,
            php_extensions: None,
            auto_update: false,
            editor: "notepad.exe".into(),
            add_to_path: true,
        }
    }
}

/// Tela 2 - Serviços e Portas (uma linha por serviço).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceEntry {
    /// Mesmo id do pacote no catálogo (apache, mysql, ...).
    pub id: String,
    pub enabled: bool,
    pub port: u16,
    /// `Some` apenas para serviços que suportam SSL (Apache, Nginx).
    #[serde(default)]
    pub ssl_port: Option<u16>,
    #[serde(default)]
    pub ssl_enabled: bool,
}

impl ServiceEntry {
    fn new(id: &str, port: u16, ssl_port: Option<u16>) -> Self {
        Self { id: id.into(), enabled: false, port, ssl_port, ssl_enabled: false }
    }
}

pub fn default_services() -> Vec<ServiceEntry> {
    vec![
        ServiceEntry::new("apache", 80, Some(443)),
        ServiceEntry::new("mysql", 3306, None),
        ServiceEntry::new("mariadb", 3306, None),
        ServiceEntry::new("postgresql", 5432, None),
        ServiceEntry::new("nginx", 8080, Some(8443)),
        ServiceEntry::new("memcached", 11211, None),
        ServiceEntry::new("redis", 6379, None),
        ServiceEntry::new("mongodb", 27017, None),
        ServiceEntry::new("opensearch", 9200, None),
        ServiceEntry::new("elasticsearch", 9201, None),
        ServiceEntry::new("mailpit", 1025, None),
    ]
}

/// Pacote instalado em `C:\jjphpmy\bin\{id}\{version}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledPackage {
    pub id: String,
    pub version: String,
    /// Versão ativa quando há várias instaladas do mesmo pacote (e variante).
    pub active: bool,
    /// Só no PHP: "ts" (Apache) ou "nts" (Nginx/php-cgi).
    #[serde(default)]
    pub variant: Option<String>,
    /// Pasta onde foi extraído (`bin\{id}\{versão}`).
    #[serde(default)]
    pub path: String,
}

impl InstalledPackage {
    pub fn same(&self, id: &str, version: &str, variant: Option<&str>) -> bool {
        self.id == id && self.version == version && self.variant.as_deref() == variant
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub settings: Settings,
    pub services: Vec<ServiceEntry>,
    pub installed: Vec<InstalledPackage>,
    /// Chave do cookie do phpMyAdmin (`blowfish_secret`), criada uma vez.
    pub pma_secret: String,
    /// PHP de cada site (host -> "versão-variante" de um PHP instalado), como o
    /// MultiPHP do cPanel. Sites fora daqui usam o PHP padrão.
    pub site_php: std::collections::BTreeMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        Self { settings: Settings::default(), services: default_services(), installed: Vec::new(), pma_secret: String::new(), site_php: Default::default() }
    }
}

impl Config {
    /// Lê o arquivo; se não existir ou estiver inválido, usa o padrão.
    pub fn load() -> Self {
        let mut cfg: Config = fs::read_to_string(config_file())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        // Garante que serviços novos (adicionados em versões futuras) apareçam.
        for def in default_services() {
            if !cfg.services.iter().any(|s| s.id == def.id) {
                cfg.services.push(def);
            }
        }
        cfg
    }

    pub fn save(&self) -> Result<(), String> {
        let path = config_file();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("Não foi possível criar {}: {e}", dir.display()))?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(&path, json).map_err(|e| format!("Não foi possível salvar {}: {e}", path.display()))
    }

    pub fn service(&self, id: &str) -> Option<&ServiceEntry> {
        self.services.iter().find(|s| s.id == id)
    }
}

/// Retorna mensagens para cada porta usada por mais de um serviço ativado.
pub fn port_conflicts(services: &[ServiceEntry]) -> Vec<String> {
    let mut used: HashMap<u16, Vec<String>> = HashMap::new();
    for s in services.iter().filter(|s| s.enabled) {
        used.entry(s.port).or_default().push(s.id.clone());
        if let (true, Some(ssl)) = (s.ssl_enabled, s.ssl_port) {
            used.entry(ssl).or_default().push(format!("{} (SSL)", s.id));
        }
    }
    let mut out: Vec<String> = used
        .into_iter()
        .filter(|(_, ids)| ids.len() > 1)
        .map(|(port, ids)| format!("Porta {port} usada por: {}", ids.join(", ")))
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_ports_do_not_conflict() {
        let mut services = default_services();
        for s in &mut services {
            // MariaDB é alternativa ao MySQL (mesma porta 3306): não liga junto.
            s.enabled = s.id != "mariadb";
            s.ssl_enabled = true;
        }
        assert!(port_conflicts(&services).is_empty());
        // Ligar os dois é avisado como conflito.
        services.iter_mut().find(|s| s.id == "mariadb").unwrap().enabled = true;
        assert_eq!(port_conflicts(&services), ["Porta 3306 usada por: mysql, mariadb"]);
    }

    #[test]
    fn detects_conflict() {
        let mut services = default_services();
        services[0].enabled = true; // apache 80
        services[3].enabled = true; // nginx
        services[3].port = 80;
        assert_eq!(port_conflicts(&services).len(), 1);
    }
}
