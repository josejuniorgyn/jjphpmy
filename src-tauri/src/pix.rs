//! QR Code PIX estático para "Contribua com o projeto"
//! (padrão BR Code / EMV do Banco Central).

use qrcode::render::svg;
use qrcode::QrCode;
use serde::Serialize;

pub const PIX_KEY: &str = "josejuniorgyn@gmail.com";
/// O BR Code aceita até 25 caracteres, sem acentos.
const MERCHANT_NAME: &str = "JJPHPMY JOSE A M JUNIOR";
/// Até 15 caracteres, sem acentos.
const MERCHANT_CITY: &str = "GOIANIA";

/// Valores oferecidos na tela Sobre: R$ 20,00 a R$ 100,00.
pub fn is_allowed_amount(cents: u32) -> bool {
    (2000..=10000).contains(&cents) && cents % 1000 == 0
}

/// Campo EMV: id + tamanho com 2 dígitos + valor.
fn field(id: &str, value: &str) -> String {
    format!("{id}{:02}{value}", value.len())
}

/// CRC16-CCITT (polinômio 0x1021, início 0xFFFF), exigido no campo 63.
fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0xFFFFu16;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

/// Código "PIX copia e cola".
pub fn payload(amount_cents: u32) -> String {
    let account = field("00", "br.gov.bcb.pix") + &field("01", PIX_KEY);
    let name: String = MERCHANT_NAME.chars().take(25).collect();
    let amount = format!("{}.{:02}", amount_cents / 100, amount_cents % 100);

    let mut p = String::new();
    p += &field("00", "01");
    p += &field("26", &account);
    p += &field("52", "0000");
    p += &field("53", "986"); // BRL
    p += &field("54", &amount);
    p += &field("58", "BR");
    p += &field("59", &name);
    p += &field("60", MERCHANT_CITY);
    p += &field("62", &field("05", "***"));
    p += "6304";
    let crc = crc16(p.as_bytes());
    format!("{p}{crc:04X}")
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PixQr {
    pub payload: String,
    pub svg: String,
}

pub fn qr(amount_cents: u32) -> Result<PixQr, String> {
    if !is_allowed_amount(amount_cents) {
        return Err(format!("Valor não permitido: {amount_cents} centavos"));
    }
    let payload = payload(amount_cents);
    let code = QrCode::new(payload.as_bytes()).map_err(|e| format!("Falha ao gerar o QR Code: {e}"))?;
    let svg = code
        .render::<svg::Color>()
        .min_dimensions(240, 240)
        .dark_color(svg::Color("#000000"))
        .light_color(svg::Color("#ffffff"))
        .build();
    Ok(PixQr { payload, svg })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_reference_vector() {
        assert_eq!(crc16(b"123456789"), 0x29B1);
    }

    #[test]
    fn payload_has_expected_fields() {
        let p = payload(2000);
        assert!(p.starts_with("000201"));
        assert!(p.contains("0014br.gov.bcb.pix0123josejuniorgyn@gmail.com"));
        assert!(p.contains("540520.00"));
        assert!(p.contains("5923JJPHPMY JOSE A M JUNIOR6007GOIANIA"));
        let (body, crc) = p.split_at(p.len() - 4);
        assert_eq!(crc, format!("{:04X}", crc16(body.as_bytes())));
    }

    #[test]
    fn only_listed_amounts_are_allowed() {
        assert!(is_allowed_amount(2000) && is_allowed_amount(10000));
        assert!(!is_allowed_amount(1000) &&!is_allowed_amount(1500) && !is_allowed_amount(11000));
        assert!(qr(1500).is_err());
        assert!(qr(5000).unwrap().svg.contains("<svg"));
    }
}
