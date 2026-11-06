//! Etapa 7: HTTPS local. Uma CA própria do JJPHPMY (criada uma vez) assina
//! um certificado com `localhost`, `127.0.0.1` e cada site `{name}.test`
//! (mais `*.{name}.test`). A CA tem **name constraints**: só vale para
//! localhost, `.test` e 127.0.0.0/8 — mesmo que a chave vaze, ela não serve
//! para falsificar outros sites.
//!
//! Arquivos em `etc\ssl\`: `jjphpmy-ca.crt/.key`, `server.crt/.key` e
//! `server.json` (nomes e validade do certificado atual).

use crate::confgen::Layout;
use crate::config::Config;
use crate::installer::hidden;
use rcgen::{
    BasicConstraints, CertificateParams, CidrSubnet, DistinguishedName, DnType, ExtendedKeyUsagePurpose,
    GeneralSubtree, IsCa, Issuer, KeyPair, KeyUsagePurpose, NameConstraints, SanType, SerialNumber,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;
use std::process::Command;
use time::{Duration, OffsetDateTime};

pub const CA_NAME: &str = "JJPHPMY Local CA";
/// Valididade do certificado dos sites (limite aceito pelos navegadores).
const SERVER_DAYS: i64 = 397;
/// Renova o certificado quando faltar menos que isso.
const RENEW_DAYS: i64 = 30;

pub fn dir(layout: &Layout) -> PathBuf {
    layout.dir(&["etc", "ssl"])
}
pub fn ca_cert(layout: &Layout) -> PathBuf {
    dir(layout).join("jjphpmy-ca.crt")
}
fn ca_key(layout: &Layout) -> PathBuf {
    dir(layout).join("jjphpmy-ca.key")
}
fn ca_serial_file(layout: &Layout) -> PathBuf {
    dir(layout).join("jjphpmy-ca.serial")
}
pub fn server_cert(layout: &Layout) -> PathBuf {
    dir(layout).join("server.crt")
}
pub fn server_key(layout: &Layout) -> PathBuf {
    dir(layout).join("server.key")
}
fn state_file(layout: &Layout) -> PathBuf {
    dir(layout).join("server.json")
}

/// Porta HTTPS do servidor, se o SSL estiver ligado em Serviços e Portas.
pub fn port(cfg: &Config, server: &str) -> Option<u16> {
    cfg.service(server).filter(|s| s.ssl_enabled).and_then(|s| s.ssl_port)
}

pub fn any_enabled(cfg: &Config) -> bool {
    port(cfg, "apache").is_some() || port(cfg, "nginx").is_some()
}

/// Linhas `listen ... ssl` do Nginx (vazio sem SSL). `indent` = recuo do bloco.
pub fn nginx_listen(cfg: &Config, layout: &Layout, indent: &str) -> String {
    match port(cfg, "nginx") {
        Some(p) => format!(
            "{indent}listen       {p} ssl;\n{indent}ssl_certificate      \"{}\";\n{indent}ssl_certificate_key  \"{}\";\n",
            crate::confgen::fwd(server_cert(layout)),
            crate::confgen::fwd(server_key(layout))
        ),
        None => String::new(),
    }
}

/// Bytes aleatórios sem dependência extra (RandomState é semeado pelo sistema).
fn random_bytes(n: usize) -> Vec<u8> {
    use std::hash::{BuildHasher, Hasher};
    let mut out = Vec::new();
    while out.len() < n {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
        out.extend(h.finish().to_be_bytes());
    }
    out.truncate(n);
    out[0] = (out[0] & 0x7f) | 0x10; // positivo e sem zero à esquerda
    out
}

fn err(what: &str) -> impl Fn(rcgen::Error) -> String + '_ {
    move |e| format!("{what}: {e}")
}

fn write(path: PathBuf, content: &str) -> Result<(), String> {
    fs::write(&path, content).map_err(|e| format!("Não foi possível gravar {}: {e}", path.display()))
}

/* ---------- CA ---------- */

