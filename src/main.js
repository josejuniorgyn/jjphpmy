// Interface do jjphpmy. Usa a API global do Tauri (app.withGlobalTauri = true).
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const SERVICE_NAMES = {
  apache: "Apache",
  mysql: "MySQL",
  postgresql: "PostgreSQL",
  nginx: "Nginx",
  memcached: "Memcached",
  redis: "Redis",
  mongodb: "MongoDB",
  mariadb: "MariaDB",
  opensearch: "OpenSearch",
  elasticsearch: "Elasticsearch",
  mailpit: "Mailpit",
};

const CATEGORY_NAMES = {
  webserver: "Servidores web",
  language: "Linguagens",
  database: "Bancos de dados",
  cache: "Cache",
  tool: "Ferramentas",
};

let config;
let catalog;
let lastResolution = null;
/** Pacotes marcados pelo usuário (os automáticos vêm da resolução). */
const userSelected = new Set();
const versionsLoaded = new Set();
/** id -> VersionInfo[] (com URLs de download), usado na instalação. */
const versionsById = new Map();

const $ = (sel) => document.querySelector(sel);

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function nameOf(id) {
  return catalog.packages.find((p) => p.id === id)?.name ?? id;
}

/* ---------- Idiomas ---------- */
// O texto da interface é escrito em português do Brasil (o padrão). Outros
// idiomas ficam em src/i18n/{código}.json, com a frase em português como
// chave: {"Pacotes": "Packages"}. Para incluir um idioma, crie o arquivo e
// acrescente-o em src/i18n/languages.json. Frases sem tradução ficam em português.
let dict = {};

function translateText(node) {
  // Frases longas do HTML ocupam várias linhas: compara com os espaços normalizados.
  const s = node.nodeValue.replace(/\s+/g, " ").trim();
  if (s && dict[s]) node.nodeValue = node.nodeValue.match(/^\s*/)[0] + dict[s] + node.nodeValue.match(/\s*$/)[0];
}

function translateTree(root) {
  if (root.nodeType === Node.TEXT_NODE) return translateText(root);
  if (root.nodeType !== Node.ELEMENT_NODE) return;
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  for (let n = walker.nextNode(); n; n = walker.nextNode()) translateText(n);
  for (const el of [root, ...root.querySelectorAll("[placeholder],[title]")]) {
    for (const attr of ["placeholder", "title"]) {
      const v = el.getAttribute?.(attr);
      if (v && dict[v]) el.setAttribute(attr, dict[v]);
    }
  }
}

async function loadLanguage(lang) {
  if (!lang || lang === "pt-BR") return;
  try {
    dict = await (await fetch(`i18n/${lang}.json`)).json();
  } catch {
    return;
  }
  document.documentElement.lang = lang;
  translateTree(document.body);
  // Textos criados depois (listas, avisos) também são traduzidos.
  new MutationObserver((mutations) => {
    for (const m of mutations) for (const n of m.addedNodes) translateTree(n);
  }).observe(document.body, { childList: true, subtree: true });
}

async function fillLanguages() {
  const select = document.querySelector('select[name="language"]');
  try {
    const list = await (await fetch("i18n/languages.json")).json();
    select.replaceChildren(...list.map((l) => new Option(l.name, l.code)));
  } catch {
    /* mantém as opções do HTML */
  }
}

let toastTimer;
function toast(message, isError = false) {
  const box = $("#toast");
  box.textContent = String(message);
  box.classList.toggle("error", isError);
  box.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (box.hidden = true), isError ? 8000 : 3500);
}

function showTab(id) {
  for (const tab of document.querySelectorAll(".tab")) {
    tab.classList.toggle("active", tab.dataset.tab === id);
  }
  for (const panel of document.querySelectorAll(".panel")) {
    panel.classList.toggle("active", panel.id === id);
  }
}

/* ---------- Pacotes ---------- */

function renderPackages() {
  const list = $("#package-list");
  list.replaceChildren();

  for (const [category, title] of Object.entries(CATEGORY_NAMES)) {
    const pkgs = catalog.packages.filter((p) => p.category === category && !p.hidden);
    if (!pkgs.length) continue;

    const box = el("div", "category");
    box.append(el("h2", null, title));

    for (const pkg of pkgs) {
      const row = el("div", "pkg");
      row.dataset.id = pkg.id;

      const checkbox = document.createElement("input");
      checkbox.type = "checkbox";
      checkbox.addEventListener("change", () => {
        if (checkbox.checked) userSelected.add(pkg.id);
        else userSelected.delete(pkg.id);
        updateResolution();
      });

      const info = el("div");
      info.append(
        el("div", "name", pkg.name),
        el("div", "desc", pkg.description),
        el("div", "installed"),
        el("div", "auto"),
        el("div", "origin"),
      );

      const select = document.createElement("select");
      select.disabled = true;
      select.append(new Option("Mais recente", ""));
      select.addEventListener("change", () => lastResolution && renderPlan(lastResolution));

      const progress = el("div", "progress");
      progress.hidden = true;
      progress.append(document.createElement("progress"), el("span"));

      row.append(checkbox, info, select, progress);
      box.append(row);
    }
    list.append(box);
  }
}

