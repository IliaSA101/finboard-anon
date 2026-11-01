//! Двоичные части книги, которые раньше копировались как есть и не проверялись, а отчёт при этом писал
//! «реальных значений не найдено»: миниатюра первого листа, настройки принтера, модель данных Power Pivot,
//! запросы Power Query, внедрённые объекты, макросы, рисунки.
//! Каждый тест падал до исправления, кроме b04 — он сторожит прежнюю строку проверки у обычной книги.

mod common;

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use common::*;
use finboard_anon::anon::{run, Job, Report};
use finboard_anon::profile::{Kind, Profile, Rule, Settings};
use finboard_anon::xlsx::Workbook;

const OLD_CHECK: &str = "Проверка обезличенного файла: реальных значений из размеченных колонок в нём не найдено.";
const HONEST_CHECK: &str =
    "Проверка обезличенного файла: в листах и XML-частях реальных значений из размеченных колонок не найдено; двоичные части (см. «Внимание» выше) не проверялись.";

fn prof() -> Profile {
    Profile { rules: vec![Rule::new("Л", 0, 2, None, Kind::FullName)], settings: Settings { salt: "bin".into(), ..Settings::default() } }
}

fn go(input: &Path, output: &Path, p: &Profile) -> (Report, Vec<String>) {
    let job = Job { input: input.into(), output: output.into(), key_in: None, key_out: None, profile: p.clone() };
    let rep = run(&job, &mut |_| {}).unwrap();
    let lines = rep.summary(p);
    (rep, lines)
}

fn people_book() -> Book {
    let mut b = Book { author: "Автор".into(), ..Default::default() };
    b.sheet("Л").extend([(1, 0, s("ФИО")), (2, 0, s("Иванов Иван Иванович")), (3, 0, s("Петрова Мария Сергеевна"))]);
    b
}

fn raw_parts(path: &Path) -> HashMap<String, Vec<u8>> {
    let mut z = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    (0..z.len())
        .map(|i| {
            let mut f = z.by_index(i).unwrap();
            let mut d = Vec::new();
            f.read_to_end(&mut d).unwrap();
            (f.name().to_string(), d)
        })
        .collect()
}

fn text(parts: &HashMap<String, Vec<u8>>, name: &str) -> String {
    String::from_utf8(parts[name].clone()).unwrap()
}