fn ca_params() -> CertificateParams {
    let mut p = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, CA_NAME);
    dn.push(DnType::OrganizationName, "JJPHPMY");
    p.distinguished_name = dn;
    p.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    p.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign, KeyUsagePurpose::DigitalSignature];
    p.name_constraints = Some(NameConstraints {
        permitted_subtrees: vec![
            GeneralSubtree::DnsName("localhost".into()),
            GeneralSubtree::DnsName("test".into()),
            GeneralSubtree::IpAddress(CidrSubnet::V4([127, 0, 0, 0], [255, 0, 0, 0])),
            GeneralSubtree::IpAddress(CidrSubnet::V6(Ipv6Addr::LOCALHOST.octets(), [0xff; 16])),
        ],
        excluded_subtrees: vec![],
    });
    let now = OffsetDateTime::now_utc();
    p.not_before = now - Duration::days(1);
    p.not_after = now + Duration::days(3650);
    p.serial_number = Some(SerialNumber::from_slice(&random_bytes(16)));
    p
}

/// Cria a CA se ainda não existir. Devolve true se criou agora.
pub fn ensure_ca(layout: &Layout) -> Result<bool, String> {
    if ca_cert(layout).exists() && ca_key(layout).exists() {
        return Ok(false);
    }
    fs::create_dir_all(dir(layout)).map_err(|e| format!("Não foi possível criar {}: {e}", dir(layout).display()))?;
    let key = KeyPair::generate().map_err(err("Falha ao gerar a chave da CA"))?;
    let params = ca_params();
    let serial: String = params.serial_number.as_ref().map(|s| s.as_ref().iter().map(|b| format!("{b:02X}")).collect()).unwrap_or_default();
    let cert = params.self_signed(&key).map_err(err("Falha ao criar a CA"))?;
    write(ca_key(layout), &key.serialize_pem())?;
    write(ca_cert(layout), &cert.pem())?;
    write(ca_serial_file(layout), &serial)?;
    // O certificado antigo foi assinado por outra CA: recria.
    let _ = fs::remove_file(state_file(layout));
    Ok(true)
}