const ORIGIN_HINTS = {
  cache: "Sem conexão com a fonte oficial: usando a lista salva.",
  fallback: "Sem conexão com a fonte oficial: usando a lista de reserva.",
};

async function loadVersions(id, select, force = false) {
  if (versionsLoaded.has(id) && !force) return;
  versionsLoaded.add(id);
  const row = select.closest(".pkg");
  const previous = select.value;
  select.replaceChildren(new Option("Carregando versões...", ""));
  try {
    const list = await invoke("fetch_versions", { id, force });
    versionsById.set(id, list.versions);
    const latest = list.versions[0];
    select.replaceChildren(new Option(latest ? `Mais recente (${latest.version})` : "Mais recente", ""));
    for (const v of list.versions) {
      select.append(new Option(v.label ? `${v.version} (${v.label})` : v.version, v.version));
    }
    if ([...select.options].some((o) => o.value === previous)) select.value = previous;
    row.querySelector(".origin").textContent =
      list.origin === "cache" && !list.error ? "" : ORIGIN_HINTS[list.origin] ?? "";
    row.querySelector(".origin").title = list.error ?? "";
  } catch (err) {
    select.replaceChildren(new Option("Mais recente", ""));
    versionsLoaded.delete(id);
    toast(err, true);
  }
  if (lastResolution) renderPlan(lastResolution);
}

/** Busca de novo, ignorando o cache, as versões dos pacotes marcados. */
function refreshVersions() {
  for (const row of document.querySelectorAll(".pkg")) {
    if (row.querySelector("input[type=checkbox]").checked) {
      loadVersions(row.dataset.id, row.querySelector("select"), true);
    }
  }
}

async function updateResolution() {
  let res;
  try {
    res = await invoke("resolve_selection", { selected: [...userSelected] });
  } catch (err) {
    toast(err, true);
    return;
  }
  lastResolution = res;
  const auto = new Map(res.autoAdded.map((a) => [a.id, a.requiredBy]));

  for (const row of document.querySelectorAll(".pkg")) {
    const id = row.dataset.id;
    const checkbox = row.querySelector("input[type=checkbox]");
    const select = row.querySelector("select");
    const isAuto = auto.has(id) && !userSelected.has(id);

    checkbox.checked = userSelected.has(id) || isAuto;
    checkbox.disabled = isAuto;
    row.querySelector(".auto").textContent = isAuto
      ? `Marcado automaticamente (exigido por ${nameOf(auto.get(id))})`
      : "";
    select.disabled = !checkbox.checked;
    if (checkbox.checked) loadVersions(id, select);
  }

  renderPlan(res);
  $("#btn-install").disabled = res.installOrder.length === 0;
}

/** Versão escolhida; "Mais recente" vira a primeira da lista carregada. */
function chosenVersion(id) {
  return document.querySelector(`.pkg[data-id="${id}"] select`)?.value || versionsById.get(id)?.[0]?.version || null;
}

function renderPlan(res) {
  const plan = $("#plan");
  plan.replaceChildren();
  plan.hidden = res.installOrder.length === 0;
  if (plan.hidden) return;

  plan.append(el("strong", null, "Ordem de instalação"));
  const ol = el("ol");
  for (const id of res.installOrder) {
    let text = `${nameOf(id)} - ${chosenVersion(id) ?? "mais recente"}`;
    if (id === "php" && res.phpBuild) {
      text += res.phpBuild === "ts" ? " (Thread Safe, para Apache)" : " (Non-Thread Safe, php-cgi)";
    }
    ol.append(el("li", null, text));
  }
  plan.append(ol);
}

async function installSelected() {
  if (!lastResolution) return;
  const packages = lastResolution.installOrder.map((id) => ({ id, version: chosenVersion(id) }));
  const button = $("#btn-install");
  button.disabled = true;
  button.textContent = "Instalando...";
  for (const box of document.querySelectorAll(".pkg .progress")) box.hidden = true;
  let ok = false;
  try {
    config = await invoke("install_packages", { packages });
    toast("Instalação concluída.");
    ok = true;
  } catch (err) {
    toast(err, true);
  }
  // Instalou tudo: desmarca os pacotes (inclusive os automáticos) e volta as
  // versões para "Mais recente". Com erro, mantém a seleção para tentar de novo.
  if (ok) {
    userSelected.clear();
    for (const select of document.querySelectorAll(".pkg select")) select.value = "";
    await updateResolution();
  }
  button.textContent = "Instalar selecionados";
  button.disabled = false;
  markInstalled();
  renderInstalled();
  renderServices();
  renderSites(); // pacotes novos vêm marcados como "Ativado"
}

