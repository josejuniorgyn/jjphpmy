<?php
/*
 * Gerado pelo JJPHPMY. Se você editar este arquivo, o JJPHPMY deixa de
 * sobrescrevê-lo (use "Recriar arquivos de configuração" para voltar ao padrão).
 *
 * Como no Laragon: entra direto como root, sem senha (só em localhost).
 */
$cfg['blowfish_secret'] = '{{secret}}';

$i = 1;
$cfg['Servers'][$i]['host'] = '127.0.0.1';
$cfg['Servers'][$i]['port'] = '{{mysql_port}}';
$cfg['Servers'][$i]['auth_type'] = 'config';
$cfg['Servers'][$i]['user'] = 'root';
$cfg['Servers'][$i]['password'] = '';
$cfg['Servers'][$i]['AllowNoPassword'] = true;

$cfg['TempDir'] = '{{tmp}}/phpmyadmin';
$cfg['DefaultLang'] = 'pt_BR';
$cfg['VersionCheck'] = false;
