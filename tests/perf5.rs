//! 0.5.0: большие и повреждённые книги. Память не растёт на миллионах событий XML и не занимается
//! по размеру из заголовка архива, проверка результата линейна и не теряет значение на стыке кусков,
//! битая часть customXml не останавливает обработку.
//!
//! Долгие замеры на книге в сотни мегабайт — `#[ignore]`:
//! `cargo test --release --test perf5 -- --ignored --nocapture` (PERF5_DIR — куда положить книгу и профиль).

mod common;

use std::io::Write;
use std::path::Path;

use common::*;
use finboard_anon::anon::{run, scan_leaks, Job};
use finboard_anon::profile::{Kind, Profile, Rule, Settings};
use finboard_anon::xlsx::Workbook;

const NS: &str = r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main""#;
const NAME: &str = "Иванов Иван Иванович";
const MB: usize = 1 << 20;

// ---------------------------------------------------------------- zip вручную

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// Архив без сжатия, собранный побайтно. Третье в записи — размер, который заявляет центральный каталог
/// (None — настоящий); больше 4 ГБ — через поле zip64, как в настоящих больших архивах.
fn raw_zip(path: &Path, parts: &[(&str, Vec<u8>, Option<u64>)]) {
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    let le16 = |v: &mut Vec<u8>, x: u16| v.extend_from_slice(&x.to_le_bytes());
    let le32 = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
    for (name, data, declared) in parts {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let n = data.len() as u32;
        // Локальный заголовок — честный.
        le32(&mut out, 0x0403_4b50);
        le16(&mut out, 20);
        le16(&mut out, 0x0800); // имена в UTF-8
        le16(&mut out, 0); // без сжатия
        le16(&mut out, 0);
        le16(&mut out, 0x21);
        le32(&mut out, crc);
        le32(&mut out, n);
        le32(&mut out, n);
        le16(&mut out, name.len() as u16);
        le16(&mut out, 0);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        // Центральный каталог — с заявленным размером.
        let size = declared.unwrap_or(n as u64);
        let zip64 = size >= 0xFFFF_FFFF;
        le32(&mut central, 0x0201_4b50);
        le16(&mut central, 45);
        le16(&mut central, if zip64 { 45 } else { 20 });
        le16(&mut central, 0x0800);
        le16(&mut central, 0);
        le16(&mut central, 0);
        le16(&mut central, 0x21);
        le32(&mut central, crc);
        le32(&mut central, n);
        le32(&mut central, if zip64 { 0xFFFF_FFFF } else { size as u32 });
        le16(&mut central, name.len() as u16);
        le16(&mut central, if zip64 { 12 } else { 0 });
        le16(&mut central, 0);
        le16(&mut central, 0);
        le16(&mut central, 0);
        le32(&mut central, 0);
        le32(&mut central, offset);
        central.extend_from_slice(name.as_bytes());
        if zip64 {
            le16(&mut central, 0x0001);
            le16(&mut central, 8);
            central.extend_from_slice(&size.to_le_bytes());
        }
    }
    let (cd_off, cd_len) = (out.len() as u32, central.len() as u32);
    out.extend_from_slice(&central);
    le32(&mut out, 0x0605_4b50);
    le16(&mut out, 0);
    le16(&mut out, 0);
    le16(&mut out, parts.len() as u16);
    le16(&mut out, parts.len() as u16);
    le32(&mut out, cd_len);
    le32(&mut out, cd_off);
    le16(&mut out, 0);
    std::fs::write(path, out).unwrap();
}