/* ---------- Progresso da instalação ---------- */

function formatMB(bytes) {
  return (bytes / 1048576).toLocaleString("pt-BR", { maximumFractionDigits: 1 }) + " MB";
}

const STAGE_NAMES = { download: "Baixando", verify: "Verificando", extract: "Extraindo" };

function onProgress(p) {
  const box = document.querySelector(`.pkg[data-id="${p.id}"] .progress`);
  if (!box) return; // pacote oculto (Visual C++)
  const bar = box.querySelector("progress");
  const text = box.querySelector("span");
  box.hidden = false;
  box.classList.toggle("error", p.stage === "error");

  if (p.stage === "download" || p.stage === "extract") {
    if (p.total) {
      bar.max = p.total;
      bar.value = p.received;
    } else {
      bar.removeAttribute("value"); // barra indeterminada
    }
  } else if (p.stage === "done" || p.stage === "skipped") {
    bar.max = 1;
    bar.value = 1;
  }

  let msg = p.message ?? STAGE_NAMES[p.stage] ?? "";
  if (p.stage === "download" && !p.message) {
    msg = `Baixando ${p.version}: ${formatMB(p.received)}` + (p.total ? ` de ${formatMB(p.total)}` : "");
  } else if (p.stage === "extract" && !p.message && p.total) {
    msg = `Extraindo: ${Math.round((100 * p.received) / p.total)}%`;
  }
  text.textContent = msg;
}

/* ---------- Instalados ---------- */

function variantName(variant) {
  return variant === "ts" ? "Thread Safe" : variant === "nts" ? "Non-Thread Safe" : "";
}

/** Na aba Pacotes, mostra as versões já instaladas de cada pacote. */
function markInstalled() {
  for (const row of document.querySelectorAll(".pkg")) {
    const list = config.installed.filter((p) => p.id === row.dataset.id);
    row.querySelector(".installed").textContent = list.length
      ? "Instalado: " +
        list
          .map((p) => [p.version, p.variant?.toUpperCase(), p.active ? "ativa" : null].filter(Boolean).join(" "))
          .join(", ")
      : "";
  }
}

function renderInstalled() {
  const body = $("#installed-body");
  body.replaceChildren();
  const list = [...config.installed].sort((a, b) => a.id.localeCompare(b.id) || b.version.localeCompare(a.version, undefined, { numeric: true }));
  $("#installed-empty").hidden = list.length > 0;

  for (const p of list) {
    const tr = el("tr");
    const radio = document.createElement("input");
    radio.type = "radio";
    radio.name = `active-${p.id}-${p.variant ?? ""}`;
    radio.checked = p.active;
    radio.addEventListener("change", () => changeInstalled("set_active_version", p));

    const name = nameOf(p.id) + (p.variant ? ` (${variantName(p.variant)})` : "");
    const folder = el("td", "path", p.path);

    const actions = el("td");
    const open = el("button", "link", "Abrir pasta");
    open.type = "button";
    open.addEventListener("click", () => window.__TAURI__.opener.revealItemInDir(p.path).catch((e) => toast(e, true)));
    const remove = el("button", "link danger", "Remover");
    remove.type = "button";
    remove.addEventListener("click", () => {
      if (confirm(`Remover ${name} ${p.version}?\nA pasta ${p.path} será apagada.`)) {
        changeInstalled("remove_installed", p);
      }
    });
    actions.append(open, remove);

    const radioCell = el("td");
    radioCell.append(radio);
    tr.append(radioCell, el("td", null, name), el("td", null, p.version), folder, actions);
    body.append(tr);
  }
}

async function changeInstalled(command, p) {
  try {
    config = await invoke(command, { id: p.id, version: p.version, variant: p.variant ?? null });
    if (command === "remove_installed") toast(`${nameOf(p.id)} ${p.version} removido.`);
  } catch (err) {
    toast(err, true);
  }
  markInstalled();
  renderInstalled();
}

/* ---------- Tela 1: Configurações ---------- */

function fillSettings() {
  const form = $("#settings-form");
  for (const input of form.elements) {
    if (!input.name) continue;
    const value = config.settings[input.name];
    if (input.type === "checkbox") input.checked = Boolean(value);
    else input.value = String(value ?? "");
  }
  $("#xdebug").checked = Boolean(config.settings.xdebug);
}

async function saveSettings(event) {
  event.preventDefault();
  const settings = { ...config.settings };
  for (const input of $("#settings-form").elements) {
    if (!input.name) continue;
    if (input.type === "checkbox") settings[input.name] = input.checked;
    else if (input.type === "number" || input.name === "backupIntervalHours") settings[input.name] = Number(input.value);
    else settings[input.name] = input.value.trim();
  }
  if (!settings.hostnamePattern.includes("{name}")) {
    toast("O hostname precisa conter {name}.", true);
    return;
  }
  try {
    await invoke("save_settings", { settings });
    const languageChanged = config.settings.language !== settings.language;
    config.settings = settings;
    if (languageChanged) return location.reload(); // aplica o novo idioma
    toast("Configurações salvas.");
  } catch (err) {
    toast(err, true);
  }
}