/// Número de série da CA em hexadecimal (como o Windows mostra).
pub fn ca_serial(layout: &Layout) -> Option<String> {
    fs::read_to_string(ca_serial_file(layout)).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/* ---------- Certificado dos sites ---------- */

#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
struct ServerState {
    names: Vec<String>,
    expires: i64,
}

/// Nomes do certificado: localhost e, para cada site, `host` e `*.host`.
pub fn names(hosts: &[String]) -> Vec<String> {
    let mut out = vec!["localhost".to_string()];
    for h in hosts {
        out.push(h.clone());
        out.push(format!("*.{h}"));
    }
    out
}

/// Garante a CA e um certificado válido com os nomes atuais. Devolve true
/// se o certificado mudou (servidores rodando precisam reiniciar).
pub fn ensure_server(layout: &Layout, hosts: &[String]) -> Result<bool, String> {
    ensure_ca(layout)?;
    let names = names(hosts);
    let current: ServerState =
        fs::read_to_string(state_file(layout)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let fresh = current.expires - OffsetDateTime::now_utc().unix_timestamp() > RENEW_DAYS * 86400;
    if current.names == names && fresh && server_cert(layout).exists() && server_key(layout).exists() {
        return Ok(false);
    }

    let ca_key_pem = fs::read_to_string(ca_key(layout)).map_err(|e| format!("Chave da CA ilegível: {e}"))?;
    let ca_key = KeyPair::from_pem(&ca_key_pem).map_err(err("Chave da CA inválida"))?;
    let ca_pem = fs::read_to_string(ca_cert(layout)).map_err(|e| format!("Certificado da CA ilegível: {e}"))?;
    let issuer = Issuer::from_ca_cert_pem(&ca_pem, ca_key).map_err(err("Certificado da CA inválido"))?;

    let mut p = CertificateParams::default();
    p.distinguished_name.push(DnType::CommonName, "JJPHPMY localhost");
    p.subject_alt_names = names
        .iter()
        .map(|n| n.as_str().try_into().map(SanType::DnsName))
        .collect::<Result<Vec<_>, _>>()
        .map_err(err("Nome de site inválido para o certificado"))?;
    p.subject_alt_names.push(SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    p.subject_alt_names.push(SanType::IpAddress(IpAddr::V6(Ipv6Addr::LOCALHOST)));
    p.key_usages = vec![KeyUsagePurpose::DigitalSignature, KeyUsagePurpose::KeyEncipherment];
    p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let now = OffsetDateTime::now_utc();
    p.not_before = now - Duration::days(1);
    p.not_after = now + Duration::days(SERVER_DAYS);
    p.serial_number = Some(SerialNumber::from_slice(&random_bytes(16)));

    let key = KeyPair::generate().map_err(err("Falha ao gerar a chave do certificado"))?;
    let cert = p.signed_by(&key, &issuer).map_err(err("Falha ao assinar o certificado"))?;
    write(server_key(layout), &key.serialize_pem())?;
    // Servidores enviam a cadeia: certificado do site seguido do da CA.
    write(server_cert(layout), &format!("{}{}", cert.pem(), ca_pem))?;
    let state = ServerState { names, expires: p.not_after.unix_timestamp() };
    write(state_file(layout), &serde_json::to_string_pretty(&state).unwrap_or_default())?;
    Ok(true)
}

/* ---------- Confiança no Windows ---------- */

/// A CA já está nas "Autoridades de Certificação Raiz Confiáveis" do usuário?
pub fn ca_trusted(layout: &Layout) -> bool {
    let Some(serial) = ca_serial(layout) else { return false };
    let script = format!(
        "if (Get-ChildItem Cert:\\CurrentUser\\Root | Where-Object {{ $_.SerialNumber -eq '{serial}' }}) {{ exit 0 }} else {{ exit 1 }}"
    );
    hidden(Command::new("powershell").args(["-NoProfile", "-Command", &script]))
        .status()
        .is_ok_and(|s| s.success())
}

/// Instala a CA no repositório do usuário (sem administrador). O Windows
/// mostra um aviso de segurança e o usuário precisa clicar em "Sim".
pub fn trust_ca(layout: &Layout) -> Result<(), String> {
    let status = hidden(Command::new("certutil").args(["-user", "-addstore", "Root"]).arg(ca_cert(layout)))
        .status()
        .map_err(|e| format!("Não foi possível executar o certutil: {e}"))?;
    if status.success() && ca_trusted(layout) {
        Ok(())
    } else {
        Err("O certificado da CA do JJPHPMY não foi instalado (o aviso do Windows foi recusado?). \
             Sem ele, o navegador mostra \"Não seguro\" nos sites https."
            .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(name: &str) -> Layout {
        let base = std::env::temp_dir().join(format!("jjphpmy-ssl-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        Layout { base }
    }

    #[test]
    fn creates_ca_once_and_reissues_on_change() {
        let l = layout("ca");
        assert!(ensure_ca(&l).unwrap());
        assert!(!ensure_ca(&l).unwrap());
        let serial = ca_serial(&l).unwrap();
        assert_eq!(serial.len(), 32);

        let hosts = vec!["blog.test".to_string()];
        assert!(ensure_server(&l, &hosts).unwrap());
        assert!(!ensure_server(&l, &hosts).unwrap(), "sem mudanças não recria");
        assert!(ensure_server(&l, &["blog.test".into(), "loja.test".into(), "pontolink_site.test".into()]).unwrap());
        let chain = fs::read_to_string(server_cert(&l)).unwrap();
        assert_eq!(chain.matches("BEGIN CERTIFICATE").count(), 2);
        let _ = fs::remove_dir_all(&l.base);
    }

    #[test]
    fn names_include_wildcards() {
        assert_eq!(names(&["blog.test".into()]), ["localhost", "blog.test", "*.blog.test"]);
    }
}
