<div align="center">

<img src="src-tauri/icons/128x128.png" alt="JJPHPMY" width="96">

# JJPHPMY

**Ambiente de desenvolvimento PHP para Windows: leve, rápido e sem complicação.**

Apache, Nginx, PHP, MySQL, PostgreSQL, phpMyAdmin, Node.js, Composer, Git e muito mais,
instalados com um clique, só o que você escolher.

[![Baixar para Windows](https://img.shields.io/badge/Baixar%20para%20Windows-2563eb?style=for-the-badge&logo=windows&logoColor=white)](https://github.com/josejuniorgyn/jjphpmy/releases/latest)

[![Versão](https://img.shields.io/github/v/release/josejuniorgyn/jjphpmy?label=vers%C3%A3o&color=2563eb)](https://github.com/josejuniorgyn/jjphpmy/releases/latest)
![Windows 10/11](https://img.shields.io/badge/Windows-10%20%7C%2011-0078d4)
![Instalador](https://img.shields.io/badge/instalador-~2,6%20MB-success)
![Sem administrador](https://img.shields.io/badge/n%C3%A3o%20pede-administrador-success)

</div>

---

## Por que usar o JJPHPMY?

- 🪶 **Leve de verdade:** o instalador tem cerca de 2,6 MB. Nada vem embutido: você instala só o que vai usar.
- ⚡ **Um clique para tudo:** escolha os programas, clique em **Instalar** e depois em **Iniciar tudo**.
- 🔒 **Downloads oficiais e verificados:** cada programa vem do site oficial do projeto, com conferência SHA-256 quando a fonte publica o hash.
- 🧩 **Dependências automáticas:** marcou o phpMyAdmin? PHP, MySQL e Apache são marcados sozinhos.
- 🌐 **Sites `.test` automáticos:** crie uma pasta em `www` e ela vira `http://nome-da-pasta.test`, inclusive com HTTPS.
- 🛡️ **Não pede administrador** para instalar nem para usar (só o Windows pede permissão uma vez para atualizar o arquivo `hosts`).
- 🔄 **Atualiza sozinho** pelo GitHub, com assinatura digital.
- 🇧🇷 **Em português do Brasil**, com opção de inglês.

## O que você pode instalar

| Categoria | Programas |
|---|---|
| Servidores web | **Apache** (com PHP Thread Safe) e **Nginx** (com PHP-CGI) |
| Linguagens | **PHP** (do 7.4 ao mais novo, várias versões lado a lado), **Composer**, **Node.js** (com rótulo LTS), **Python** |
| Bancos de dados | **MySQL**, **PostgreSQL**, **MongoDB**, **phpMyAdmin** |
| Cache | **Redis**, **Memcached** |
| Ferramentas | **Mailpit** (caixa de e-mails de teste), **Git** + Git Bash, **Notepad++**, **WinSCP** (SFTP/FTP) e atalho para o **FileZilla** |

Escolha a versão de cada um ou deixe em **Mais recente**. Dá para ter várias versões instaladas e trocar a ativa com um clique.

## Recursos

### 🚀 Novo projeto com um clique
Escolha um modelo, digite o nome e pronto: o projeto é criado em `www` e já abre em `http://nome.test`.

- Laravel + **Vue** (Inertia), Laravel + **React** (Inertia), Laravel + **Livewire**
- **Laravel** (API ou projeto simples) e **Symfony**
- **Vite** + Vue, React ou Svelte
- **PHP puro**

### 🖥️ Serviços sob controle
- **Iniciar, parar e reiniciar** cada serviço, ou todos de uma vez, pela janela ou pelo ícone da bandeja.
- **Avisa quem está usando a porta** (ex.: "a porta 80 está em uso por httpd.exe") antes de iniciar.
- Se um serviço parar sozinho, você é avisado com as últimas linhas do log.
- Ao fechar o JJPHPMY, **nada fica rodando escondido**.

### 🌐 Hosts virtuais e HTTPS automáticos
- Cada pasta em `www` vira um site `{nome}.test`, também com subdomínios (`api.loja.test`).
- Projetos Laravel abrem direto na pasta `public`.
- **HTTPS local** com uma autoridade certificadora própria, **restrita a `localhost` e `.test`**. Mesmo que alguém copie a chave, ela não serve para falsificar outros sites.

### ⚙️ Configuração pronta
- `httpd.conf`, `nginx.conf`, `php.ini`, `my.ini` e os demais arquivos são gerados com as portas e pastas escolhidas.
- **PHP já configurado:** principais extensões ativas, fuso de São Paulo e limites maiores de upload e memória. O `mail()` cai direto no **Mailpit**.
- **Suas edições são respeitadas:** arquivos alterados à mão não são sobrescritos.
- Os programas entram no **PATH do Windows**, e há um **Terminal** já configurado no menu.

### 📦 Migração fácil do Laragon e do XAMPP
- Detecta o **Laragon** (`C:\laragon`) e o **XAMPP** (`C:\xampp`), ou qualquer pasta que você escolher.
- Copia os sites (`www` / `htdocs`) **sem sobrescrever nada**.
- Importa os bancos do **MySQL** e do **MariaDB**. Com a mesma versão do MySQL, copia a pasta de dados inteira, mantendo usuários e senhas.
- **A instalação antiga só é lida, nunca alterada.**

### 💾 Backup automático
- Backup do MySQL, do PostgreSQL e do MongoDB em `.zip`, no intervalo que você escolher (de hora em hora até semanal).
- Mantém os últimos N backups e apaga os mais antigos.
- Botões **Fazer backup agora** e **Restaurar**.

## Instalação

1. Baixe o **`jjphpmy_x.y.z_x64-setup.exe`** em [**Releases**](https://github.com/josejuniorgyn/jjphpmy/releases/latest).
2. Execute o instalador (não pede administrador).
3. Abra o JJPHPMY e, na aba **Pacotes**, marque o que quer usar (ex.: PHP, MySQL e phpMyAdmin).
4. Clique em **Instalar selecionados** e depois em **Iniciar tudo**.
5. Acesse **http://localhost** e **http://localhost/phpmyadmin**.

> **Aviso do Windows (SmartScreen):** o instalador ainda não tem assinatura de código paga. Se aparecer "O Windows protegeu o computador", clique em **Mais informações → Executar assim mesmo**.

### Requisitos
- Windows 10 ou 11, 64 bits.
- Microsoft Edge WebView2 (já vem no Windows 11; no Windows 10, o instalador cuida disso).

## Onde ficam os arquivos

```
C:\jjphpmy\
├── bin\      programas instalados (várias versões lado a lado)
├── etc\      arquivos de configuração (httpd.conf, php.ini, my.ini...)
├── data\     dados dos bancos
├── www\      seus sites → cada pasta vira {nome}.test
├── logs\     logs de cada serviço
└── backup\   backups automáticos
```

Tudo numa pasta só: fácil de achar, de fazer backup e de remover.

## Portas padrão

| Serviço | Porta |
|---|---|
| Apache | 80 (HTTPS 443) |
| Nginx | 8080 (HTTPS 8443) |
| MySQL | 3306 |
| PostgreSQL | 5432 |
| MongoDB | 27017 |
| Redis | 6379 |
| Memcached | 11211 |
| Mailpit | 1025 (SMTP) e 8025 (painel web) |

Todas podem ser alteradas na aba **Serviços e Portas**.

## Contribua com o projeto ☕

O JJPHPMY é gratuito. Se ele ajuda no seu dia a dia, apoie pelo **PIX**: na aba **Sobre** do programa há QR Codes de R$ 20 a R$ 100.

Também é muito bem-vindo: ⭐ uma estrela no repositório, [sugestões e relatos de problemas](https://github.com/josejuniorgyn/jjphpmy/issues) e pull requests.

## Para desenvolvedores

Feito com **[Tauri 2](https://tauri.app)** (Rust + HTML/JS), por isso é tão leve.

```bash
npm install
npm run dev     # roda em modo desenvolvimento
npm test        # testes
npm run build   # gera o instalador
```

Arquitetura, decisões e roteiro completos: [`CLAUDE.md`](CLAUDE.md).

---

<div align="center">

Feito por **Jose Antonio de Melo Junior** · [josejuniorgyn@gmail.com](mailto:josejuniorgyn@gmail.com)

</div>