/* ---------- Tela 2: Serviços e Portas ---------- */

function portInput(value, field, id) {
  const input = document.createElement("input");
  input.type = "number";
  input.min = 1;
  input.max = 65535;
  input.value = value;
  input.dataset.id = id;
  input.dataset.field = field;
  return input;
}

function checkInput(checked, field, id) {
  const input = document.createElement("input");
  input.type = "checkbox";
  input.checked = checked;
  input.dataset.id = id;
  input.dataset.field = field;
  return input;
}

function renderServices() {
  const body = $("#services-body");
  body.replaceChildren();
  for (const s of config.services) {
    const tr = el("tr");
    const cells = [
      checkInput(s.enabled, "enabled", s.id),
      el("span", null, SERVICE_NAMES[s.id] ?? s.id),
      portInput(s.port, "port", s.id),
      s.sslPort != null ? checkInput(s.sslEnabled, "sslEnabled", s.id) : el("span", "hint", "-"),
      s.sslPort != null ? portInput(s.sslPort, "sslPort", s.id) : el("span", "hint", "-"),
      el("span", "status"),
      el("span", "service-actions"),
    ];
    tr.dataset.id = s.id;
    for (const c of cells) {
      const td = el("td");
      td.append(c);
      tr.append(td);
    }
    body.append(tr);
  }
  refreshStatus();
}

/* ---------- Status e controle dos serviços ---------- */

const STATE_NAMES = { running: "Rodando", stopped: "Parado", error: "Erro" };
const busy = new Set();

async function refreshStatus() {
  let list;
  try {
    list = await invoke("services_status");
  } catch (err) {
    return toast(err, true);
  }
  for (const st of list) {
    const row = document.querySelector(`#services-body tr[data-id="${st.id}"]`);
    if (!row) continue;
    const status = row.querySelector(".status");
    const actions = row.querySelector(".service-actions");
    status.className = `status ${st.installed ? st.state : "missing"}`;
    status.textContent = busy.has(st.id)
      ? "Aguarde..."
      : !st.installed
        ? "Não instalado"
        : STATE_NAMES[st.state] + (st.version ? ` (${st.version})` : "");
    status.title = st.message ?? (st.pid ? `PID ${st.pid}` : "");

    actions.replaceChildren();
    if (!st.installed || busy.has(st.id)) continue;
    const buttons = st.state === "running" ? [["Parar", "stop"], ["Reiniciar", "restart"]] : [["Iniciar", "start"]];
    for (const [label, action] of buttons) {
      const b = el("button", "link", label);
      b.type = "button";
      b.addEventListener("click", () => serviceAction(st.id, action));
      actions.append(b);
    }
    if (st.state === "error") {
      const b = el("button", "link danger", "Ver erro");
      b.type = "button";
      b.addEventListener("click", () => toast(st.message, true));
      actions.append(b);
    }
  }
}

async function serviceAction(id, action) {
  busy.add(id);
  refreshStatus();
  try {
    await invoke("service_action", { id, action });
  } catch (err) {
    toast(err, true);
  }
  busy.delete(id);
  refreshStatus();
}

async function runAll(command, button) {
  button.disabled = true;
  try {
    await invoke(command);
  } catch (err) {
    toast(err, true);
  }
  button.disabled = false;
  refreshStatus();
}

async function saveServices(event) {
  event.preventDefault();
  const services = config.services.map((s) => ({ ...s }));
  for (const input of document.querySelectorAll("#services-body input")) {
    const service = services.find((s) => s.id === input.dataset.id);
    const field = input.dataset.field;
    if (input.type === "checkbox") {
      service[field] = input.checked;
    } else {
      const port = Number(input.value);
      if (!Number.isInteger(port) || port < 1 || port > 65535) {
        toast(`Porta inválida para ${SERVICE_NAMES[service.id]}: ${input.value}`, true);
        return;
      }
      service[field] = port;
    }
  }
  try {
    await invoke("save_services", { services });
    config.services = services;
    toast("Serviços e portas salvos.");
  } catch (err) {
    toast(err, true);
  }
}

/* ---------- Backup ---------- */

async function renderBackupInfo() {
  try {
    const info = await invoke("backup_info");
    $("#backup-info").textContent = info.last ? `Último: ${info.last} (${info.count} no total)` : "Nenhum backup ainda.";
    $("#btn-backup-folder").dataset.dir = info.dir;
  } catch (err) {
    toast(err, true);
  }
}

