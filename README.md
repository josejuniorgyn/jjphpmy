# jjphpmy

Ambiente de desenvolvimento local para Windows: Apache, Nginx, PHP, MySQL, PostgreSQL, MongoDB, phpMyAdmin, Node.js, Python, Memcached, Redis e Mailpit. Só instala o que você selecionar.

## 1. Pré-requisitos (uma vez só)

1. **Rust**: baixe e rode o `rustup-init.exe` de https://rustup.rs e aceite as opções padrão.
   - Se ele pedir o **Visual Studio C++ Build Tools**, instale com a carga de trabalho "Desenvolvimento para desktop com C++".
2. **Node.js LTS**: https://nodejs.org (o Node que veio com o Laragon também serve).
3. **WebView2**: já vem no Windows 11.
4. **VS Code**, com as extensões sugeridas ao abrir a pasta (rust-analyzer e Tauri).

Confira no terminal:

```bash
rustc --version
```

```bash
node --version
```

## 2. Rodar

Abra a pasta `jjphpmy` no VS Code e, no terminal (Ctrl+'):

```bash
npm install
```

```bash
npm run icon
```

```bash
npm run dev
```

A primeira compilação demora alguns minutos. O ícone "jj" aparece na bandeja, perto do relógio.

Testes:

```bash
npm test
```

Gerar o instalador (`src-tauri/target/release/bundle/nsis/jjphpmy_0.1.0_x64-setup.exe`):

```bash
npm run build
```

## 3. Instalador atual

Se preferir clique em Releases e baixe o instalador, ou clique no link abaixo:

[jjphpmy_0.2.0_x64-setup.exe](https://github.com/josejuniorgyn/jjphpmy/releases/download/v0.2.0/jjphpmy_0.2.0_x64-setup.exe)
