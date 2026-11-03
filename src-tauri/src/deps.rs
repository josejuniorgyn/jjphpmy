//! Resolução de dependências: marca automaticamente o que os pacotes
//! selecionados exigem e calcula a ordem de instalação.

use crate::catalog::{Catalog, Package};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap};

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AutoAdded {
    pub id: String,
    pub required_by: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Resolution {
    /// Todos os pacotes a instalar, dependências antes de quem depende delas.
    pub install_order: Vec<String>,
    /// Pacotes marcados automaticamente e quem os exigiu.
    pub auto_added: Vec<AutoAdded>,
    /// "ts" (Thread Safe, para mod_php do Apache) ou "nts" (php-cgi/Nginx).
    pub php_build: Option<String>,
}

pub fn resolve(catalog: &Catalog, selected: &[String]) -> Result<Resolution, String> {
    for id in selected {
        if catalog.get(id).is_none() {
            return Err(format!("Pacote desconhecido: '{id}'"));
        }
    }

    let mut chosen: BTreeSet<String> = selected.iter().cloned().collect();
    let mut auto_added = Vec::new();

    // Expande até não haver mais nada a adicionar (ponto fixo).
    loop {
        let mut changed = false;
        for id in chosen.clone() {
            let pkg = catalog.get(&id).expect("id validado");
            for dep in &pkg.requires {
                if chosen.insert(dep.clone()) {
                    auto_added.push(AutoAdded { id: dep.clone(), required_by: id.clone() });
                    changed = true;
                }
            }
            for group in &pkg.requires_any_of {
                if let Some(first) = group.first() {
                    if !group.iter().any(|g| chosen.contains(g)) && chosen.insert(first.clone()) {
                        auto_added.push(AutoAdded { id: first.clone(), required_by: id.clone() });
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }

    let mut state = HashMap::new();
    let mut install_order = Vec::new();
    for id in &chosen {
        visit(catalog, &chosen, id, &mut state, &mut install_order)?;
    }

    let php_build = chosen
        .contains("php")
        .then(|| if chosen.contains("apache") { "ts" } else { "nts" }.to_string());

    Ok(Resolution { install_order, auto_added, php_build })
}

/// Dependências efetivas de um pacote dentro da seleção atual.
fn deps_in<'a>(pkg: &'a Package, chosen: &'a BTreeSet<String>) -> impl Iterator<Item = &'a String> {
    pkg.requires
        .iter()
        .chain(pkg.requires_any_of.iter().flatten().filter(move |d| chosen.contains(*d)))
}

/// Ordenação topológica por DFS (1 = visitando, 2 = concluído).
fn visit(
    catalog: &Catalog,
    chosen: &BTreeSet<String>,
    id: &str,
    state: &mut HashMap<String, u8>,
    out: &mut Vec<String>,
) -> Result<(), String> {
    match state.get(id) {
        Some(2) => return Ok(()),
        Some(1) => return Err(format!("Dependência circular envolvendo '{id}'")),
        _ => {}
    }
    state.insert(id.to_string(), 1);
    let pkg = catalog.get(id).ok_or_else(|| format!("Pacote desconhecido: '{id}'"))?;
    for dep in deps_in(pkg, chosen) {
        visit(catalog, chosen, dep, state, out)?;
    }
    state.insert(id.to_string(), 2);
    out.push(id.to_string());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    fn pos(order: &[String], id: &str) -> usize {
        order.iter().position(|x| x == id).unwrap_or_else(|| panic!("{id} ausente"))
    }

    #[test]
    fn phpmyadmin_pulls_its_dependencies() {
        let r = resolve(&Catalog::embedded(), &sel(&["phpmyadmin"])).unwrap();
        for id in ["php", "mysql", "apache", "vcredist"] {
            assert!(pos(&r.install_order, id) < pos(&r.install_order, "phpmyadmin"));
        }
        assert_eq!(r.php_build.as_deref(), Some("ts"));
        assert!(r.auto_added.iter().any(|a| a.id == "apache" && a.required_by == "phpmyadmin"));
    }

    #[test]
    fn nginx_keeps_nginx_and_uses_nts() {
        let r = resolve(&Catalog::embedded(), &sel(&["nginx", "phpmyadmin"])).unwrap();
        assert!(!r.install_order.contains(&"apache".to_string()));
        assert_eq!(r.php_build.as_deref(), Some("nts"));
    }

    #[test]
    fn independent_package_has_no_extras() {
        let r = resolve(&Catalog::embedded(), &sel(&["redis"])).unwrap();
        assert_eq!(r.install_order, sel(&["redis"]));
        assert!(r.auto_added.is_empty());
        assert_eq!(r.php_build, None);
    }

    #[test]
    fn unknown_package_is_error() {
        assert!(resolve(&Catalog::embedded(), &sel(&["xyz"])).is_err());
    }
}