async function backupNow(button) {
  button.disabled = true;
  button.textContent = "Fazendo backup...";
  try {
    toast(await invoke("backup_now"));
  } catch (err) {
    toast(err, true);
  }
  button.textContent = "Fazer backup agora";
  button.disabled = false;
  renderBackupInfo();
}

/* ---------- Importar do Laragon / restaurar ---------- */

/** Origens lidas: C:\laragon, C:\xampp e a pasta escolhida (se houver). */
let sources = null;

/** Origem da pasta do campo "Pasta de origem". */
function selectedSource() {
  const root = $("#imp-root").value.trim().toLowerCase();
  return sources?.find((s) => s.root.toLowerCase() === root);
}

/** Descrição da origem: o que tem e como os bancos serão importados. */
function describeSource(s) {
  if (!s.found) return `${s.name} não encontrado em ${s.root}`;
  const banco = s.mysql
    ? `${s.mysql.mariadb ? "MariaDB" : "MySQL"} ${s.mysql.version} com ${s.mysql.dataMb.toLocaleString("pt-BR")} MB de dados`
    : "sem banco de dados";
  const sites = s.kind === "xampp" ? "htdocs" : "www";
  return `${s.name} encontrado em ${s.root}: ${s.wwwItems} itens em ${sites}, ${banco}`;
}

async function renderLaragon(customPath) {
  const list = $("#import-sources");
  list.replaceChildren(el("li", "hint", "Procurando..."));
  try {
    sources = await invoke("import_detect", { path: customPath ?? ($("#imp-root").value.trim() || null) });
  } catch (err) {
    return toast(err, true);
  }
  // Uma linha para cada lugar padrão (Laragon e XAMPP), encontrado ou não.
  list.replaceChildren(
    ...sources.slice(0, 2).map((s) => el("li", s.found ? "found" : "hint", describeSource(s))),
  );
  if (!$("#imp-root").value.trim()) {
    const first = sources.find((s) => s.found);
    if (first) $("#imp-root").value = first.root;
  }
  renderSourceInfo();
}

/** Detalhes da pasta escolhida no campo. */
function renderSourceInfo() {
  const info = $("#import-info");
  const s = selectedSource();
  $("#btn-import").disabled = !s?.found;
  if (!s) {
    info.textContent = $("#imp-root").value.trim() ? 'Clique em "Verificar de novo" para ler esta pasta.' : "";
    return;
  }
  if (!s.found) {
    info.textContent = `Nenhum Laragon ou XAMPP em ${s.root}.`;
    return;
  }
  let text = `${describeSource(s)}.`;
  const mine = config.installed.find((p) => p.id === "mysql" && p.active);
  if (s.mysql && mine) {
    const mm = (v) => v.split(".").slice(0, 2).join(".");
    text += !s.mysql.mariadb && mm(mine.version) === mm(s.mysql.version)
      ? ` O MySQL do JJPHPMY (${mine.version}) é da mesma linha: a pasta de dados será copiada.`
      : ` Os bancos serão exportados e importados no MySQL ${mine.version} do JJPHPMY.`;
  } else if (s.mysql) {
    text += " Instale o MySQL na aba Pacotes antes de importar os bancos.";
  }
  if (s.hasPostgresql) text += " O PostgreSQL da origem ainda não é importado.";
  if (s.running.length) text += ` Programas da origem abertos: ${s.running.join(", ")}.`;
  info.textContent = text;
  $("#imp-mysql").disabled = !s.mysql;
  $("#imp-mysql").checked = Boolean(s.mysql);
}

async function browseSource() {
  try {
    const dir = await window.__TAURI__.dialog.open({ directory: true, title: "Pasta do Laragon ou do XAMPP" });
    if (!dir) return;
    $("#imp-root").value = dir;
    renderLaragon(dir);
  } catch (err) {
    toast(err, true);
  }
}

async function importLaragon(button) {
  const s = selectedSource();
  if (!s?.found) return toast("Escolha a pasta do Laragon ou do XAMPP.", true);
  const www = $("#imp-www").checked;
  const mysql = $("#imp-mysql").checked;
  if (!www && !mysql) return toast("Marque o que deseja importar.", true);
  const what = [www && "os sites", mysql && "os bancos de dados"].filter(Boolean).join(" e ");
  if (!confirm(`Importar ${what} do ${s.name} (${s.root})?\nIsso pode demorar alguns minutos.`)) return;
  const log = $("#import-log");
  log.textContent = "";
  log.hidden = false;
  button.disabled = true;
  try {
    const summary = await invoke("import_run", { root: s.root, www, mysql, password: $("#imp-password").value });
    log.textContent += `\n${summary}\n`;
    toast("Importação concluída.");
  } catch (err) {
    log.textContent += `\nErro: ${err}\n`;
    toast(err, true);
  }
  button.disabled = false;
  renderSites();
}