fn utf16le(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// Значения атрибута по всем тегам: `attr_values(xml, "Target")`.
fn attr_values(xml: &str, attr: &str) -> Vec<String> {
    let key = format!(" {attr}=\"");
    xml.match_indices(&key).map(|(p, _)| xml[p + key.len()..].split('"').next().unwrap().to_string()).collect()
}

/// Книга, как её сохраняет Excel с включённым «Сохранять эскизы»: картинка первого листа в docProps,
/// связь на неё в _rels/.rels и её тип в [Content_Types].xml.
fn with_thumbnail(path: &Path) {
    let edit = |name: &str, t: &str| match name {
        "_rels/.rels" => Some(t.replace(
            "</Relationships>",
            r#"<Relationship Id="rId9" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail" Target="docProps/thumbnail.emf"/></Relationships>"#,
        )),
        "[Content_Types].xml" => Some(t.replace(
            "</Types>",
            r#"<Default Extension="emf" ContentType="image/x-emf"/><Override PartName="/docProps/thumbnail.emf" ContentType="image/x-emf"/></Types>"#,
        )),
        _ => None,
    };
    // Внутри картинки — сам текст ячеек (в EMF он и правда лежит текстом UTF-16).
    let mut emf = b"\x01\x00\x00\x00EMF-thumbnail ".to_vec();
    emf.extend(utf16le("Иванов Иван Иванович"));
    repack(path, &edit, &[("docProps/thumbnail.emf", emf)]);
}

#[test]
fn b01_thumbnail_dropped_with_its_references() {
    let d = tmp("b01");
    let (input, output) = (d.join("b01.xlsx"), d.join("b01_anon.xlsx"));
    write_xlsx(&input, &people_book());
    with_thumbnail(&input);
    let (rep, lines) = go(&input, &output, &prof());
    let parts = raw_parts(&output);
    assert!(!parts.keys().any(|n| n.to_lowercase().contains("thumbnail")), "миниатюра осталась: {:?}", parts.keys());
    let rels = text(&parts, "_rels/.rels");
    let ct = text(&parts, "[Content_Types].xml");
    assert!(!rels.contains("thumbnail") && !ct.contains("thumbnail"), "ссылки на миниатюру остались:\n{rels}\n{ct}");
    // Книга цела: каждая связь и каждый тип указывают на существующую часть, остальные ссылки на месте.
    for t in attr_values(&rels, "Target") {
        assert!(parts.contains_key(t.trim_start_matches('/')), "связь на несуществующую часть {t}");
    }
    for p in attr_values(&ct, "PartName") {
        assert!(parts.contains_key(p.trim_start_matches('/')), "тип для несуществующей части {p}");
    }
    assert!(rels.contains("xl/workbook.xml") && rels.contains("docProps/core.xml"), "{rels}");
    assert!(ct.contains("/xl/workbook.xml") && ct.contains("/xl/worksheets/sheet1.xml"), "{ct}");
    Workbook::open(&output).unwrap();
    // Миниатюры больше нет — проверять нечего: прежняя строка проверки.
    assert!(rep.unchecked.is_empty(), "{:?}", rep.unchecked);
    assert!(lines.iter().any(|l| l == OLD_CHECK), "{lines:#?}");
}

/// Настройки принтера Windows (DEVMODE): имя принтера с сервером, ориентация, данные драйвера.
fn devmode(name: &str, private: &str) -> Vec<u8> {
    let mut d = vec![0u8; 220];
    let n = utf16le(name);
    d[..n.len()].copy_from_slice(&n);
    d[64..66].copy_from_slice(&0x0401u16.to_le_bytes());
    d[68..70].copy_from_slice(&220u16.to_le_bytes());
    let extra = utf16le(private);
    d[70..72].copy_from_slice(&(extra.len() as u16).to_le_bytes());
    d[72..76].copy_from_slice(&0x0000_0001u32.to_le_bytes());
    d[76..78].copy_from_slice(&2u16.to_le_bytes()); // альбомная
    let form = utf16le("A4");
    d[102..102 + form.len()].copy_from_slice(&form);
    d.extend(extra);
    d
}

fn with_printer(path: &Path, bin: Vec<u8>) {
    let edit = |name: &str, t: &str| match name {
        "[Content_Types].xml" => Some(t.replace(
            "</Types>",
            r#"<Default Extension="bin" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.printerSettings"/></Types>"#,
        )),
        "xl/worksheets/sheet1.xml" => Some(t.replace(
            "</worksheet>",
            r#"<pageMargins left="0.7" right="0.7" top="0.75" bottom="0.75" header="0.3" footer="0.3"/><pageSetup paperSize="9" orientation="landscape" r:id="rId1" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"/></worksheet>"#,
        )),
        _ => None,
    };
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/printerSettings" Target="../printerSettings/printerSettings1.bin"/></Relationships>"#;
    repack(
        path,
        &edit,
        &[("xl/printerSettings/printerSettings1.bin", bin), ("xl/worksheets/_rels/sheet1.xml.rels", rels.as_bytes().to_vec())],
    );
}

#[test]
fn b02_printer_name_and_driver_data_cleared() {
    let d = tmp("b02");
    let (input, output) = (d.join("b02.xlsx"), d.join("b02_anon.xlsx"));
    write_xlsx(&input, &people_book());
    with_printer(&input, devmode(r"\\BUH-SRV\HP 2 этаж", "Владелец задания: Петров П.П."));
    let (rep, lines) = go(&input, &output, &prof());
    let parts = raw_parts(&output);
    let bin = &parts["xl/printerSettings/printerSettings1.bin"];
    let hay = String::from_utf16_lossy(&bin.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>());
    assert!(!hay.contains("BUH-SRV") && !hay.contains("Петров"), "имя принтера или данные драйвера остались: {hay:?}");
    // Настройки печати целы: размер структуры, ориентация, бумага.
    assert_eq!(bin.len(), 220);
    assert_eq!(u16::from_le_bytes([bin[68], bin[69]]), 220);
    assert_eq!(u16::from_le_bytes([bin[70], bin[71]]), 0);
    assert_eq!(u16::from_le_bytes([bin[76], bin[77]]), 2);
    assert!(hay.contains("A4"));
    // Связь листа на настройки на месте — иначе Excel «восстанавливает» книгу.
    assert!(text(&parts, "xl/worksheets/_rels/sheet1.xml.rels").contains("printerSettings1.bin"));
    assert!(rep.unchecked.is_empty(), "{:?}", rep.unchecked);
    assert!(lines.iter().any(|l| l == OLD_CHECK), "{lines:#?}");
}

#[test]
fn b02b_printer_settings_unknown_format_warned() {
    let d = tmp("b02b");
    let (input, output) = (d.join("b02b.xlsx"), d.join("b02b_anon.xlsx"));
    write_xlsx(&input, &people_book());
    // Не DEVMODE Windows (так пишет, например, Excel для Mac) — не трогаем, но и не молчим.
    with_printer(&input, b"bplist00 \\BUH-SRV\\HP 2".to_vec());
    let (rep, lines) = go(&input, &output, &prof());
    assert!(lines.iter().any(|l| l.starts_with("Внимание: настройки принтера в незнакомом формате")), "{lines:#?}");
    assert_eq!(rep.unchecked, vec!["xl/printerSettings/printerSettings1.bin".to_string()]);
    assert!(lines.iter().any(|l| l == HONEST_CHECK), "{lines:#?}");
}

#[test]
fn b03_unprocessed_binary_parts_warned_and_check_line_honest() {
    let d = tmp("b03");
    let (input, output) = (d.join("b03.xlsx"), d.join("b03_anon.xlsx"));
    write_xlsx(&input, &people_book());
    // Запрос Power Query: customXml в UTF-16, внутри — base64 архива с текстом запросов.
    let mut mashup = vec![0xFF, 0xFE];
    mashup.extend(utf16le(
        r#"<?xml version="1.0" encoding="utf-16"?><DataMashup xmlns="http://schemas.microsoft.com/DataMashup">AAAAAEUDAABQSwMEFAACAAgA0J1hW</DataMashup>"#,
    ));
    let real = "Иванов Иван Иванович".as_bytes().to_vec();
    repack(
        &input,
        &|_, _| None,
        &[
            ("xl/model/item.data", real.clone()),
            ("xl/vbaProject.bin", real.clone()),
            ("xl/embeddings/Microsoft_Excel_Worksheet.xlsx", real.clone()),
            ("xl/media/image1.png", real.clone()),
            ("xl/activeX/activeX1.bin", real.clone()),
            ("customXml/item1.xml", mashup),
        ],
    );
    let (rep, lines) = go(&input, &output, &prof());
    for start in [
        "Внимание: в книге есть модель данных Power Pivot (xl/model/item.data)",
        "Внимание: в книге есть запросы Power Query (customXml/item1.xml",
        "Внимание: в книге есть внедрённые объекты",
        "Внимание: в книге есть макросы (xl/vbaProject.bin)",
        "Внимание: в книге есть рисунки (1 шт.: xl/media/image1.png)",
        "Внимание: в книге есть другие двоичные части (xl/activeX/activeX1.bin)",
    ] {
        assert_eq!(lines.iter().filter(|l| l.starts_with(start)).count(), 1, "нет «{start}»: {lines:#?}");
    }
    // Каждое предупреждение говорит, что делать.
    for l in lines.iter().filter(|l| l.starts_with("Внимание: в книге есть") && !l.contains("другие двоичные")) {
        assert!(l.contains("Перед пересылкой") || l.contains("перед пересылкой"), "без совета: {l}");
    }
    assert_eq!(rep.unchecked.len(), 6, "{:?}", rep.unchecked);
    // Прежнее «не найдено» обещало то, чего проверка не делала.
    assert!(!lines.iter().any(|l| l == OLD_CHECK), "{lines:#?}");
    assert!(lines.iter().any(|l| l == HONEST_CHECK), "{lines:#?}");
    // Части сохранены: без них книга с моделью или макросами ломается.
    let parts = raw_parts(&output);
    for n in ["xl/model/item.data", "xl/vbaProject.bin", "xl/embeddings/Microsoft_Excel_Worksheet.xlsx", "xl/media/image1.png", "customXml/item1.xml"] {
        assert!(parts.contains_key(n), "часть {n} пропала");
    }
}

#[test]
fn b04_plain_workbook_keeps_old_check_line() {
    let d = tmp("b04");
    let (input, output) = (d.join("b04.xlsx"), d.join("b04_anon.xlsx"));
    write_xlsx(&input, &people_book());
    let (rep, lines) = go(&input, &output, &prof());
    assert!(rep.unchecked.is_empty());
    assert!(lines.iter().any(|l| l == OLD_CHECK), "{lines:#?}");
    assert!(!lines.iter().any(|l| l.contains("двоичн")), "{lines:#?}");
}

#[test]
fn b05_leaks_and_binary_parts_both_reported() {
    let d = tmp("b05");
    let (input, output) = (d.join("b05.xlsx"), d.join("b05_anon.xlsx"));
    let mut b = people_book();
    // Реальное ФИО внутри фразы при выключенном поиске внутри текста — его покажет проверка.
    b.sheets[0].1.push((1, 1, s("Связаться: Иванов Иван Иванович, доб. 12")));
    write_xlsx(&input, &b);
    repack(&input, &|_, _| None, &[("xl/model/item.data", b"x".to_vec())]);
    let p = Profile { settings: Settings { inner_text: false, ..prof().settings }, ..prof() };
    let (rep, lines) = go(&input, &output, &p);
    assert!(!rep.leaks.is_empty(), "{lines:#?}");
    assert!(lines.iter().any(|l| l == "  Двоичные части (см. «Внимание» выше) не проверялись."), "{lines:#?}");
}
