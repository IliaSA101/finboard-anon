//! Находки независимого охотника за пробелами (сценарии воспроизведены им на файлах).
//! Каждый тест падал до исправления. Общая проверка — реальных значений нет нигде в файле,
//! включая текст, разрезанный на фрагменты форматирования.

mod common;

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use common::*;
use finboard_anon::anon::{run, scan_leaks, Job, Report};
use finboard_anon::detect;
use finboard_anon::fake::Faker;
use finboard_anon::profile::{Kind, Profile, Rule, Settings};
use finboard_anon::xlsx::{load_grid, Workbook};

fn prof(rules: Vec<Rule>) -> Profile {
    Profile { rules, settings: Settings { salt: "hunt".into(), ..Settings::default() } }
}

fn go(input: &Path, output: &Path, key_in: Option<&Path>, key_out: Option<&Path>, p: &Profile) -> Report {
    let job = Job { input: input.into(), output: output.into(), key_in: key_in.map(Into::into), key_out: key_out.map(Into::into), profile: p.clone() };
    run(&job, &mut |_| {}).unwrap()
}

fn parts(path: &Path) -> HashMap<String, String> {
    let mut z = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut out = HashMap::new();
    for i in 0..z.len() {
        let mut f = z.by_index(i).unwrap();
        let mut s = String::new();
        if f.read_to_string(&mut s).is_ok() {
            out.insert(f.name().to_string(), s);
        }
    }
    out
}