async function renderBackupList() {
  const select = $("#restore-file");
  select.replaceChildren();
  try {
    const list = await invoke("backup_list");
    for (const b of list) select.append(new Option(`${b.name} (${b.sizeKb.toLocaleString("pt-BR")} KB)`, b.name));
    if (!list.length) select.append(new Option("Nenhum backup ainda", ""));
    $("#btn-restore").disabled = !list.length;
  } catch (err) {
    toast(err, true);
  }
}

async function restoreBackup(button) {
  const name = $("#restore-file").value;
  if (!name || !confirm(`Restaurar ${name}?\nOs bancos do backup substituem os atuais com o mesmo nome.`)) return;
  button.disabled = true;
  try {
    toast(await invoke("backup_restore", { name }));
  } catch (err) {
    toast(err, true);
  }
  button.disabled = false;
}

/* ---------- Novo projeto ---------- */

let templates = [];

async function renderTemplates() {
  if (templates.length) return;
  try {
    templates = await invoke("project_templates");
  } catch (err) {
    return toast(err, true);
  }
  const select = $("#proj-template");
  select.replaceChildren(...templates.map((t) => new Option(t.name, t.id)));
  const describe = () => {
    $("#proj-description").textContent = templates.find((t) => t.id === select.value)?.description ?? "";
  };
  select.addEventListener("change", describe);
  describe();
}

async function createProject(button) {
  const name = $("#proj-name").value.trim();
  const template = $("#proj-template").value;
  if (!name) return toast("Informe o nome da pasta do projeto.", true);
  const log = $("#project-log");
  log.textContent = "";
  log.hidden = false;
  button.disabled = true;
  button.textContent = "Criando... (pode levar alguns minutos)";
  try {
    const msg = await invoke("create_project", { template, name });
    log.textContent += `\n${msg}\n`;
    toast(msg);
    $("#proj-name").value = "";
  } catch (err) {
    log.textContent += `\nErro: ${err}\n`;
    toast(err, true);
  }
  log.scrollTop = log.scrollHeight;
  button.textContent = "Criar projeto";
  button.disabled = false;
}

/* ---------- Hosts virtuais ---------- */

async function renderSites() {
  const list = $("#sites");
  let sites;
  try {
    sites = await invoke("list_sites");
  } catch (err) {
    return toast(err, true);
  }
  list.replaceChildren();
  if (!sites.length) {
    list.append(el("li", "hint", "Nenhum site ainda. Crie uma pasta dentro do Document Root."));
    return;
  }
  let shared = {};
  try {
    shared = await invoke("shared_sites");
  } catch {}
  // Versões de PHP instaladas para o "PHP por site" (chave = "versão-variante").
  const phps = config.installed.filter((p) => p.id === "php");
  for (const s of sites) {
    const li = el("li");
    const link = (url) => {
      const a = el("a", null, url);
      a.href = "#";
      a.title = s.root;
      a.addEventListener("click", (e) => {
        e.preventDefault();
        window.__TAURI__.opener.openUrl(url).catch((err) => toast(err, true));
      });
      return a;
    };
    li.append(link(s.url));
    if (s.httpsUrl) li.append(" ", link(s.httpsUrl));
    li.append(el("span", "hint", ` www\\${s.folder}${s.root.endsWith("/public") ? "\\public" : ""}`));
    li.append(siteTools(s, phps, shared[s.host]));
    list.append(li);
  }
}

/** PHP do site e "Compartilhar" (Cloudflare Tunnel). */
function siteTools(site, phps, sharedUrl) {
  const box = el("span", "site-tools");
  if (phps.length > 1) {
    const select = el("select");
    select.title = "Versão do PHP deste site";
    select.append(new Option("PHP padrão", ""));
    for (const p of phps) {
      const key = `${p.version}-${p.variant ?? "ts"}`;
      select.append(new Option(`PHP ${p.version}`, key));
    }
    // TS e NTS da mesma versão dão no mesmo: mostra só uma opção por versão.
    const seen = new Set();
    for (const opt of [...select.options]) {
      const v = opt.textContent;
      if (seen.has(v)) opt.remove();
      else seen.add(v);
    }
    select.value = config.sitePhp?.[site.host] ?? "";
    if (select.value === "" && config.sitePhp?.[site.host]) {
      // A chave salva é da outra variante: mostra pela versão.
      const ver = config.sitePhp[site.host].split("-").slice(0, -1).join("-");
      for (const opt of select.options) if (opt.value.startsWith(`${ver}-`)) select.value = opt.value;
    }
    select.addEventListener("change", async () => {
      try {
        await invoke("set_site_php", { host: site.host, key: select.value || null });
        config.sitePhp = { ...(config.sitePhp ?? {}) };
        if (select.value) config.sitePhp[site.host] = select.value;
        else delete config.sitePhp[site.host];
        toast(`PHP de ${site.host}: ${select.selectedOptions[0].textContent}.`);
      } catch (err) {
        toast(err, true);
        renderSites();
      }
    });
    box.append(select);
  }
  if (sharedUrl) {
    const a = el("a", "share-url", sharedUrl);
    a.href = "#";
    a.addEventListener("click", (e) => {
      e.preventDefault();
      window.__TAURI__.opener.openUrl(sharedUrl).catch((err) => toast(err, true));
    });
    const copy = el("button", "link", "Copiar link");
    copy.type = "button";
    copy.addEventListener("click", () => navigator.clipboard.writeText(sharedUrl).then(() => toast("Link copiado.")));
    const stop = el("button", "secondary", "Parar");
    stop.type = "button";
    stop.addEventListener("click", () => invoke("stop_share", { host: site.host }).catch((err) => toast(err, true)));
    box.append(a, copy, stop);
  } else {
    const share = el("button", "secondary", "Compartilhar");
    share.type = "button";
    share.title = "Cria um link público temporário (https://...trycloudflare.com) para este site";
    share.addEventListener("click", async () => {
      share.disabled = true;
      share.textContent = "Abrindo link...";
      try {
        const url = await invoke("share_site", { host: site.host });
        await navigator.clipboard.writeText(url).catch(() => {});
        toast(`Link público (copiado): ${url}\nQualquer pessoa com o link vê o site enquanto estiver compartilhado.`);
      } catch (err) {
        toast(err, true);
        share.disabled = false;
        share.textContent = "Compartilhar";
      }
    });
    box.append(share);
  }
  return box;
}

