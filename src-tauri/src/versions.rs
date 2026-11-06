//! Versões disponíveis de cada pacote: leitura das fontes oficiais,
//! cache de 24 h e lista de reserva (fallback) quando não há internet.
//!
//! Cada fonte tem um `parse_*` puro (testado com `tests/fixtures/`) e o
//! `fetch_online` só faz o HTTP e chama o parser certo.

use crate::catalog::Package;
use crate::config::base_path;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const CACHE_TTL_SECS: u64 = 24 * 60 * 60;
/// Por linha maior.menor (ex.: 8.4.x), para todos os ramos aparecerem.
const MAX_PER_LINE: usize = 10;
const MAX_VERSIONS: usize = 150;
const PHP_BASE: &str = "https://windows.php.net/downloads/releases/";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Download {
    /// "x64" ou, no PHP, "ts" / "nts".
    pub variant: String,
    pub url: String,
    #[serde(default)]
    pub sha256: Option<String>,
    /// Arquivo de checksums a consultar na instalação, quando o hash não vem na listagem.
    #[serde(default)]
    pub sha256_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VersionInfo {
    pub version: String,
    /// Rótulo extra exibido na lista (ex.: "LTS Jod" no Node.js).
    #[serde(default)]
    pub label: Option<String>,
    pub downloads: Vec<Download>,
}

impl VersionInfo {
    fn single(version: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            version: version.into(),
            label: None,
            downloads: vec![Download { variant: "x64".into(), url: url.into(), sha256: None, sha256_url: None }],
        }
    }

    fn sha256(mut self, sha256: Option<String>) -> Self {
        self.downloads[0].sha256 = sha256.filter(|s| !s.is_empty());
        self
    }

    fn sha256_url(mut self, url: String) -> Self {
        self.downloads[0].sha256_url = Some(url);
        self
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionList {
    pub versions: Vec<VersionInfo>,
    /// "online" | "cache" | "fallback"
    pub origin: String,
    /// Motivo de não ter vindo da fonte oficial.
    pub error: Option<String>,
}

/* ---------- Ordenação ---------- */

/// "8.5.12-1" -> [8, 5, 12, 1], para comparar versões numericamente.
pub fn version_key(v: &str) -> Vec<u64> {
    v.split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().unwrap_or(0))
        .collect()
}

fn at_least(v: &str, min: &[u64]) -> bool {
    version_key(v).as_slice() >= min
}

/// Mais nova primeiro, sem repetidas, até `MAX_PER_LINE` por linha
/// maior.menor e `MAX_VERSIONS` no total.
fn sort_desc(list: &mut Vec<VersionInfo>) {
    list.sort_by(|a, b| version_key(&b.version).cmp(&version_key(&a.version)));
    list.dedup_by(|a, b| a.version == b.version);
    let mut per_line: HashMap<Vec<u64>, usize> = HashMap::new();
    list.retain(|v| {
        let line: Vec<u64> = version_key(&v.version).into_iter().take(2).collect();
        let count = per_line.entry(line).or_default();
        *count += 1;
        *count <= MAX_PER_LINE
    });
    list.truncate(MAX_VERSIONS);
}

fn json<T: for<'de> Deserialize<'de>>(text: &str) -> Result<T, String> {
    serde_json::from_str(text).map_err(|e| format!("Resposta inválida da fonte de versões: {e}"))
}

fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/* ---------- Parsers ---------- */

/// https://windows.php.net/downloads/releases/releases.json (última de cada ramo).
pub fn parse_php_releases(text: &str) -> Result<Vec<VersionInfo>, String> {
    let root: BTreeMap<String, Value> = json(text)?;
    let mut out = Vec::new();
    for branch in root.values() {
        let (Some(version), Some(builds)) = (branch["version"].as_str(), branch.as_object()) else {
            continue;
        };
        let mut downloads = Vec::new();
        for (key, build) in builds {
            let variant = match key.split('-').next() {
                Some("ts") => "ts",
                Some("nts") => "nts",
                _ => continue,
            };
            let Some(path) = build["zip"]["path"].as_str().filter(|_| key.ends_with("-x64")) else {
                continue;
            };
            downloads.push(Download {
                variant: variant.into(),
                url: format!("{PHP_BASE}{path}"),
                sha256: build["zip"]["sha256"].as_str().map(String::from),
                sha256_url: None,
            });
        }
        downloads.sort_by_key(|d| d.variant != "ts");
        if !downloads.is_empty() {
            out.push(VersionInfo { version: version.into(), label: None, downloads });
        }
    }
    Ok(out)
}

/// Listagem HTML de https://windows.php.net/downloads/releases/archives/ (a partir do 7.4).
pub fn parse_php_archives(html: &str) -> Vec<VersionInfo> {
    let re = Regex::new(r#"href="(php-(\d+\.\d+\.\d+(?:-\d+)?)-(nts-)?Win32-v[cs]\d+-x64\.zip)""#).unwrap();
    let mut by_version: BTreeMap<String, Vec<Download>> = BTreeMap::new();
    for c in re.captures_iter(html) {
        let version = c[2].to_string();
        if !at_least(&version, &[7, 4]) {
            continue;
        }
        let variant = if c.get(3).is_some() { "nts" } else { "ts" };
        let downloads = by_version.entry(version).or_default();
        if !downloads.iter().any(|d| d.variant == variant) {
            downloads.push(Download {
                variant: variant.into(),
                url: format!("{PHP_BASE}archives/{}", &c[1]),
                sha256: None,
                sha256_url: None,
            });
        }
    }
    by_version
        .into_iter()
        .map(|(version, mut downloads)| {
            downloads.sort_by_key(|d| d.variant != "ts");
            VersionInfo { version, label: None, downloads }
        })
        .collect()
}

/// https://www.apachelounge.com/download/ (só publica a versão atual).
pub fn parse_apache_lounge(html: &str) -> Vec<VersionInfo> {
    let re = Regex::new(r#"(?i)href="(/download/[^"]+/httpd-(\d+\.\d+\.\d+)-\d+-win64-vs\d+\.zip)""#).unwrap();
    re.captures_iter(html)
        .map(|c| {
            let url = format!("https://www.apachelounge.com{}", &c[1]);
            VersionInfo::single(&c[2], &url).sha256_url(format!("{url}.txt"))
        })
        .collect()
}

/// https://nginx.org/en/download.html
pub fn parse_nginx(html: &str) -> Vec<VersionInfo> {
    let re = Regex::new(r#"href="/download/nginx-(\d+\.\d+\.\d+)\.zip""#).unwrap();
    re.captures_iter(html)
        .map(|c| VersionInfo::single(&c[1], format!("https://nginx.org/download/nginx-{}.zip", &c[1])))
        .collect()
}

/// Tags do GitHub mysql/mysql-server. A CDN só mantém a última versão de
/// cada linha (8.0, 8.4, 9.x...), então só ela é oferecida.
pub fn parse_mysql_tags(text: &str) -> Result<Vec<VersionInfo>, String> {
    #[derive(Deserialize)]
    struct Ref {
        #[serde(rename = "ref")]
        name: String,
    }
    let re = Regex::new(r"^refs/tags/mysql-(\d+)\.(\d+)\.(\d+)$").unwrap();
    let mut latest: BTreeMap<(u64, u64), u64> = BTreeMap::new();
    for r in json::<Vec<Ref>>(text)? {
        let Some(c) = re.captures(&r.name) else { continue };
        let (major, minor, patch): (u64, u64, u64) = (c[1].parse().unwrap(), c[2].parse().unwrap(), c[3].parse().unwrap());
        if major < 8 {
            continue;
        }
        let best = latest.entry((major, minor)).or_insert(patch);
        *best = (*best).max(patch);
    }
    Ok(latest
        .into_iter()
        .map(|((major, minor), patch)| {
            let v = format!("{major}.{minor}.{patch}");
            let url = format!("https://cdn.mysql.com/Downloads/MySQL-{major}.{minor}/mysql-{v}-winx64.zip");
            VersionInfo::single(v, url)
        })
        .collect())
}

/// https://www.postgresql.org/versions.json + binários zip da EDB.
pub fn parse_postgresql(text: &str) -> Result<Vec<VersionInfo>, String> {
    let list: Vec<Value> = json(text)?;
    Ok(list
        .iter()
        .filter(|v| v["supported"].as_bool() == Some(true))
        .filter_map(|v| Some(format!("{}.{}", text_of(&v["major"])?, text_of(&v["latestMinor"])?)))
        .filter(|v| at_least(v, &[13]))
        .map(|v| {
            let url = format!("https://get.enterprisedb.com/postgresql/postgresql-{v}-1-windows-x64-binaries.zip");
            VersionInfo::single(v, url)
        })
        .collect())
}

/// https://downloads.mongodb.org/current.json (Community, Windows x64).
pub fn parse_mongodb(text: &str) -> Result<Vec<VersionInfo>, String> {
    let root: Value = json(text)?;
    let empty = Vec::new();
    let mut out = Vec::new();
    for v in root["versions"].as_array().unwrap_or(&empty) {
        let Some(version) = v["version"].as_str() else { continue };
        let windows = v["downloads"].as_array().unwrap_or(&empty).iter().find(|d| {
            d["target"] == "windows" && d["edition"] == "base" && d["arch"] == "x86_64"
        });
        if let Some(url) = windows.and_then(|d| d["archive"]["url"].as_str()) {
            let sha = windows.and_then(|d| d["archive"]["sha256"].as_str()).map(String::from);
            out.push(VersionInfo::single(version, url).sha256(sha));
        }
    }
    Ok(out)
}

/// https://www.phpmyadmin.net/home_page/version.json
pub fn parse_phpmyadmin(text: &str) -> Result<Vec<VersionInfo>, String> {
    let root: Value = json(text)?;
    let empty = Vec::new();
    Ok(root["releases"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter_map(|r| r["version"].as_str())
        .map(|v| {
            let url = format!("https://files.phpmyadmin.net/phpMyAdmin/{v}/phpMyAdmin-{v}-all-languages.zip");
            VersionInfo::single(v, &url).sha256_url(format!("{url}.sha256"))
        })
        .collect())
}

/// https://nodejs.org/dist/index.json (só estáveis com zip win-x64).
pub fn parse_node(text: &str) -> Result<Vec<VersionInfo>, String> {
    #[derive(Deserialize)]
    struct NodeRelease {
        version: String,
        files: Vec<String>,
        #[serde(default)]
        lts: Value,
    }
    Ok(json::<Vec<NodeRelease>>(text)?
        .into_iter()
        .filter(|r| !r.version.contains('-') && r.files.iter().any(|f| f == "win-x64-zip"))
        .map(|r| {
            let v = &r.version; // com "v" na frente
            let mut info = VersionInfo::single(
                v.trim_start_matches('v'),
                format!("https://nodejs.org/dist/{v}/node-{v}-win-x64.zip"),
            )
            .sha256_url(format!("https://nodejs.org/dist/{v}/SHASUMS256.txt"));
            info.label = r.lts.as_str().map(|name| format!("LTS {name}"));
            info
        })
        .collect())
}

/// https://www.python.org/api/v2/downloads/release_file/?os=1 (arquivos Windows).
/// Usa o zip "embeddable"; versões só com código-fonte ficam de fora.
pub fn parse_python(text: &str) -> Result<Vec<VersionInfo>, String> {
    #[derive(Deserialize)]
    struct PyFile {
        url: String,
        #[serde(default)]
        sha256_sum: String,
    }
    let re = Regex::new(r"/ftp/python/(\d+\.\d+\.\d+)/python-(\d+\.\d+\.\d+)-embed-amd64\.zip$").unwrap();
    Ok(json::<Vec<PyFile>>(text)?
        .into_iter()
        .filter_map(|f| {
            let c = re.captures(&f.url)?;
            (c[1] == c[2] && at_least(&c[1], &[3, 8]))
                .then(|| VersionInfo::single(&c[1], &f.url).sha256(Some(f.sha256_sum.clone())))
        })
        .collect())
}

/// Releases do GitHub. `pattern` é uma regex para o nome do asset; se tiver
/// um grupo de captura, ele é a versão; senão, usa a tag.
pub fn parse_github(text: &str, pattern: &str) -> Result<Vec<VersionInfo>, String> {
    #[derive(Deserialize)]
    struct Release {
        tag_name: String,
        #[serde(default)]
        prerelease: bool,
        #[serde(default)]
        draft: bool,
        assets: Vec<Asset>,
    }
    #[derive(Deserialize)]
    struct Asset {
        name: String,
        browser_download_url: String,
        #[serde(default)]
        digest: Option<String>,
    }
    let re = Regex::new(pattern).map_err(|e| format!("assetPattern inválido '{pattern}': {e}"))?;
    let mut out = Vec::new();
    for rel in json::<Vec<Release>>(text)?.into_iter().filter(|r| !r.prerelease && !r.draft) {
        let found = rel.assets.iter().find_map(|a| re.captures(&a.name).map(|c| (a, c.get(1).map(|m| m.as_str().to_string()))));
        if let Some((asset, version)) = found {
            let version = version.unwrap_or_else(|| rel.tag_name.trim_start_matches('v').to_string());
            let sha = asset.digest.as_deref().and_then(|d| d.strip_prefix("sha256:")).map(String::from);
            out.push(VersionInfo::single(version, &asset.browser_download_url).sha256(sha));
        }
    }
    Ok(out)
}

/// RSS de arquivos de um projeto do SourceForge (WinSCP). `pattern` casa o
/// link de download inteiro e o grupo 1 é a versão; RCs ficam de fora pelo
/// próprio padrão. O SourceForge não publica SHA-256.
pub fn parse_sourceforge(text: &str, pattern: &str) -> Result<Vec<VersionInfo>, String> {
    let re = Regex::new(pattern).map_err(|e| format!("assetPattern inválido '{pattern}': {e}"))?;
    let mut out: Vec<VersionInfo> = Vec::new();
    for c in re.captures_iter(text) {
        let Some(v) = c.get(1).map(|m| m.as_str()) else { continue };
        if !out.iter().any(|x| x.version == v) {
            out.push(VersionInfo::single(v, &c[0]));
        }
    }
    Ok(out)
}

/// https://getcomposer.org/versions: versões estáveis (a atual e a LTS).
pub fn parse_composer(text: &str) -> Result<Vec<VersionInfo>, String> {
    let root: Value = json(text)?;
    let empty = Vec::new();
    Ok(root["stable"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter_map(|v| {
            let version = v["version"].as_str()?;
            let url = format!("https://getcomposer.org{}", v["path"].as_str()?);
            let mut info = VersionInfo::single(version, &url).sha256_url(format!("{url}.sha256sum"));
            info.label = v["lts"].as_bool().filter(|l| *l).map(|_| "LTS".to_string());
            Some(info)
        })
        .collect())
}

/// https://downloads.mariadb.org/rest-api/mariadb/: linhas estáveis
/// (id, é LTS?), da mais nova para a mais antiga.
pub fn parse_mariadb_majors(text: &str) -> Result<Vec<(String, bool)>, String> {
    let root: Value = json(text)?;
    let empty = Vec::new();
    Ok(root["major_releases"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter(|r| r["release_status"].as_str() == Some("Stable"))
        .filter_map(|r| {
            let id = r["release_id"].as_str()?.to_string();
            let lts = r["release_support_type"].as_str().is_some_and(|t| t.contains("Long Term"));
            Some((id, lts))
        })
        .collect())
}

/// https://downloads.mariadb.org/rest-api/mariadb/{linha}/latest/: o zip winx64.
pub fn parse_mariadb_latest(text: &str, lts: bool) -> Option<VersionInfo> {
    let root: Value = json(text).ok()?;
    let (version, rel) = root["releases"].as_object()?.iter().next()?;
    let file = rel["files"].as_array()?.iter().find(|f| f["file_name"].as_str().is_some_and(|n| n.ends_with("-winx64.zip")))?;
    // A API devolve http://; o mesmo endereço funciona em https.
    let url = file["file_download_url"].as_str()?.replacen("http://", "https://", 1);
    let sha = file["checksum"]["sha256sum"].as_str().map(String::from);
    let mut info = VersionInfo::single(version.as_str(), url).sha256(sha);
    info.label = lts.then(|| "LTS".to_string());
    Some(info)
}

/// Releases do GitHub sem arquivo anexo (OpenSearch, Elasticsearch): a versão
/// vem da tag ("v8.19.24" -> "8.19.24") e o link, do `urlTemplate`.
pub fn parse_github_versions(text: &str, template: &str) -> Result<Vec<VersionInfo>, String> {
    let list: Vec<Value> = json(text)?;
    Ok(list
        .iter()
        .filter(|r| !r["prerelease"].as_bool().unwrap_or(false) && !r["draft"].as_bool().unwrap_or(false))
        .filter_map(|r| {
            let v = r["tag_name"].as_str()?.trim_start_matches('v');
            v.split('.').all(|p| p.parse::<u32>().is_ok()).then(|| {
                let mm = v.split('.').take(2).collect::<Vec<_>>().join(".");
                VersionInfo::single(v, template.replace("{v}", v).replace("{mm}", &mm))
            })
        })
        .collect())
}

/* ---------- Fallback ---------- */

/// Lista de reserva do catálogo; monta a URL quando há `urlTemplate`
/// (`{v}` = versão, `{mm}` = maior.menor).
pub fn fallback(pkg: &Package) -> Vec<VersionInfo> {
    pkg.fallback_versions
        .iter()
        .map(|v| {
            let mm = v.split('.').take(2).collect::<Vec<_>>().join(".");
            match &pkg.source.url_template {
                Some(t) => VersionInfo::single(v, t.replace("{v}", v).replace("{mm}", &mm)),
                None => VersionInfo { version: v.clone(), label: None, downloads: Vec::new() },
            }
        })
        .collect()
}

/* ---------- HTTP ---------- */

fn http() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(concat!("jjphpmy/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())
}

pub(crate) async fn get_text(url: &str) -> Result<String, String> {
    http()?
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Falha ao acessar {url}: {e}"))?
        .error_for_status()
        .map_err(|e| format!("{url} respondeu com erro: {e}"))?
        .text()
        .await
        .map_err(|e| format!("Falha ao ler {url}: {e}"))
}

pub async fn fetch_online(pkg: &Package) -> Result<Vec<VersionInfo>, String> {
    let src = &pkg.source;
    let mut list = match src.kind.as_str() {
        "php-releases" => {
            let mut list = parse_php_releases(&get_text(&src.url).await?)?;
            // Versões antigas: se o arquivo falhar, segue só com as atuais.
            if let Ok(html) = get_text(&format!("{PHP_BASE}archives/")).await {
                let base = |v: &str| v.split('-').next().unwrap_or(v).to_string();
                for old in parse_php_archives(&html) {
                    if !list.iter().any(|v| base(&v.version) == base(&old.version)) {
                        list.push(old);
                    }
                }
            }
            list
        }
        "apachelounge" => parse_apache_lounge(&get_text(&src.url).await?),
        "nginx" => parse_nginx(&get_text(&src.url).await?),
        "mysql-tags" => parse_mysql_tags(&get_text(&src.url).await?)?,
        "postgresql-versions" => parse_postgresql(&get_text(&src.url).await?)?,
        "mongodb-current" => parse_mongodb(&get_text(&src.url).await?)?,
        "phpmyadmin" => parse_phpmyadmin(&get_text(&src.url).await?)?,
        "node-dist" => parse_node(&get_text(&src.url).await?)?,
        "python-files" => parse_python(&get_text(&src.url).await?)?,
        "github-releases" => parse_github(&get_text(&src.url).await?, src.asset_pattern.as_deref().unwrap_or(".*"))?,
        "static" => fallback(pkg),
        // Sem download automático: a "versão" é a página oficial, aberta no navegador.
        "manual" => vec![VersionInfo::single("site oficial", &src.url)],
        "composer" => parse_composer(&get_text(&src.url).await?)?,
        "github-versions" => parse_github_versions(&get_text(&src.url).await?, src.url_template.as_deref().unwrap_or(""))?,
        "mariadb" => {
            let mut list = Vec::new();
            // As 6 linhas estáveis mais novas (uma consulta por linha).
            for (id, lts) in parse_mariadb_majors(&get_text(&src.url).await?)?.into_iter().take(6) {
                if let Ok(text) = get_text(&format!("{}{id}/latest/", src.url)).await {
                    list.extend(parse_mariadb_latest(&text, lts));
                }
            }
            list
        }
        "sourceforge" => parse_sourceforge(&get_text(&src.url).await?, src.asset_pattern.as_deref().unwrap_or("$^"))?,
        other => return Err(format!("Tipo de fonte desconhecido: '{other}'")),
    };
    sort_desc(&mut list);
    Ok(list)
}

/* ---------- Cache (C:\jjphpmy\tmp\versions-cache.json) ---------- */

#[derive(Serialize, Deserialize)]
struct CacheEntry {
    fetched_at: u64,
    versions: Vec<VersionInfo>,
}

/// Evita que buscas paralelas sobrescrevam o arquivo umas das outras.
static CACHE_LOCK: Mutex<()> = Mutex::new(());

fn cache_file() -> PathBuf {
    base_path(&["tmp", "versions-cache.json"])
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn read_cache() -> HashMap<String, CacheEntry> {
    fs::read_to_string(cache_file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn cached(id: &str) -> Option<CacheEntry> {
    let _guard = CACHE_LOCK.lock().unwrap();
    read_cache().remove(id).filter(|e| !e.versions.is_empty())
}

fn store(id: &str, versions: &[VersionInfo]) {
    let _guard = CACHE_LOCK.lock().unwrap();
    let mut cache = read_cache();
    cache.insert(id.into(), CacheEntry { fetched_at: now(), versions: versions.to_vec() });
    let path = cache_file();
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string(&cache) {
        let _ = fs::write(path, json);
    }
}

/// Cache válido (< 24 h) -> fonte oficial -> cache vencido -> fallback.
/// `force` ignora o cache válido (botão "Atualizar versões").
pub async fn versions_for(pkg: &Package, force: bool) -> VersionList {
    let cache = cached(&pkg.id);
    if let Some(entry) = cache.as_ref().filter(|e| !force && now().saturating_sub(e.fetched_at) < CACHE_TTL_SECS) {
        return VersionList { versions: entry.versions.clone(), origin: "cache".into(), error: None };
    }
    let error = match fetch_online(pkg).await {
        Ok(versions) if !versions.is_empty() => {
            store(&pkg.id, &versions);
            return VersionList { versions, origin: "online".into(), error: None };
        }
        Ok(_) => "Nenhuma versão encontrada na fonte oficial.".to_string(),
        Err(e) => e,
    };
    match cache {
        Some(entry) => VersionList { versions: entry.versions, origin: "cache".into(), error: Some(error) },
        None => VersionList { versions: fallback(pkg), origin: "fallback".into(), error: Some(error) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;

    macro_rules! fixture {
        ($name:literal) => {
            include_str!(concat!("../tests/fixtures/", $name))
        };
    }

    fn sorted(mut list: Vec<VersionInfo>) -> Vec<VersionInfo> {
        sort_desc(&mut list);
        list
    }

    fn versions(list: &[VersionInfo]) -> Vec<&str> {
        list.iter().map(|v| v.version.as_str()).collect()
    }

    fn is_sha256(s: &Option<String>) -> bool {
        s.as_ref().is_some_and(|s| s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()))
    }

    #[test]
    fn mariadb_stable_lines_and_zip() {
        let majors = r#"{"major_releases":[
            {"release_id":"13.1","release_status":"RC","release_support_type":"Rolling"},
            {"release_id":"13.0","release_status":"Stable","release_support_type":"Rolling"},
            {"release_id":"11.4","release_status":"Stable","release_support_type":"Long Term Support"}]}"#;
        assert_eq!(parse_mariadb_majors(majors).unwrap(), [("13.0".to_string(), false), ("11.4".to_string(), true)]);
        let latest = r#"{"releases":{"11.4.13":{"files":[
            {"file_name":"mariadb-11.4.13-winx64.msi","file_download_url":"http://x/a.msi","checksum":{}},
            {"file_name":"mariadb-11.4.13-winx64.zip","file_download_url":"http://downloads.mariadb.org/rest-api/mariadb/11.4.13/mariadb-11.4.13-winx64.zip",
             "checksum":{"sha256sum":"d62986d433eeebfde218560b276103831604a61e929e87f1a17f5aebd80257e2"}}]}}}"#;
        let v = parse_mariadb_latest(latest, true).unwrap();
        assert_eq!(v.version, "11.4.13");
        assert_eq!(v.label.as_deref(), Some("LTS"));
        assert!(v.downloads[0].url.starts_with("https://downloads.mariadb.org/"));
        assert!(is_sha256(&v.downloads[0].sha256));
    }

    #[test]
    fn versions_from_github_tags() {
        let text = r#"[{"tag_name":"v9.5.6","prerelease":false},{"tag_name":"v9.6.0-rc1","prerelease":true},{"tag_name":"2.19.7"},{"tag_name":"nightly"}]"#;
        let list = parse_github_versions(text, "https://x/es-{v}-win.zip").unwrap();
        assert_eq!(versions(&list), ["9.5.6", "2.19.7"]);
        assert_eq!(list[0].downloads[0].url, "https://x/es-9.5.6-win.zip");
    }

    #[test]
    fn composer_stable_and_lts() {
        let text = r#"{"stable":[
            {"path":"/download/2.11.1/composer.phar","version":"2.11.1","lts":false},
            {"path":"/download/2.2.31/composer.phar","version":"2.2.31","lts":true}],
            "preview":[{"path":"/download/2.12.0-RC1/composer.phar","version":"2.12.0-RC1"}]}"#;
        let list = parse_composer(text).unwrap();
        assert_eq!(versions(&list), ["2.11.1", "2.2.31"]);
        assert_eq!(list[1].label.as_deref(), Some("LTS"));
        assert_eq!(list[0].downloads[0].url, "https://getcomposer.org/download/2.11.1/composer.phar");
        assert!(list[0].downloads[0].sha256_url.as_deref().unwrap().ends_with(".phar.sha256sum"));
    }

    #[test]
    fn winscp_from_sourceforge_rss() {
        let catalog = Catalog::embedded();
        let pattern = catalog.get("winscp").unwrap().source.asset_pattern.clone().unwrap();
        let rss = "<item><link>https://sourceforge.net/projects/winscp/files/WinSCP/6.7/WinSCP-6.7-Portable.zip/download</link></item>                   <item><link>https://sourceforge.net/projects/winscp/files/WinSCP/6.7/WinSCP-6.7.msi/download</link></item>                   <item><link>https://sourceforge.net/projects/winscp/files/WinSCP/6.6.4%20RC/WinSCP-6.6.4.RC-Portable.zip/download</link></item>                   <item><link>https://sourceforge.net/projects/winscp/files/WinSCP/6.5.7/WinSCP-6.5.7-Portable.zip/download</link></item>";
        let list = sorted(parse_sourceforge(rss, &pattern).unwrap());
        assert_eq!(versions(&list), ["6.7", "6.5.7"]);
        assert!(list[0].downloads[0].url.ends_with("/6.7/WinSCP-6.7-Portable.zip/download"));
    }

    #[test]
    fn notepadpp_portable_zip_only() {
        let catalog = Catalog::embedded();
        let pattern = catalog.get("notepadpp").unwrap().source.asset_pattern.clone().unwrap();
        let json = r#"[{"tag_name":"v8.9.8.1","assets":[
            {"name":"npp.8.9.8.1.portable.7z","browser_download_url":"a"},
            {"name":"npp.8.9.8.1.portable.arm64.zip","browser_download_url":"b"},
            {"name":"npp.8.9.8.1.portable.minimalist.x64.7z","browser_download_url":"c"},
            {"name":"npp.8.9.8.1.portable.x64.zip","browser_download_url":"https://x/npp.8.9.8.1.portable.x64.zip","digest":"sha256:beddf5548e75c97930d1575ba1c7d113e6e2b33d0a200310e734ca08e225f414"}]}]"#;
        let list = parse_github(json, &pattern).unwrap();
        assert_eq!(versions(&list), ["8.9.8.1"]);
        assert!(list[0].downloads[0].url.ends_with("portable.x64.zip"));
        assert!(is_sha256(&list[0].downloads[0].sha256));
    }

    #[test]
    fn sorts_numerically() {
        let list = sorted(vec![
            VersionInfo::single("9.7.4", ""),
            VersionInfo::single("26.10.0", ""),
            VersionInfo::single("8.4.13", ""),
            VersionInfo::single("8.4.9", ""),
            VersionInfo::single("8.4.13", ""),
        ]);
        assert_eq!(versions(&list), ["26.10.0", "9.7.4", "8.4.13", "8.4.9"]);
    }

    #[test]
    fn keeps_every_branch() {
        let many: Vec<_> = (0..30)
            .map(|p| VersionInfo::single(format!("8.5.{p}"), ""))
            .chain([VersionInfo::single("8.4.27", ""), VersionInfo::single("7.4.33", "")])
            .collect();
        let list = sorted(many);
        assert_eq!(list.len(), MAX_PER_LINE + 2);
        assert!(list.iter().any(|v| v.version == "7.4.33"));
    }

    #[test]
    fn php_releases_have_ts_and_nts_x64() {
        let list = parse_php_releases(fixture!("php-releases.json")).unwrap();
        let v85 = list.iter().find(|v| v.version.starts_with("8.5.")).expect("8.5 ausente");
        assert_eq!(v85.downloads.len(), 2);
        assert_eq!(v85.downloads[0].variant, "ts");
        assert!(v85.downloads[0].url.ends_with("-Win32-vs17-x64.zip"));
        assert!(v85.downloads[1].url.contains("-nts-Win32-vs17-x64.zip"));
        assert!(v85.downloads.iter().all(|d| is_sha256(&d.sha256)));
        assert!(list.iter().any(|v| v.version == "7.4.33"));
    }

    #[test]
    fn php_archives_skip_x86() {
        let list = parse_php_archives(fixture!("php-archives.html"));
        assert_eq!(versions(&list), ["8.3.0", "8.4.1"]);
        assert!(list.iter().all(|v| v.downloads.len() == 2));
        assert_eq!(list[1].downloads[0].url, format!("{PHP_BASE}archives/php-8.4.1-Win32-vs17-x64.zip"));
    }

    #[test]
    fn apache_lounge_win64_only() {
        let list = parse_apache_lounge(fixture!("apachelounge.html"));
        assert_eq!(versions(&list), ["2.4.69"]);
        let d = &list[0].downloads[0];
        assert_eq!(d.url, "https://www.apachelounge.com/download/VS18/binaries/httpd-2.4.69-261002-Win64-VS18.zip");
        assert_eq!(d.sha256_url.as_deref(), Some(format!("{}.txt", d.url).as_str()));
    }

    #[test]
    fn nginx_zip_links() {
        let list = sorted(parse_nginx(fixture!("nginx.html")));
        assert_eq!(list[0].version, "1.31.8");
        assert!(list.iter().any(|v| v.version == "1.26.3"));
        assert_eq!(list[0].downloads[0].url, "https://nginx.org/download/nginx-1.31.8.zip");
    }

    #[test]
    fn mysql_latest_per_line_from_8() {
        let list = sorted(parse_mysql_tags(fixture!("mysql-tags.json")).unwrap());
        assert_eq!(versions(&list), ["26.10.0", "9.7.4", "8.4.13", "8.0.46"]);
        assert_eq!(list[2].downloads[0].url, "https://cdn.mysql.com/Downloads/MySQL-8.4/mysql-8.4.13-winx64.zip");
    }

    #[test]
    fn postgresql_supported_only() {
        let list = sorted(parse_postgresql(fixture!("postgresql-versions.json")).unwrap());
        assert_eq!(list[0].version, "18.6");
        assert!(list.iter().all(|v| at_least(&v.version, &[13])));
        assert_eq!(
            list[0].downloads[0].url,
            "https://get.enterprisedb.com/postgresql/postgresql-18.6-1-windows-x64-binaries.zip"
        );
    }

    #[test]
    fn mongodb_windows_community() {
        let list = parse_mongodb(fixture!("mongodb-current.json")).unwrap();
        assert_eq!(list[0].version, "9.0.4");
        assert!(list[0].downloads[0].url.ends_with("mongodb-windows-x86_64-9.0.4.zip"));
        assert!(list.iter().all(|v| is_sha256(&v.downloads[0].sha256)));
    }

    #[test]
    fn phpmyadmin_releases() {
        let list = parse_phpmyadmin(fixture!("phpmyadmin-version.json")).unwrap();
        assert_eq!(versions(&list), ["5.2.3", "4.9.11"]);
        assert!(list[0].downloads[0].sha256_url.as_deref().unwrap().ends_with("all-languages.zip.sha256"));
    }

    #[test]
    fn node_stable_windows_with_lts_label() {
        let list = sorted(parse_node(fixture!("node-index.json")).unwrap());
        assert!(!list.iter().any(|v| v.version.contains('-') || v.version == "0.10.0"));
        let v22 = list.iter().find(|v| v.version == "22.11.0").unwrap();
        assert_eq!(v22.label.as_deref(), Some("LTS Jod"));
        assert_eq!(v22.downloads[0].url, "https://nodejs.org/dist/v22.11.0/node-v22.11.0-win-x64.zip");
    }

    #[test]
    fn python_embeddable_final_releases() {
        let list = sorted(parse_python(fixture!("python-files.json")).unwrap());
        assert_eq!(versions(&list), ["3.14.10", "3.14.2", "3.14.1", "3.14.0"]);
        assert!(is_sha256(&list[0].downloads[0].sha256));
        assert_eq!(list[1].downloads[0].sha256, None);
    }

    #[test]
    fn github_assets() {
        let mailpit = parse_github(fixture!("github-mailpit.json"), r"^mailpit-windows-amd64\.zip$").unwrap();
        assert_eq!(versions(&mailpit), ["1.33.0", "1.32.0"]);
        assert!(is_sha256(&mailpit[0].downloads[0].sha256));

        let redis = parse_github(fixture!("github-redis.json"), r"^Redis-(\d+\.\d+\.\d+)-Windows-x64-msys2\.zip$").unwrap();
        assert_eq!(versions(&redis), ["8.10.3"]);
        assert!(redis[0].downloads[0].url.ends_with("Redis-8.10.3-Windows-x64-msys2.zip"));

        let memcached = sorted(
            parse_github(fixture!("github-memcached.json"), r"^memcached-(\d+\.\d+\.\d+)-win64-mingw\.zip$").unwrap(),
        );
        assert_eq!(versions(&memcached), ["1.6.8"]);
    }

    #[test]
    fn catalog_patterns_match_fixtures() {
        let cat = Catalog::embedded();
        let pattern = |id: &str| cat.get(id).unwrap().source.asset_pattern.clone().unwrap();
        assert!(!parse_github(fixture!("github-mailpit.json"), &pattern("mailpit")).unwrap().is_empty());
        assert!(!parse_github(fixture!("github-redis.json"), &pattern("redis")).unwrap().is_empty());
        assert!(!parse_github(fixture!("github-memcached.json"), &pattern("memcached")).unwrap().is_empty());
    }

    /// Consulta as fontes reais: `cargo test live_sources -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_sources() {
        let cat = Catalog::embedded();
        let mut failures = Vec::new();
        for pkg in &cat.packages {
            match tauri::async_runtime::block_on(fetch_online(pkg)) {
                Ok(list) if !list.is_empty() => {
                    let top: Vec<_> = list.iter().take(4).map(|v| v.version.as_str()).collect();
                    println!("{:<11} {:>3} versões  {}  {}", pkg.id, list.len(), top.join(", "), list[0].downloads[0].url);
                }
                Ok(_) => failures.push(format!("{}: lista vazia", pkg.id)),
                Err(e) => failures.push(format!("{}: {e}", pkg.id)),
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn fallback_uses_url_template() {
        let cat = Catalog::embedded();
        let mysql = fallback(cat.get("mysql").unwrap());
        assert!(mysql[0].downloads[0].url.contains("/MySQL-8.4/mysql-8.4."));
        assert!(fallback(cat.get("apache").unwrap())[0].downloads.is_empty());
    }
}
