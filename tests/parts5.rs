//! Части книги вне ячеек (0.5.0): подключения к базам данных и пути к связанным книгам, битый customXml,
//! «архивная бомба» (часть распаковывается в гигабайты), текст в CDATA. Каждый тест падал до исправления.
//! Результаты — «*_anon.xlsx»: их открывает в Excel `tools/excel_open.ps1 -Root <временная папка тестов>`.

mod common;

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use common::*;
use finboard_anon::anon::{run, Job, Report};
use finboard_anon::profile::{Kind, Profile, Rule, Settings};

const MAIN_NS: &str = r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main""#;
const R_NS: &str = r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;
const RELS_NS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#;
const NAME: &str = "Иванов Иван Иванович";

fn profile() -> Profile {
    Profile { rules: vec![Rule::new("Л", 0, 2, None, Kind::FullName)], settings: Settings { salt: "parts5".into(), ..Settings::default() } }
}

fn job(input: &Path, output: &Path) -> Job {
    Job { input: input.into(), output: output.into(), key_in: None, key_out: None, profile: profile() }
}

fn people_book() -> Book {
    let mut b = Book { author: "Автор".into(), ..Default::default() };
    b.sheet("Л").extend([(1, 0, s("ФИО")), (2, 0, s(NAME)), (3, 0, s("Смирнова Мария Сергеевна"))]);
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

fn text_parts(path: &Path) -> HashMap<String, String> {
    raw_parts(path).into_iter().filter_map(|(n, d)| String::from_utf8(d).ok().map(|t| (n, t))).collect()
}

fn summary(r: &Report) -> String {
    r.summary(&profile()).join("\n")
}

// ---------------------------------------------------------------- подключения и связанные книги

/// Книга с подключением ODBC к базе 1С (сервер, логин, пароль, запрос, файл .odc на сетевом диске) и формулой
/// со ссылкой на другую книгу `\\srv\HR\Петров.xlsx`, как их сохраняет Excel.
fn connected_book(path: &Path) {
    let mut b = people_book();
    b.sheets[0].1.push((2, 1, Cell::F("[1]Лист1!A1".into(), 5.0)));
    b.extra.push((
        "xl/connections.xml".into(),
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><connections {MAIN_NS}><connection id="1" odcFile="\\srv\HR\Петров_подключение.odc" name="Зарплата" type="1" refreshedVersion="6" savePassword="1" refreshOnLoad="1" background="1" saveData="1"><dbPr connection="DSN=srv-1c;UID=ivanov;PWD=Qwerty1;Data Source=srv-1c; User ID=ivanov; Initial Catalog=Зарплата;" command="SELECT * FROM Сотрудники WHERE ФИО='Петров'"/></connection></connections>"#
        ),
    ));
    b.extra.push((
        "xl/externalLinks/externalLink1.xml".into(),
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><externalLink {MAIN_NS} {R_NS}><externalBook r:id="rId1"><sheetNames><sheetName val="Лист1"/></sheetNames><sheetDataSet><sheetData sheetId="0"><row r="1"><cell r="A1"><v>5</v></cell></row></sheetData></sheetDataSet></externalBook></externalLink>"#
        ),
    ));
    b.extra.push((
        "xl/externalLinks/_rels/externalLink1.xml.rels".into(),
        format!(r#"{RELS_NS}<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/externalLinkPath" Target="file:///\\srv\HR\Петров.xlsx" TargetMode="External"/></Relationships>"#),
    ));
    write_xlsx(path, &b);
    repack(
        path,
        &|name, t| match name {
            "xl/workbook.xml" => Some(t.replace("</sheets>", r#"</sheets><externalReferences><externalReference r:id="rId90"/></externalReferences>"#)),
            "xl/_rels/workbook.xml.rels" => Some(t.replace(
                "</Relationships>",
                r#"<Relationship Id="rId90" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/externalLink" Target="externalLinks/externalLink1.xml"/><Relationship Id="rId91" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/connections" Target="connections.xml"/></Relationships>"#,
            )),
            "[Content_Types].xml" => Some(t.replace(
                "</Types>",
                r#"<Override PartName="/xl/externalLinks/externalLink1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.externalLink+xml"/><Override PartName="/xl/connections.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.connections+xml"/></Types>"#,
            )),
            _ => None,
        },
        &[],
    );
}

#[test]
fn s01_connections_and_external_links_are_cleaned() {
    let d = tmp("parts5_conn");
    let (input, output) = (d.join("conn.xlsx"), d.join("conn_anon.xlsx"));
    connected_book(&input);
    // Книга собрана как надо: всё это в исходнике есть.
    let src = text_parts(&input);
    assert!(src["xl/connections.xml"].contains("ivanov") && src["xl/externalLinks/_rels/externalLink1.xml.rels"].contains("Петров.xlsx"));

    let report = run(&job(&input, &output), &mut |_| {}).unwrap();
    let p = text_parts(&output);
    for (name, xml) in &p {
        for real in ["srv", "ivanov", "Петров", "Qwerty1", "Сотрудники"] {
            assert!(!xml.contains(real), "«{real}» осталось в {name}: {xml}");
        }
    }
    let conn = &p["xl/connections.xml"];
    assert!(conn.contains(r#"odcFile="connection.odc""#), "{conn}");
    assert!(conn.contains(r#"command="""#), "{conn}");
    assert!(conn.contains(r#"refreshOnLoad="0""#), "{conn}");
    assert!(conn.contains("Data Source=server") && conn.contains("User ID=user") && conn.contains("PWD=;"), "{conn}");
    let rels = &p["xl/externalLinks/_rels/externalLink1.xml.rels"];
    assert!(rels.contains(r#"Target="external.xlsx""#), "{rels}");
    // Формула со ссылкой на другую книгу и её значение — на месте.
    assert!(p["xl/worksheets/sheet1.xml"].contains("[1]Лист1!A1"), "{}", p["xl/worksheets/sheet1.xml"]);
    let s = summary(&report);
    assert!(s.contains("Внимание: подключения к базам данных очищены (сервер, логин, запрос) — обновить данные из базы в результате нельзя"), "{s}");
    assert!(s.contains("Внимание: пути к связанным файлам"), "{s}");
}

/// Без очистки служебных полей подключения не трогаются — как и прочие служебные поля.
#[test]
fn s02_setting_off_keeps_connections() {
    let d = tmp("parts5_conn_off");
    let (input, output) = (d.join("conn.xlsx"), d.join("conn_off_anon.xlsx"));
    connected_book(&input);
    let mut j = job(&input, &output);
    j.profile.settings.clean_metadata = false;
    let report = run(&j, &mut |_| {}).unwrap();
    let p = text_parts(&output);
    assert!(p["xl/connections.xml"].contains("ivanov"));
    assert!(!summary(&report).contains("подключения к базам данных очищены"));
}

// ---------------------------------------------------------------- битый UTF-16 в customXml

fn utf16le(s: &str) -> Vec<u8> {
    let mut v = vec![0xFF, 0xFE];
    for u in s.encode_utf16() {
        v.extend_from_slice(&u.to_le_bytes());
    }
    v
}

/// Свойства SharePoint в customXml (UTF-16) с непарным суррогатом: раньше часть копировалась как есть —
/// имена людей уходили в результат. Теперь она удаляется вместе со связями, ссылкой из книги и типом.
#[test]
fn s03_broken_utf16_custom_xml_is_dropped() {
    let d = tmp("parts5_utf16");
    let (input, output) = (d.join("utf16.xlsx"), d.join("utf16_anon.xlsx"));
    write_xlsx(&input, &people_book());
    let mut broken = utf16le(&format!(r#"<?xml version="1.0" encoding="UTF-16"?><root><DisplayName>{NAME}</DisplayName>"#));
    broken.extend_from_slice(&0xD800u16.to_le_bytes()); // непарный суррогат
    broken.extend_from_slice(&utf16le("</root>")[2..]);
    let props = r#"<?xml version="1.0" encoding="UTF-8" standalone="no"?><ds:datastoreItem ds:itemID="{11111111-2222-3333-4444-555555555555}" xmlns:ds="http://schemas.openxmlformats.org/officeDocument/2006/customXml"><ds:schemaRefs/></ds:datastoreItem>"#;
    let item_rels = format!(r#"{RELS_NS}<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/customXmlProps" Target="itemProps1.xml"/></Relationships>"#);
    repack(
        &input,
        &|name, t| match name {
            "xl/_rels/workbook.xml.rels" => Some(t.replace(
                "</Relationships>",
                r#"<Relationship Id="rId95" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/customXml" Target="../customXml/item1.xml"/></Relationships>"#,
            )),
            "[Content_Types].xml" => Some(t.replace(
                "</Types>",
                r#"<Override PartName="/customXml/itemProps1.xml" ContentType="application/vnd.openxmlformats-officedocument.customXmlProperties+xml"/></Types>"#,
            )),
            _ => None,
        },
        &[("customXml/item1.xml", broken), ("customXml/_rels/item1.xml.rels", item_rels.into_bytes()), ("customXml/itemProps1.xml", props.as_bytes().to_vec())],
    );

    let report = run(&job(&input, &output), &mut |_| {}).unwrap();
    let parts = raw_parts(&output);
    assert!(!parts.contains_key("customXml/item1.xml"), "битая часть осталась");
    assert!(!parts.contains_key("customXml/_rels/item1.xml.rels"), "связи битой части остались");
    let wbr = String::from_utf8(parts["xl/_rels/workbook.xml.rels"].clone()).unwrap();
    assert!(!wbr.contains("customXml/item1.xml"), "ссылка на удалённую часть осталась: {wbr}");
    assert!(wbr.contains("worksheets/sheet1.xml"), "{wbr}");
    let s = summary(&report);
    assert!(s.contains("Внимание: часть customXml/item1.xml повреждена (кодировка) и удалена из результата"), "{s}");
    // Удалённую часть проверять не нужно — итог проверки обычный.
    assert!(report.unchecked.is_empty(), "{:?}", report.unchecked);
    let a2 = text(&read_book(&output), "Л", "A2");
    assert!(!a2.is_empty() && a2 != NAME, "{a2}");
}

// ---------------------------------------------------------------- «архивная бомба»

/// Часть, которая распаковывается больше предела: понятная ошибка, результат не записан. Предел в тесте —
/// 1 МБ вместо 2 и 8 ГБ, чтобы не распаковывать гигабайты; сжатая часть — несколько килобайт.
fn bomb(case: &str, edit: &dyn Fn(&Path)) {
    let d = tmp(&format!("parts5_bomb_{case}"));
    let (input, output) = (d.join("bomb.xlsx"), d.join("bomb_anon.xlsx"));
    edit(&input);
    finboard_anon::xlsx::set_part_limits(Some((1 << 20, 1 << 20)));
    let r = run(&job(&input, &output), &mut |_| {});
    finboard_anon::xlsx::set_part_limits(None);
    let e = format!("{:#}", r.expect_err("книга с бомбой обработана"));
    assert!(e.contains("слишком большая после распаковки (больше 1 МБ) — файл повреждён или это не обычная книга Excel"), "{e}");
    assert!(!e.contains("I/O error"), "сообщение с технической обёрткой: {e}");
    assert!(!output.exists(), "результат записан");
    let left: Vec<_> = std::fs::read_dir(&d).unwrap().flatten().map(|x| x.file_name()).collect();
    assert_eq!(left.len(), 1, "в папке что-то осталось: {left:?}");
}

#[test]
fn s04_bomb_in_other_part() {
    bomb("part", &|p| {
        write_xlsx(p, &people_book());
        let big = format!(r#"<?xml version="1.0" encoding="UTF-8"?><x>{}</x>"#, "a".repeat(2 << 20));
        repack(p, &|_, _| None, &[("xl/drawings/drawing9.xml", big.into_bytes())]);
    });
}

#[test]
fn s05_bomb_in_sheet() {
    bomb("sheet", &|p| {
        let mut b = people_book();
        b.sheet_tail.insert(0, " ".repeat(2 << 20));
        write_xlsx(p, &b);
    });
}

#[test]
fn s06_bomb_in_shared_strings() {
    bomb("sst", &|p| {
        write_xlsx(p, &people_book());
        repack(p, &|n, t| (n == "xl/sharedStrings.xml").then(|| t.replace("</sst>", &format!("{}</sst>", " ".repeat(2 << 20)))), &[]);
    });
}

// ---------------------------------------------------------------- CDATA

/// Текст примечания в CDATA раньше не заменялся: переписывание прочих частей не видело таких узлов.
#[test]
fn s07_cdata_in_comment_is_replaced() {
    let d = tmp("parts5_cdata");
    let (input, output) = (d.join("cdata.xlsx"), d.join("cdata_anon.xlsx"));
    let mut b = people_book();
    b.extra.push((
        "xl/comments1.xml".into(),
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><comments {MAIN_NS}><authors><author>Автор</author></authors><commentList><comment ref="B2" authorId="0"><text><t><![CDATA[{NAME}]]></t></text></comment><comment ref="B3" authorId="0"><text><r><t><![CDATA[{NAME}]]></t></r></text></comment></commentList></comments>"#
        ),
    ));
    write_xlsx(&input, &b);
    let report = run(&job(&input, &output), &mut |_| {}).unwrap();
    let c = &text_parts(&output)["xl/comments1.xml"];
    assert!(!c.contains(NAME), "{c}");
    let fake = text(&read_book(&output), "Л", "A2");
    assert_eq!(c.matches(fake.as_str()).count(), 2, "{c}");
    assert!(report.leaks.is_empty(), "{:?}", report.leaks);
}