/* ---------- PHP: Xdebug e extensões ---------- */

async function toggleXdebug(event) {
  const box = event.currentTarget;
  const enabled = box.checked;
  box.disabled = true;
  if (enabled) toast("Baixando o Xdebug...");
  try {
    const msg = await invoke("set_xdebug", { enabled });
    config.settings.xdebug = enabled;
    toast(msg || (enabled ? "Xdebug ligado." : "Xdebug desligado."));
  } catch (err) {
    box.checked = !enabled;
    toast(err, true);
  }
  box.disabled = false;
}

async function renderPhpExtensions() {
  const grid = $("#php-ext");
  grid.replaceChildren();
  let info;
  try {
    info = await invoke("php_extensions");
  } catch (err) {
    $("#php-ext-info").textContent = String(err);
    $("#btn-ext-save").disabled = $("#btn-ext-default").disabled = true;
    return;
  }
  $("#btn-ext-save").disabled = $("#btn-ext-default").disabled = false;
  $("#php-ext-info").textContent = `PHP ${info.php}: ${info.enabled.length} de ${info.available.length} ligadas${info.default ? " (lista padrão do JJPHPMY)" : ""}. Reinicie o Apache/Nginx depois de salvar.`;
  for (const name of info.available) {
    const label = el("label", "check");
    const box = el("input");
    box.type = "checkbox";
    box.value = name;
    box.checked = info.enabled.includes(name);
    label.append(box, ` ${name}`);
    grid.append(label);
  }
}

async function savePhpExtensions(list) {
  try {
    await invoke("save_php_extensions", { list });
    config.settings.phpExtensions = list;
    toast(list ? "Extensões salvas." : "Extensões de volta ao padrão.");
    renderPhpExtensions();
  } catch (err) {
    toast(err, true);
  }
}

/** Regrava etc\ do zero, inclusive arquivos editados à mão. */
async function regenerateConfigs() {
  if (!confirm("Recriar os arquivos de configuração?\nAlterações feitas à mão em etc\\ serão perdidas.")) return;
  try {
    const report = await invoke("regenerate_configs");
    toast(report.written.length ? `Arquivos recriados:\n${report.written.join("\n")}` : "Nenhum pacote instalado para configurar.");
  } catch (err) {
    toast(err, true);
  }
}

/* ---------- Sobre / Contribua com o projeto ---------- */

const PIX_AMOUNTS = [20, 30, 40, 50, 60, 70, 80, 90, 100];
const brl = new Intl.NumberFormat("pt-BR", { style: "currency", currency: "BRL" });

function renderAbout() {
  window.__TAURI__.app.getVersion().then((v) => ($("#app-version").textContent = `v${v}`));
  $("#author-email").addEventListener("click", (e) => {
    e.preventDefault();
    window.__TAURI__.opener.openUrl("mailto:josejuniorgyn@gmail.com");
  });
  $("#repo-link").addEventListener("click", (e) => {
    e.preventDefault();
    window.__TAURI__.opener.openUrl("https://github.com/josejuniorgyn/jjphpmy");
  });

  const box = $("#amounts");
  for (const reais of PIX_AMOUNTS) {
    const btn = el("button", "amount", brl.format(reais));
    btn.type = "button";
    btn.addEventListener("click", () => showPix(reais, btn));
    box.append(btn);
  }
  // Já abre com o primeiro valor (R$ 20,00) escolhido e o QR Code gerado;
  // o valor clicado fica marcado até escolher outro.
  showPix(PIX_AMOUNTS[0], box.firstElementChild);

  $("#btn-copy-pix").addEventListener("click", async () => {
    const code = $("#pix-payload").value;
    if (!code) return toast("Selecione um valor primeiro.", true);
    await navigator.clipboard.writeText(code);
    toast("Código PIX copiado.");
  });
}