fn unescape(s: &str) -> String {
    s.replace("&quot;", "\"").replace("&apos;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

/// Текст без тегов: фрагменты одного абзаца склеиваются, как их видит человек.
fn detag(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    unescape(&out)
}

/// Ни одного реального значения ни в одной части файла (в сыром XML и в склеенном тексте).
fn assert_absent(path: &Path, reals: &[&str]) {
    for (name, xml) in parts(path) {
        let (raw, text) = (unescape(&xml), detag(&xml));
        for r in reals {
            assert!(!raw.contains(r) && !text.contains(r), "«{r}» осталось в {name}");
        }
    }
}

fn detect_all(path: &Path) -> Vec<String> {
    let wb = Workbook::open(path).unwrap();
    let mut v = Vec::new();
    for (i, sh) in wb.sheets.iter().enumerate() {
        for r in detect::suggest(&sh.name, &load_grid(path, &wb, i, 1_000_000).unwrap()) {
            v.push(format!("{} {:?}", r.range_text(), r.kind));
        }
    }
    v
}

const RELS_NS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#;

// 1 ---------------------------------------------------------------- колонка из формул (внешняя книга)

#[test]
fn h01_formula_column_from_external_book() {
    let d = tmp("h01");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    let mut b = Book::default();
    let c = b.sheet("Дашборд");
    c.extend([(1, 0, s("ФИО")), (1, 1, s("Телефон"))]);
    c.push((2, 0, Cell::FS("[1]Сотрудники!A2".into(), "Иванов Иван Иванович".into())));
    c.push((2, 1, Cell::FS("[1]Сотрудники!B2".into(), "+7 (916) 123-45-67".into())));
    c.push((3, 0, Cell::FS("[1]Сотрудники!A3".into(), "Петрова Мария Сергеевна".into())));
    b.extra.push(("xl/externalLinks/externalLink1.xml".into(), r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><externalLink xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><externalBook><sheetNames><sheetName val="Сотрудники"/></sheetNames><sheetDataSet><sheetData sheetId="0"><row r="2"><cell r="A2" t="str"><v>Иванов Иван Иванович</v></cell><cell r="B2" t="str"><v>+7 (916) 123-45-67</v></cell></row><row r="3"><cell r="A3" t="str"><v>Петрова Мария Сергеевна</v></cell></row></sheetData></sheetDataSet></externalBook></externalLink>"#.into()));
    write_xlsx(&input, &b);
    let p = prof(vec![Rule::new("Дашборд", 0, 2, None, Kind::FullName), Rule::new("Дашборд", 1, 2, None, Kind::Phone)]);
    let rep = go(&input, &output, None, None, &p);
    assert!(rep.leaks.is_empty(), "{:?}", rep.leaks);
    assert_absent(&output, &["Иванов Иван Иванович", "Петрова Мария Сергеевна", "+7 (916) 123-45-67"]);
    assert!(parts(&output)["xl/worksheets/sheet1.xml"].contains("[1]Сотрудники!A2"), "формула должна остаться");
}

// 2 ---------------------------------------------------------------- производные кэши формул

#[test]
fn h02_derived_formula_caches() {
    let d = tmp("h02");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    let mut b = Book::default();
    let c = b.sheet("Л");
    c.extend([(1, 0, s("ФИО")), (1, 1, s("Телефон")), (1, 2, s("Дата рождения"))]);
    c.push((2, 0, s("Иванов Иван Иванович")));
    c.push((2, 1, Cell::N(9161234567.0)));
    c.push((2, 2, Cell::N(30000.0)));
    c.push((2, 3, Cell::FS("LEFT(A2,FIND(\" \",A2)-1)".into(), "Иванов".into())));
    c.push((2, 4, Cell::FS("TEXT(C2,\"dd.mm.yyyy\")".into(), "18.02.1982".into())));
    c.push((2, 5, Cell::F("C2".into(), 30000.0)));
    c.push((2, 6, Cell::FS("TEXT(B2,\"+7 (000) 000-00-00\")".into(), "+7 (916) 123-45-67".into())));
    // Цепочка: формула от формулы.
    c.push((2, 7, Cell::FS("UPPER(D2)".into(), "ИВАНОВ".into())));
    // Формула, не зависящая от ПДн, сохраняет кэш.
    c.push((2, 8, Cell::F("1+1".into(), 2.0)));
    write_xlsx(&input, &b);
    let p = prof(vec![
        Rule::new("Л", 0, 2, None, Kind::FullName),
        Rule::new("Л", 1, 2, None, Kind::Phone),
        Rule::new("Л", 2, 2, None, Kind::BirthDate),
    ]);
    go(&input, &output, None, None, &p);
    assert_absent(&output, &["Иванов", "ИВАНОВ", "18.02.1982", "+7 (916) 123-45-67"]);
    let sheet = parts(&output)["xl/worksheets/sheet1.xml"].clone();
    let cell = |a: &str| sheet.split(&format!(r#"<c r="{a}""#)).nth(1).unwrap().split("</c>").next().unwrap().to_string();
    assert!(!cell("F2").contains("<v>30000</v>"), "кэш =C2 с датой рождения: {}", cell("F2"));
    assert!(cell("I2").contains("<v>2</v>"), "кэш независимой формулы потерян: {}", cell("I2"));
}

// 3 ---------------------------------------------------------------- двухстрочная шапка, клиент без контактов, mail.ru

fn clients_book(group_header: &str) -> Book {
    let mut b = Book::default();
    let c = b.sheet("Клиенты");
    c.extend([(1, 0, s("№")), (1, 1, s(group_header)), (2, 2, s("Телефон")), (2, 3, s("Email"))]);
    if group_header == "Клиент" {
        c.push((2, 1, s("ФИО клиента")));
    } else {
        c.push((1, 2, s("Контакты")));
    }
    let rows = [
        ("Иванов Иван Иванович", Some(("+7 (916) 111-22-33", "ivanov@mail.ru"))),
        ("Петров Пётр Петрович", Some(("+7 (916) 222-33-44", "petrov@mail.ru"))),
        ("Сидоров Олег Ильич", None),
        ("Кузнецова Анна Сергеевна", Some(("+7 (916) 444-55-66", "kuznetsova@mail.ru"))),
    ];
    for (i, (n, contacts)) in rows.iter().enumerate() {
        let r = 3 + i as u32;
        c.push((r, 0, Cell::N((i + 1) as f64)));
        c.push((r, 1, s(n)));
        if let Some((ph, em)) = contacts {
            c.push((r, 2, s(ph)));
            c.push((r, 3, s(em)));
        }
    }
    b
}

#[test]
fn h03_two_row_header_and_gap_in_contacts() {
    let d = tmp("h03");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    write_xlsx(&input, &clients_book("ФИО клиента"));
    assert_eq!(detect_all(&input), ["B3:B… FullName", "C3:C… Phone", "D3:D… Email"]);
    let wb = Workbook::open(&input).unwrap();
    let p = prof(detect::suggest("Клиенты", &load_grid(&input, &wb, 0, 1000).unwrap()));
    go(&input, &output, None, None, &p);
    assert_absent(&output, &["+7 (916) 444-55-66", "kuznetsova@mail.ru", "Кузнецова Анна Сергеевна"]);
}

// 7 ---------------------------------------------------------------- групповая шапка «Клиент» над подзаголовками

#[test]
fn h07_group_header_above_subheaders() {
    let d = tmp("h07");
    let input = d.join("in.xlsx");
    write_xlsx(&input, &clients_book("Клиент"));
    assert_eq!(detect_all(&input), ["B3:B… FullName", "C3:C… Phone", "D3:D… Email"]);
}

// 4 ---------------------------------------------------------------- книга 1904 и сводная по дате рождения

#[test]
fn h04_date1904_pivot() {
    let d = tmp("h04");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    let iso = "1986-02-19T00:00:00"; // 30000 в системе 1904
    let mut b = Book { date1904: true, ..Default::default() };
    b.sheet("Л").extend([(1, 0, s("ФИО")), (1, 1, s("Дата рождения")), (2, 0, s("Иванов Иван Иванович")), (2, 1, Cell::N(30000.0))]);
    b.extra.push(("xl/pivotCache/pivotCacheDefinition1.xml".into(), format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><pivotCacheDefinition xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" r:id="rId1" recordCount="1"><cacheSource type="worksheet"><worksheetSource ref="A1:B2" sheet="Л"/></cacheSource><cacheFields count="2"><cacheField name="ФИО" numFmtId="0"><sharedItems count="1"><s v="Иванов Иван Иванович"/></sharedItems></cacheField><cacheField name="Дата рождения" numFmtId="14"><sharedItems containsDate="1" minDate="{iso}" maxDate="{iso}" count="1"><d v="{iso}"/></sharedItems></cacheField></cacheFields></pivotCacheDefinition>"#)));
    b.extra.push(("xl/pivotCache/pivotCacheRecords1.xml".into(), format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><pivotCacheRecords xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="1"><r><x v="0"/><d v="{iso}"/></r></pivotCacheRecords>"#)));
    b.extra.push(("xl/pivotCache/_rels/pivotCacheDefinition1.xml.rels".into(), format!(r#"{RELS_NS}<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotCacheRecords" Target="pivotCacheRecords1.xml"/></Relationships>"#)));
    write_xlsx(&input, &b);
    let p = prof(vec![Rule::new("Л", 0, 2, None, Kind::FullName), Rule::new("Л", 1, 2, None, Kind::BirthDate)]);
    go(&input, &output, None, None, &p);
    assert_absent(&output, &["1986-02-19", "Иванов Иван Иванович"]);
}

// 5 ---------------------------------------------------------------- имя, разрезанное на фрагменты в примечании

fn comment_book() -> Book {
    let mut b = Book::default();
    b.sheet("Л").extend([(1, 0, s("ФИО")), (2, 0, s("Иванов Иван Иванович"))]);
    b.extra.push(("xl/comments1.xml".into(), r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><comments xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><authors><author>Автор</author></authors><commentList><comment ref="B2" authorId="0"><text><r><rPr><b/></rPr><t>Автор:</t></r><r><t xml:space="preserve">Звонил </t></r><r><rPr><b/></rPr><t>Иванов Иван</t></r><r><t xml:space="preserve"> Иванович, перезвонить</t></r></text></comment></commentList></comments>"#.into()));
    b
}

#[test]
fn h05_name_split_across_runs_in_comment() {
    let d = tmp("h05");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    write_xlsx(&input, &comment_book());
    go(&input, &output, None, None, &prof(vec![Rule::new("Л", 0, 2, None, Kind::FullName)]));
    assert_absent(&output, &["Иванов Иван Иванович", "Иванов Иван"]);
    // Сканер видит разрезанное имя, даже если заменять его не стали.
    let leaks = scan_leaks(&input, &["Иванов Иван Иванович".to_string()], &[]).unwrap();
    assert!(leaks.iter().any(|l| l.part == "xl/comments1.xml"), "{leaks:?}");
}

// 6 ---------------------------------------------------------------- сканер: & и " вместе

#[test]
fn h06_scanner_sees_amp_and_quote_together() {
    let d = tmp("h06");
    let path = d.join("in.xlsx");
    let mut b = Book::default();
    b.sheet("Л").push((1, 0, s("x")));
    b.sheet_tail.insert(0, r#"<headerFooter><oddHeader>Петров "Пётр" &amp; Ко</oddHeader></headerFooter>"#.into());
    write_xlsx(&path, &b);
    let leaks = scan_leaks(&path, &["Петров \"Пётр\" & Ко".to_string()], &[]).unwrap();
    assert_eq!(leaks.len(), 1, "{leaks:?}");
}

// 8 ---------------------------------------------------------------- паника на «(№ 5)»

#[test]
fn h08_no_panic_on_multibyte_parenthesis() {
    let mut f = Faker::new(&Settings { salt: "x".into(), ..Settings::default() });
    let r = Rule::new("Л", 0, 1, None, Kind::FullName);
    for v in ["Иванов Иван (№ 5)", "Петров Пётр (— уволен)", "Сидоров (ИД 7)", "Кузнецов (I)", "()", "Анна ("] {
        let out = f.fake(&r, v, false);
        assert!(!out.is_empty(), "{v}");
    }
    assert!(f.fake(&r, "Иванов Иван (№ 5)", false).contains("(№ 5)"));
}

// 9 ---------------------------------------------------------------- умная таблица: шапка == tableColumn@name

#[test]
fn h09_table_column_names_follow_header_cells() {
    let d = tmp("h09");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    let mut b = Book::default();
    b.sheet("Менеджеры").extend([(1, 0, s("Менеджер")), (2, 0, s("Иванов Иван Иванович")), (3, 0, s("Петрова Мария Сергеевна"))]);
    b.sheet("План").extend([
        (1, 0, s("Месяц")), (1, 1, s("Иванов Иван Иванович")), (1, 2, s("Петрова Мария Сергеевна")),
        (2, 0, s("Январь")), (2, 1, Cell::N(100.0)), (2, 2, Cell::N(200.0)),
        (3, 1, Cell::F("SUM(План[Иванов Иван Иванович])".into(), 100.0)),
    ]);
    b.extra.push(("xl/tables/table1.xml".into(), r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><table xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" id="1" name="План" displayName="План" ref="A1:C2"><autoFilter ref="A1:C2"/><tableColumns count="3"><tableColumn id="1" name="Месяц"/><tableColumn id="2" name="Иванов Иван Иванович"/><tableColumn id="3" name="Петрова Мария Сергеевна"/></tableColumns></table>"#.into()));
    write_xlsx(&input, &b);
    go(&input, &output, None, None, &prof(vec![Rule::new("Менеджеры", 0, 2, None, Kind::FullName)]));
    let g = read_book(&output);
    let table = unescape(&parts(&output)["xl/tables/table1.xml"]);
    for a1 in ["B1", "C1"] {
        let head = text(&g, "План", a1);
        assert!(table.contains(&format!(r#"name="{head}""#)), "шапка {head} не совпадает с таблицей: {table}");
    }
    let sheet2 = unescape(&parts(&output)["xl/worksheets/sheet2.xml"]);
    assert!(sheet2.contains(&format!("План[{}]", text(&g, "План", "B1"))), "структурная ссылка: {sheet2}");
    assert_absent(&output, &["Иванов Иван Иванович", "Петрова Мария Сергеевна"]);
}

// 10 --------------------------------------------------------------- инициалы после загрузки ключа

#[test]
fn h10_initials_link_to_full_name_from_key() {
    let d = tmp("h10");
    let (in1, out1, key, in2, out2) = (d.join("in1.xlsx"), d.join("o1.xlsx"), d.join("k.csv"), d.join("in2.xlsx"), d.join("o2.xlsx"));
    let mut b = Book::default();
    b.sheet("Л").extend([(1, 0, s("ФИО")), (2, 0, s("Сидоров Пётр Алексеевич"))]);
    write_xlsx(&in1, &b);
    let p = prof(vec![Rule::new("Л", 0, 2, None, Kind::FullName)]);
    go(&in1, &out1, None, Some(&key), &p);
    let full = text(&read_book(&out1), "Л", "A2");
    let mut b = Book::default();
    b.sheet("Л").extend([(1, 0, s("ФИО")), (2, 0, s("Сидоров П.А."))]);
    write_xlsx(&in2, &b);
    go(&in2, &out2, Some(&key), None, &p);
    let short = text(&read_book(&out2), "Л", "A2");
    let w: Vec<&str> = full.split_whitespace().collect();
    let expect = format!("{} {}.{}. (ID 1)", w[0], w[1].chars().next().unwrap(), w[2].chars().next().unwrap());
    assert_eq!(short, expect, "полное: {full}");
}

// 11 --------------------------------------------------------------- без ID разные люди не сливаются

#[test]
fn h11_distinct_people_distinct_fakes_without_id() {
    let mut f = Faker::new(&Settings { salt: "x".into(), ..Settings::default() });
    let mut r = Rule::new("Л", 0, 1, None, Kind::FullName);
    r.with_id = false;
    let stems = ["Иванов", "Петров", "Сидоров", "Павлов", "Фёдоров", "Андреев", "Николаев", "Сергеев", "Алексеев", "Дмитриев",
        "Борисов", "Глебов", "Егоров", "Захаров", "Ильин", "Кириллов", "Львов", "Максимов", "Олегов", "Романов",
        "Степанов", "Тимофеев", "Ярославов", "Артёмов", "Денисов", "Игорев", "Константинов", "Леонидов", "Маркович", "Никитин",
        "Остапов", "Прохоров", "Русланов", "Савельев", "Тарасов", "Устинов", "Филиппов", "Харитонов", "Эдуардов", "Юрьев",
        "Вадимов", "Геннадьев", "Григорьев", "Евгеньев"];
    let mut seen = HashMap::new();
    for st in stems {
        let real = format!("Тестов Иван {st}ич");
        let fake = f.fake(&r, &real, false);
        if let Some(prev) = seen.insert(fake.clone(), real.clone()) {
            panic!("{prev} и {real} → один фейк {fake}");
        }
    }
}

// 12 --------------------------------------------------------------- организации без ID после ключа

#[test]
fn h12_company_after_key_does_not_collide() {
    let s1 = Settings { salt: "a".into(), ..Settings::default() };
    let mut r = Rule::new("Л", 0, 1, None, Kind::Company);
    r.with_id = false;
    let mut f = Faker::new(&s1);
    let mut known = Vec::new();
    for i in 0..30 {
        known.push(f.fake(&r, &format!("ООО Альфа{i}"), false));
    }
    let mut g = Faker::new(&Settings { salt: "b".into(), ..Settings::default() });
    g.load_entries(f.entries.clone());
    for i in 0..30 {
        let new = g.fake(&r, &format!("ООО Бета{i}"), false);
        assert!(!known.contains(&new), "новая организация получила чужой фейк {new}");
    }
}

// 15 --------------------------------------------------------------- телефон-число внутри текста примечания
// (нашлось при проверке файлов охотника в Excel)

#[test]
fn h15_numeric_phone_inside_comment_text() {
    let d = tmp("h15");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    let mut b = Book::default();
    b.sheet("Л").extend([(1, 0, s("Телефон")), (2, 0, Cell::N(9161234567.0)), (3, 0, Cell::N(91612345.0))]);
    b.extra.push(("xl/comments1.xml".into(), r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><comments xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><authors><author>А</author></authors><commentList><comment ref="B2" authorId="0"><text><r><t>Перезвонить, тел. 9161234567; доб. 91612345</t></r></text></comment></commentList></comments>"#.into()));
    write_xlsx(&input, &b);
    let rep = go(&input, &output, None, None, &prof(vec![Rule::new("Л", 0, 2, None, Kind::Phone)]));
    assert!(rep.leaks.is_empty(), "{:?}", rep.leaks);
    assert_absent(&output, &["9161234567"]);
    // Короткое число (8 цифр) внутри текста не трогаем — слишком легко совпасть случайно.
    assert!(parts(&output)["xl/comments1.xml"].contains("доб. 91612345"));
}

// гипотезы охотника, которые подтвердились ------------------------------

#[test]
fn h13_mailto_hyperlink_target() {
    let d = tmp("h13");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    let mut b = Book::default();
    b.sheet("Л").extend([(1, 0, s("Email")), (2, 0, s("ivanov@mail.ru"))]);
    b.sheet_tail.insert(0, r#"<hyperlinks><hyperlink ref="A2" r:id="rId1"/></hyperlinks>"#.into());
    b.extra.push(("xl/worksheets/_rels/sheet1.xml.rels".into(), format!(r#"{RELS_NS}<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="mailto:ivanov@mail.ru" TargetMode="External"/></Relationships>"#)));
    write_xlsx(&input, &b);
    let rep = go(&input, &output, None, None, &prof(vec![Rule::new("Л", 0, 2, None, Kind::Email)]));
    assert_absent(&output, &["ivanov@mail.ru"]);
    assert!(rep.leaks.is_empty());
}

#[test]
fn h14_errors_and_booleans_in_rule_column_untouched() {
    let d = tmp("h14");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    let mut b = Book::default();
    b.sheet("Л").extend([(1, 0, s("ФИО")), (2, 0, s("Иванов Иван")), (3, 0, Cell::T("e", "#N/A".into())), (4, 0, Cell::T("b", "1".into()))]);
    // «#N/A» встречается и вне правила — если его считать данными, проверка поднимет ложную тревогу.
    b.sheet("Сводка").push((1, 0, Cell::T("e", "#N/A".into())));
    write_xlsx(&input, &b);
    let p = prof(vec![Rule::new("Л", 0, 2, None, Kind::FullName)]);
    let rep = go(&input, &output, None, None, &p);
    let sheet = parts(&output)["xl/worksheets/sheet1.xml"].clone();
    assert!(rep.leaks.is_empty(), "ошибка ячейки считается данными: {:?}", rep.leaks);
    let pv = finboard_anon::anon::preview(&input, &p, None).unwrap();
    assert_eq!(pv[0].cells, 1, "в правиле одна ячейка с данными: {:?}", pv[0]);
    assert!(sheet.contains(r#"<c r="A3" t="e"><v>#N/A</v></c>"#), "{sheet}");
    assert!(sheet.contains(r#"<c r="A4" t="b"><v>1</v></c>"#), "{sheet}");
}
