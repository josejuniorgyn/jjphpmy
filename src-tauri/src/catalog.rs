//! Catálogo de pacotes (`catalog/catalog.json`, embutido no executável).
//! A busca de versões fica em `versions.rs`.

use serde::{Deserialize, Serialize};

const CATALOG_JSON: &str = include_str!("../catalog/catalog.json");

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    /// Qual parser de versões usar (ver `versions::fetch_online`).
    #[serde(rename = "type")]
    pub kind: String,
    pub url: String,
    /// Só para `github-releases`: regex do nome do asset (grupo 1 = versão).
    #[serde(default)]
    pub asset_pattern: Option<String>,
    /// URL de download das `fallbackVersions`: `{v}` = versão, `{mm}` = maior.menor.
    #[serde(default)]
    pub url_template: Option<String>,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Package {
    pub id: String,
    pub name: String,
    /// webserver | language | database | cache | tool | system
    pub category: String,
    pub description: String,
    /// Pacotes ocultos não aparecem na lista; só entram como dependência.
    #[serde(default)]
    pub hidden: bool,
    /// Dependências obrigatórias.
    #[serde(default)]
    pub requires: Vec<String>,
    /// Cada grupo exige pelo menos um dos itens; se nenhum estiver
    /// selecionado, o primeiro do grupo é marcado automaticamente.
    #[serde(default)]
    pub requires_any_of: Vec<Vec<String>>,
    /// Id da linha na tela "Serviços e Portas", se o pacote for um serviço.
    #[serde(default)]
    pub service: Option<String>,
    pub source: Source,
    /// Usadas quando não há internet nem cache.
    #[serde(default)]
    pub fallback_versions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub schema_version: u32,
    pub packages: Vec<Package>,
}

impl Catalog {
    pub fn embedded() -> Self {
        serde_json::from_str(CATALOG_JSON).expect("catalog.json inválido")
    }

    pub fn get(&self, id: &str) -> Option<&Package> {
        self.packages.iter().find(|p| p.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_catalog_is_consistent() {
        let cat = Catalog::embedded();
        for p in &cat.packages {
            for dep in p.requires.iter().chain(p.requires_any_of.iter().flatten()) {
                assert!(cat.get(dep).is_some(), "{} depende de '{dep}', que não existe", p.id);
            }
            assert!(!p.fallback_versions.is_empty(), "{} sem fallbackVersions", p.id);
            if p.source.kind == "github-releases" {
                assert!(p.source.asset_pattern.is_some(), "{} sem assetPattern", p.id);
            }
        }
    }
}