/// Все части готовой книги: (имя, содержимое).
fn unpack(path: &Path) -> Vec<(String, Vec<u8>)> {
    use std::io::Read;
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

fn job(input: &Path, output: &Path, rules: Vec<Rule>) -> Job {
    Job {
        input: input.to_path_buf(),
        output: output.to_path_buf(),
        key_in: None,
        key_out: None,
        profile: Profile { rules, settings: Settings { salt: "perf5".into(), ..Settings::default() } },
    }
}

fn people_book() -> Book {
    let mut b = Book::default();
    let sh = b.sheet("Л");
    sh.push((1, 0, s("ФИО")));
    sh.push((2, 0, s(NAME)));
    for (i, n) in NAMES.iter().enumerate() {
        sh.push((3 + i as u32, 0, s(n)));
    }
    b
}

// ---------------------------------------------------------------- проверка результата кусками

/// Часть, в которой значение `item(NAME)` пересекает каждую границу мегабайта (1..=n): поиск идёт блоками
/// по мегабайту и режет часть на куски от 16 МБ. Промежутки — `filler(k)` ровно из k байт.
fn straddling(head: &str, tail: &str, item: &dyn Fn(&str) -> String, filler: &dyn Fn(usize) -> String, n: usize) -> Vec<u8> {
    let mut s = String::from(head);
    let it = item(NAME);
    // Начало имени (в примечании имя разрезано — начало первого фрагмента).
    let off = it.find("Иванов").unwrap();
    for k in 1..=n {
        let start = k * MB - NAME.len() / 2 - off;
        let gap = start - s.len();
        s.push_str(&filler(gap));
        assert_eq!(s.len(), start);
        s.push_str(&it);
        assert!(s.len() > k * MB, "значение не пересекает границу {k} МБ");
    }
    s.push_str(tail);
    s.into_bytes()
}

/// Значение на стыке блоков чтения и кусков проверки находится — ровно один раз на каждое вхождение:
/// в общих строках (кусок режется по </si>), в записях сводной без абзацев (по любому закрывающему тегу)
/// и в примечании, где имя разрезано на фрагменты форматирования (склеивается только целый абзац).
#[test]
fn leak_on_chunk_boundary_found_exactly_once() {
    let d = tmp("p5_boundary");
    let path = d.join("big.xlsx");
    let n = 18; // больше 16 МБ — часть проверяется кусками
    let sst = straddling(
        &format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><sst {NS}>"#),
        "</sst>",
        &|v| format!("<si><t>{v}</t></si>"),
        &|k| format!("<si><t>{}</t></si>", "x".repeat(k - 16)),
        n,
    );
    let records = straddling(
        &format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><pivotCacheRecords {NS}>"#),
        "</pivotCacheRecords>",
        &|v| format!(r#"<r><n v="1"/><s v="{v}"/></r>"#),
        &|k| format!(r#"<r><s v="{}"/></r>"#, "y".repeat(k - 16)),
        n,
    );
    // В примечании имя целиком не встречается: «Иванов Иван» и « Иванович» — разные фрагменты.
    let (a, b) = NAME.split_at("Иванов Иван".len());
    let comments = straddling(
        &format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><comments {NS}><commentList>"#),
        "</commentList></comments>",
        &|_| format!(r#"<comment ref="A1"><text><r><t>{a}</t></r><r><t xml:space="preserve">{b}</t></r></text></comment>"#),
        &|k| format!("<text><t>{}</t></text>", "z".repeat(k - 20)),
        n,
    );
    raw_zip(
        &path,
        &[
            ("xl/sharedStrings.xml", sst, None),
            ("xl/pivotCache/pivotCacheRecords1.xml", records, None),
            ("xl/comments1.xml", comments, None),
        ],
    );
    let leaks = scan_leaks(&path, &[NAME.to_string()], &[]).unwrap();
    for part in ["xl/sharedStrings.xml", "xl/pivotCache/pivotCacheRecords1.xml", "xl/comments1.xml"] {
        let l = leaks.iter().find(|l| l.part == part).unwrap_or_else(|| panic!("{part}: утечки не найдены: {leaks:?}"));
        assert_eq!(l.count, n, "{part}: каждое вхождение — ровно один раз");
    }
}

/// Огромный текстовый узел без единого закрывающего тега: режется с перекрытием, вхождения на стыках
/// не теряются и не считаются дважды. Часть больше 64 МБ — только по запросу.
#[test]
#[ignore]
fn leak_in_giant_text_node_found_exactly_once() {
    let d = tmp("p5_giant");
    let path = d.join("giant.xlsx");
    let n = 80;
    let node = straddling(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><x><t>"#,
        "</t></x>",
        &|v| format!(" {v} "),
        &|k| "w".repeat(k),
        n,
    );
    raw_zip(&path, &[("xl/sharedStrings.xml", node, None)]);
    let leaks = scan_leaks(&path, &[NAME.to_string()], &[]).unwrap();
    assert_eq!(leaks.len(), 1, "{leaks:?}");
    assert_eq!(leaks[0].count, n);
}

// ---------------------------------------------------------------- заголовок архива с огромным размером

/// Заголовок архива обещает у workbook.xml 2⁶⁰ байт. Раньше под часть заранее занималось столько памяти —
/// отказ выделения обрывает процесс (окно программы закрывается без сообщения). Теперь заранее — не больше
/// 64 МБ, дальше — сколько данных на самом деле; книга обрабатывается.
#[test]
fn huge_declared_part_size_is_not_preallocated() {
    let d = tmp("p5_declared");
    let (good, bad, out) = (d.join("good.xlsx"), d.join("bad.xlsx"), d.join("out.xlsx"));
    write_xlsx(&good, &people_book());
    let parts = unpack(&good);
    let parts: Vec<(&str, Vec<u8>, Option<u64>)> = parts
        .iter()
        .map(|(n, data)| {
            let lie = matches!(n.as_str(), "xl/workbook.xml" | "xl/_rels/workbook.xml.rels" | "_rels/.rels").then_some(1u64 << 60);
            (n.as_str(), data.clone(), lie)
        })
        .collect();
    raw_zip(&bad, &parts);
    // Архив действительно заявляет огромный размер — иначе тест ничего не проверяет.
    {
        let mut z = zip::ZipArchive::new(std::fs::File::open(&bad).unwrap()).unwrap();
        assert_eq!(z.by_name("xl/workbook.xml").unwrap().size(), 1 << 60);
    }
    let wb = Workbook::open(&bad).unwrap();
    assert_eq!(wb.sheets.len(), 1);
    let report = run(&job(&bad, &out, vec![Rule::new("Л", 0, 2, None, Kind::FullName)]), &mut |_| {}).unwrap();
    assert!(report.leaks.is_empty(), "{:?}", report.leaks);
    let g = read_book(&out);
    let a2 = text(&g, "Л", "A2");
    assert!(!a2.is_empty() && a2 != NAME, "имя не заменено: {a2}");
}

// ---------------------------------------------------------------- битый UTF-16 в customXml

fn utf16le(s: &str) -> Vec<u8> {
    let mut v = vec![0xFF, 0xFE];
    for u in s.encode_utf16() {
        v.extend_from_slice(&u.to_le_bytes());
    }
    v
}

/// Часть customXml в UTF-16 с непарным суррогатом раньше обрывала всю обработку («неверный UTF-16»).
/// Теперь книга обезличивается, битая часть копируется как есть — с предупреждением, и проверка результата
/// не обещает, что в ней чисто. Исправная часть UTF-16 рядом по-прежнему обрабатывается.
#[test]
fn broken_utf16_custom_xml_warns_and_rest_is_anonymized() {
    let d = tmp("p5_utf16");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    write_xlsx(&input, &people_book());
    let mut broken = utf16le(r#"<?xml version="1.0" encoding="UTF-16"?><root><a>текст</a></root>"#);
    broken.extend_from_slice(&0xD800u16.to_le_bytes()); // непарный суррогат
    broken.extend_from_slice(&utf16le("<b/>")[2..]);
    let good = utf16le(&format!(r#"<?xml version="1.0" encoding="UTF-16"?><root><a>{NAME}</a></root>"#));
    repack(&input, &|_, _| None, &[("customXml/item1.xml", broken.clone()), ("customXml/item2.xml", good)]);

    let report = run(&job(&input, &output, vec![Rule::new("Л", 0, 2, None, Kind::FullName)]), &mut |_| {}).unwrap();
    assert!(
        report.warnings.iter().any(|w| w.contains("customXml/item1.xml") && w.contains("не прочитана")),
        "{:?}",
        report.warnings
    );
    assert!(report.unchecked.iter().any(|u| u == "customXml/item1.xml"), "{:?}", report.unchecked);
    let summary = report.summary(&Profile::default()).join("\n");
    assert!(summary.contains("Внимание: часть customXml/item1.xml не прочитана"), "{summary}");
    assert!(!summary.contains("реальных значений из размеченных колонок в нём не найдено."), "{summary}");

    let parts: std::collections::HashMap<String, Vec<u8>> = unpack(&output).into_iter().collect();
    // Битая часть — байт в байт как была.
    assert_eq!(parts["customXml/item1.xml"], broken);
    // Исправная — обезличена и осталась в UTF-16.
    let item2 = &parts["customXml/item2.xml"];
    assert_eq!(&item2[..2], &[0xFF, 0xFE]);
    let units: Vec<u16> = item2[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let text2 = String::from_utf16(&units).unwrap();
    assert!(!text2.contains(NAME), "{text2}");
    // Лист обезличен.
    let a2 = text(&read_book(&output), "Л", "A2");
    assert!(!a2.is_empty() && a2 != NAME, "{a2}");
}

// ---------------------------------------------------------------- книга для замеров

/// Книга для замера памяти и времени: записи сводной `records` × `fields` и общие строки на `sst_mb`
/// мегабайт без тегов строк. Пишется в PERF5_DIR (или во временную папку) вместе с профилем:
/// `finboard-anon-cli --profile profile.json --in big.xlsx --out big_anon.xlsx --no-key`.
#[test]
#[ignore]
fn perf5_make_big_book() {
    let records: usize = std::env::var("PERF5_RECORDS").ok().and_then(|v| v.parse().ok()).unwrap_or(300_000);
    let fields: usize = std::env::var("PERF5_FIELDS").ok().and_then(|v| v.parse().ok()).unwrap_or(30);
    let sst_mb: usize = std::env::var("PERF5_SST_MB").ok().and_then(|v| v.parse().ok()).unwrap_or(50);
    let dir = std::env::var("PERF5_DIR").map(std::path::PathBuf::from).unwrap_or_else(|_| tmp("p5_big"));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("big.xlsx");
    let people: Vec<String> = (0..2000).map(|i| format!("{} {}", NAMES[i % 4], i)).collect();

    let mut b = Book::default();
    let sh = b.sheet("Л");
    sh.push((1, 0, s("ФИО")));
    for (i, p) in people.iter().enumerate() {
        sh.push((2 + i as u32, 0, s(p)));
        sh.push((2 + i as u32, 1, Cell::N(i as f64)));
    }
    // Определение кэша сводной: источник — лист «Л», поле 0 — ФИО.
    let mut def = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><pivotCacheDefinition {NS} xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" r:id="rId1" recordCount="{records}"><cacheSource type="worksheet"><worksheetSource ref="A1:{}{}" sheet="Л"/></cacheSource><cacheFields count="{fields}">"#,
        finboard_anon::xlsx::col_name(fields as u32 - 1),
        records + 1
    );
    def.push_str(r#"<cacheField name="ФИО" numFmtId="0"><sharedItems/></cacheField>"#);
    for f in 1..fields {
        def.push_str(&format!(r#"<cacheField name="Поле {f}" numFmtId="0"><sharedItems containsNumber="1" containsInteger="1" minValue="0" maxValue="999999"/></cacheField>"#));
    }
    def.push_str("</cacheFields></pivotCacheDefinition>");
    b.extra.push(("xl/pivotCache/pivotCacheDefinition1.xml".into(), def));
    b.extra.push((
        "xl/pivotCache/_rels/pivotCacheDefinition1.xml.rels".into(),
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotCacheRecords" Target="pivotCacheRecords1.xml"/></Relationships>"#.into(),
    ));
    write_xlsx(&path, &b);

    // Большие части — дописываются потоком: записи сводной и общие строки без тегов строк.
    let mut rec = format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><pivotCacheRecords {NS} count="{records}">"#);
    for r in 0..records {
        rec.push_str(r#"<r><s v=""#);
        rec.push_str(&esc(&people[r % people.len()]));
        rec.push_str(r#""/>"#);
        for f in 1..fields {
            rec.push_str(&format!(r#"<n v="{}"/>"#, (r * 31 + f * 7) % 1_000_000));
        }
        rec.push_str("</r>");
    }
    rec.push_str("</pivotCacheRecords>");
    let mut sst_extra = String::new();
    let mut i = 0usize;
    while sst_extra.len() < sst_mb * MB {
        sst_extra.push_str(&format!("<si><t>Комментарий {i}: позвонить {} по договору {}</t></si>", people[i % people.len()], 100_000 + i));
        i += 1;
    }
    let (recs, sst_len) = (rec.len(), sst_extra.len());
    let sst_extra_ref = &sst_extra;
    repack(
        &path,
        &|name, t| (name == "xl/sharedStrings.xml").then(|| t.replace("</sst>", &format!("{sst_extra_ref}</sst>"))),
        &[("xl/pivotCache/pivotCacheRecords1.xml", rec.into_bytes())],
    );
    let profile = Profile { rules: vec![Rule::new("Л", 0, 2, None, Kind::FullName)], settings: Settings::default() };
    profile.save(&dir.join("profile.json")).unwrap();
    let mut f = std::fs::File::create(dir.join("sizes.txt")).unwrap();
    writeln!(f, "pivotCacheRecords: {} МБ, sharedStrings: {} МБ, файл: {} МБ", recs / MB, sst_len / MB, std::fs::metadata(&path).unwrap().len() as usize / MB).unwrap();
    println!("{}", path.display());
}