async function showPix(reais, button) {
  try {
    // O SVG é gerado pelo próprio back-end (pix.rs), não vem de fora.
    const { payload, svg } = await invoke("pix_qr", { amountCents: reais * 100 });
    for (const b of document.querySelectorAll(".amount")) b.classList.toggle("active", b === button);
    $("#pix-qr").innerHTML = svg;
    $("#pix-amount").textContent = brl.format(reais);
    $("#pix-payload").value = payload;
    $("#pix").hidden = false;
  } catch (err) {
    toast(err, true);
  }
}

/* ---------- Inicialização ---------- */

async function init() {
  for (const tab of document.querySelectorAll(".tab")) {
    tab.addEventListener("click", () => showTab(tab.dataset.tab));
  }
  $("#settings-form").addEventListener("submit", saveSettings);
  $("#xdebug").addEventListener("change", toggleXdebug);
  $("#php-ext-box").addEventListener("toggle", (e) => e.currentTarget.open && renderPhpExtensions());
  $("#btn-ext-save").addEventListener("click", () =>
    savePhpExtensions([...document.querySelectorAll("#php-ext input:checked")].map((b) => b.value)),
  );
  $("#btn-ext-default").addEventListener("click", () => savePhpExtensions(null));
  $("#services-form").addEventListener("submit", saveServices);
  $("#btn-regenerate").addEventListener("click", regenerateConfigs);
  $("#btn-start-all").addEventListener("click", (e) => runAll("start_all_services", e.currentTarget));
  $("#btn-stop-all").addEventListener("click", (e) => runAll("stop_all_services", e.currentTarget));
  $("#btn-terminal").addEventListener("click", () => invoke("open_terminal").catch((err) => toast(err, true)));
  await listen("services-changed", () => refreshStatus());
  await listen("sites-changed", () => renderSites());
  await listen("backup-done", () => {
    renderBackupInfo();
    renderBackupList();
  });
  await listen("import-log", (e) => {
    const log = $("#import-log");
    log.hidden = false;
    log.textContent += `${e.payload}\n`;
    log.scrollTop = log.scrollHeight;
  });
  $("#btn-import").addEventListener("click", (e) => importLaragon(e.currentTarget));
  $("#btn-update").addEventListener("click", async (e) => {
    const b = e.currentTarget;
    b.disabled = true;
    toast("Procurando atualização...");
    try {
      toast(await invoke("check_update", { install: true }));
    } catch (err) {
      toast(err, true);
    }
    b.disabled = false;
  });
  $("#btn-laragon-refresh").addEventListener("click", () => renderLaragon());
  $("#btn-imp-browse").addEventListener("click", browseSource);
  $("#btn-create-project").addEventListener("click", (e) => createProject(e.currentTarget));
  await listen("project-log", (e) => {
    const log = $("#project-log");
    log.hidden = false;
    log.textContent += `${e.payload}\n`;
    log.scrollTop = log.scrollHeight;
  });
  document.querySelector('.tab[data-tab="projetos"]').addEventListener("click", renderTemplates);
  $("#imp-root").addEventListener("input", renderSourceInfo);
  $("#btn-restore").addEventListener("click", (e) => restoreBackup(e.currentTarget));
  // A detecção lê o disco: só quando a aba Importar é aberta.
  document.querySelector('.tab[data-tab="importar"]').addEventListener("click", () => {
    if (!sources) renderLaragon();
    renderBackupList();
  });
  $("#btn-backup").addEventListener("click", (e) => backupNow(e.currentTarget));
  $("#btn-backup-folder").addEventListener("click", (e) => {
    const dir = e.currentTarget.dataset.dir;
    if (dir) window.__TAURI__.opener.revealItemInDir(dir).catch((err) => toast(err, true));
  });
  $("#btn-install").addEventListener("click", installSelected);
  $("#btn-refresh-versions").addEventListener("click", refreshVersions);

  await listen("navigate", (e) => {
    showTab(e.payload);
    if (e.payload === "projetos") renderTemplates();
  });
  await listen("toast", (e) => toast(e.payload));
  await listen("install-progress", (e) => onProgress(e.payload));

  [config, catalog] = await Promise.all([invoke("get_config"), invoke("get_catalog")]);
  await fillLanguages();
  await loadLanguage(config.settings.language);
  renderPackages();
  markInstalled();
  renderInstalled();
  renderSites();
  renderBackupInfo();
  fillSettings();
  renderServices();
  renderAbout();
  showTab("pacotes");
}

init().catch((err) => toast(err, true));
