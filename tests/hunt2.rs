//! Находки второго круга охотника за пробелами (по исправлениям первого круга).
//! Каждый тест падал до исправления.

mod common;

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::time::Instant;

use common::*;
use finboard_anon::anon::{run, Job, Report};
use finboard_anon::detect;
use finboard_anon::profile::{Kind, Profile, Rule, Settings};
use finboard_anon::xlsx::{load_grid, Workbook};

fn prof(rules: Vec<Rule>) -> Profile {
    Profile { rules, settings: Settings { salt: "hunt2".into(), ..Settings::default() } }
}

fn go(input: &Path, output: &Path, p: &Profile) -> Report {
    let job = Job { input: input.into(), output: output.into(), key_in: None, key_out: None, profile: p.clone() };
    run(&job, &mut |_| {}).unwrap()
}

fn parts(path: &Path) -> HashMap<String, String> {
    let mut z = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut out = HashMap::new();
    for i in 0..z.len() {
        let mut f = z.by_index(i).unwrap();
        let mut s = String::new();
        if f.read_to_string(&mut s).is_ok() {
            out.insert(f.name().to_string(), s.replace("&quot;", "\"").replace("&amp;", "&"));
        }
    }
    out
}

fn assert_absent(path: &Path, reals: &[&str]) {
    for (name, xml) in parts(path) {
        for r in reals {
            assert!(!xml.contains(r), "«{r}» осталось в {name}");
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

fn attr_values(xml: &str, elem: &str, attr: &str) -> Vec<String> {
    xml.split(&format!("<{elem} ")).skip(1).filter_map(|t| t.split(&format!("{attr}=\"")).nth(1)?.split('"').next().map(str::to_string)).collect()
}

const RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#;
const NS: &str = r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main""#;

// 1 ---------------------------------------------------------------- авторы примечаний остаются разными

#[test]
fn r01_comment_authors_stay_distinct() {
    let d = tmp("r01");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    let mut b = Book::default();
    b.sheet("Л").extend([(1, 0, s("ФИО")), (2, 0, s("Иванов Иван Иванович"))]);
    b.extra.push(("xl/comments1.xml".into(), format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><comments {NS}><authors><author>Петров П.</author><author>Сидорова А.</author></authors><commentList><comment ref="B2" authorId="0"><text><t>a</t></text></comment><comment ref="B3" authorId="1"><text><t>b</t></text></comment></commentList></comments>"#)));
    write_xlsx(&input, &b);
    go(&input, &output, &prof(vec![Rule::new("Л", 0, 2, None, Kind::FullName)]));
    let c = parts(&output)["xl/comments1.xml"].clone();
    let authors: Vec<&str> = c.split("<author>").skip(1).map(|x| x.split("</author>").next().unwrap()).collect();
    assert_eq!(authors.len(), 2);
    assert_ne!(authors[0], authors[1], "одинаковые авторы — Excel требует восстановления: {c}");
    assert!(!c.contains("Петров") && !c.contains("Сидорова"));
}

// 2, 7 ------------------------------------------------------------- границы дат в сводной

fn pivot_parts(b: &mut Book, source: &str, field_extra: &str, def_attrs: &str) {
    let iso = "1982-02-18T00:00:00";
    let next = "1982-02-19T00:00:00";
    b.extra.push(("xl/pivotCache/pivotCacheDefinition1.xml".into(), format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><pivotCacheDefinition {NS} xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" r:id="rId1" recordCount="1"{def_attrs}><cacheSource type="worksheet"><worksheetSource {source}/></cacheSource><cacheFields count="2"><cacheField name="ФИО" numFmtId="0"><sharedItems count="1"><s v="Иванов Иван Иванович"/></sharedItems></cacheField><cacheField name="Дата рождения" numFmtId="14"><sharedItems containsDate="1" minDate="{iso}" maxDate="{next}"/>{field_extra}</cacheField></cacheFields></pivotCacheDefinition>"#)));
    b.extra.push(("xl/pivotCache/pivotCacheRecords1.xml".into(), format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><pivotCacheRecords {NS} count="1"><r><x v="0"/><d v="{iso}"/></r></pivotCacheRecords>"#)));
    b.extra.push(("xl/pivotCache/_rels/pivotCacheDefinition1.xml.rels".into(), format!(r#"{RELS}<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotCacheRecords" Target="pivotCacheRecords1.xml"/></Relationships>"#)));
}

fn birth_book() -> Book {
    let mut b = Book::default();
    b.sheet("Л").extend([(1, 0, s("ФИО")), (1, 1, s("Дата рождения")), (2, 0, s("Иванов Иван Иванович")), (2, 1, Cell::N(30000.0))]);
    b
}

fn birth_rules() -> Profile {
    prof(vec![Rule::new("Л", 0, 2, None, Kind::FullName), Rule::new("Л", 1, 2, None, Kind::BirthDate)])
}

#[test]
fn r02_unknown_pivot_source_drops_both_date_bounds() {
    let d = tmp("r02");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    let mut b = birth_book();
    pivot_parts(&mut b, r#"name="Сотрудники""#, "", "");
    write_xlsx(&input, &b);
    go(&input, &output, &birth_rules());
    let def = parts(&output)["xl/pivotCache/pivotCacheDefinition1.xml"].clone();
    assert!(!def.contains("minDate") && !def.contains("maxDate"), "одна граница без другой — Excel требует восстановления: {def}");
}

#[test]
fn r07_pivot_date_grouping_and_refreshed_by() {
    let d = tmp("r07");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    let mut b = birth_book();
    pivot_parts(
        &mut b,
        r#"ref="A1:B2" sheet="Л""#,
        r#"<fieldGroup base="1"><rangePr groupBy="months" startDate="1982-02-18T00:00:00" endDate="1982-02-19T00:00:00"/><groupItems count="1"><s v="фев"/></groupItems></fieldGroup>"#,
        r#" refreshedBy="Петров Пётр""#,
    );
    write_xlsx(&input, &b);
    go(&input, &output, &birth_rules());
    let def = parts(&output)["xl/pivotCache/pivotCacheDefinition1.xml"].clone();
    // Границы поля сняты, границы группировки дат остаются согласованными с группами (иначе Excel
    // требует восстановления — проверено на файле охотника xl1r).
    let shared = def.split("<sharedItems").nth(2).unwrap().split('>').next().unwrap().to_string();
    assert!(!shared.contains("minDate") && !shared.contains("maxDate"), "{shared}");
    assert!(def.contains(r#"<rangePr groupBy="months" startDate="1982-02-18T00:00:00" endDate="1982-02-19T00:00:00"/>"#), "{def}");
    assert!(!def.contains("Петров"), "имя обновлявшего сводную: {def}");
}

// 3 ---------------------------------------------------------------- фейк не совпадает с уже имеющимися строками книги

#[test]
fn r03_fakes_avoid_existing_strings_table_columns_unique() {
    let d = tmp("r03");
    let surnames = ["Иванов", "Петров", "Сидоров", "Кузнецов", "Смирнов", "Попов", "Лебедев", "Козлов", "Новиков", "Морозов"];
    let rules = prof(vec![Rule::new("Список", 0, 2, None, Kind::LastName)]);
    let list = |b: &mut Book| {
        let c = b.sheet("Список");
        c.push((1, 0, s("Фамилия")));
        for (i, n) in surnames.iter().enumerate() {
            c.push((2 + i as u32, 0, s(n)));
        }
    };
    // Прогон 1: узнаём фейки при этой соли.
    let (in1, out1) = (d.join("in1.xlsx"), d.join("out1.xlsx"));
    let mut b = Book::default();
    list(&mut b);
    write_xlsx(&in1, &b);
    go(&in1, &out1, &rules);
    let g = read_book(&out1);
    let fakes: Vec<String> = (0..surnames.len()).map(|i| text(&g, "Список", &format!("A{}", 2 + i))).collect();
    // Прогон 2: те же фамилии + умная таблица, колонки которой называются этими фейками.
    let (in2, out2) = (d.join("in2.xlsx"), d.join("out2.xlsx"));
    let mut b = Book::default();
    list(&mut b);
    let plan = b.sheet("План");
    plan.push((1, 0, s("Иванов")));
    for (i, f) in fakes.iter().enumerate() {
        plan.push((1, 1 + i as u32, s(f)));
    }
    let cols: String = std::iter::once("Иванов".to_string()).chain(fakes.iter().cloned()).enumerate().map(|(i, n)| format!(r#"<tableColumn id="{}" name="{}"/>"#, i + 1, esc(&n))).collect();
    b.extra.push(("xl/tables/table1.xml".into(), format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><table {NS} id="1" name="План" displayName="План" ref="A1:K2"><tableColumns count="11">{cols}</tableColumns></table>"#)));
    write_xlsx(&in2, &b);
    go(&in2, &out2, &rules);
    let names = attr_values(&parts(&out2)["xl/tables/table1.xml"], "tableColumn", "name");
    let mut uniq = names.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), names.len(), "повтор имён колонок — Excel требует восстановления: {names:?}");
}

// 4 ---------------------------------------------------------------- значения, разлитые формулой-массивом

#[test]
fn r04_dynamic_array_spill_cells() {
    let d = tmp("r04");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    let mut b = Book::default();
    b.sheet("Данные").extend([(1, 0, s("ФИО")), (2, 0, s("Иванов Иван Иванович")), (3, 0, s("Петрова Мария Сергеевна")), (4, 0, s("Сидоров Олег Ильич"))]);
    let dash = b.sheet("Дашборд");
    dash.push((2, 0, Cell::Raw(r#"t="str"><f t="array" ref="A2:A4">_xlfn.UNIQUE(Данные!A2:A4)</f><v>Иванов Иван Иванович</v>"#.into())));
    dash.push((3, 0, Cell::Raw(r#"t="str"><v>Петрова Мария Сергеевна</v>"#.into())));
    dash.push((4, 0, Cell::Raw(r#"t="str"><v>Сидоров Олег Ильич</v>"#.into())));
    // Производный разлив: фамилии через ЛЕВСИМВ — точного совпадения нет, кэш очищается.
    dash.push((2, 1, Cell::Raw(r#"t="str"><f t="array" ref="B2:B4">LEFT(Данные!A2:A4,FIND(" ",Данные!A2:A4)-1)</f><v>Иванов</v>"#.into())));
    dash.push((3, 1, Cell::Raw(r#"t="str"><v>Петрова</v>"#.into())));
    dash.push((4, 1, Cell::Raw(r#"t="str"><v>Сидоров</v>"#.into())));
    write_xlsx(&input, &b);
    let rep = go(&input, &output, &prof(vec![Rule::new("Данные", 0, 2, None, Kind::FullName)]));
    assert!(rep.leaks.is_empty(), "{:?}", rep.leaks);
    assert_absent(&output, &["Иванов", "Петрова", "Сидоров"]);
}

// 5 ---------------------------------------------------------------- производные через структурные ссылки и числа от даты

#[test]
fn r05_structured_refs_and_numeric_derivations() {
    let d = tmp("r05");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    let mut b = Book::default();
    let c = b.sheet("Сотрудники");
    c.extend([(1, 0, s("ФИО")), (1, 1, s("Дата рождения")), (1, 2, s("Фамилия")), (1, 3, s("Год")), (1, 4, s("Однофамильцев"))]);
    c.push((2, 0, s("Иванов Иван Иванович")));
    c.push((2, 1, Cell::N(30000.0)));
    c.push((2, 2, Cell::FS("LEFT([@ФИО],FIND(\" \",[@ФИО])-1)".into(), "Иванов".into())));
    c.push((2, 3, Cell::F("YEAR(Таблица1[[#This Row],[Дата рождения]])".into(), 1982.0)));
    c.push((2, 4, Cell::F("COUNTIF(Таблица1[ФИО],[@ФИО])".into(), 1.0)));
    b.extra.push(("xl/tables/table1.xml".into(), format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><table {NS} id="1" name="Таблица1" displayName="Таблица1" ref="A1:E2"><tableColumns count="5"><tableColumn id="1" name="ФИО"/><tableColumn id="2" name="Дата рождения"/><tableColumn id="3" name="Фамилия"/><tableColumn id="4" name="Год"/><tableColumn id="5" name="Однофамильцев"/></tableColumns></table>"#)));
    b.extra.push(("xl/worksheets/_rels/sheet1.xml.rels".into(), format!(r#"{RELS}<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/table" Target="../tables/table1.xml"/></Relationships>"#)));
    b.sheet_tail.insert(0, r#"<tableParts count="1"><tablePart xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" r:id="rId1"/></tableParts>"#.into());
    write_xlsx(&input, &b);
    go(&input, &output, &prof(vec![Rule::new("Сотрудники", 0, 2, None, Kind::FullName), Rule::new("Сотрудники", 1, 2, None, Kind::BirthDate)]));
    let sheet = parts(&output)["xl/worksheets/sheet1.xml"].clone();
    let cell = |a: &str| sheet.split(&format!(r#"<c r="{a}""#)).nth(1).unwrap().split("</c>").next().unwrap().to_string();
    assert!(!cell("C2").contains("Иванов"), "фамилия из [@ФИО]: {}", cell("C2"));
    assert!(!cell("D2").contains("<v>1982</v>"), "год рождения: {}", cell("D2"));
    assert!(cell("E2").contains("<v>1</v>"), "счётчик по именам не от ПДн по смыслу — кэш остаётся: {}", cell("E2"));
}

// 6 ---------------------------------------------------------------- лист вне ячеек и определённые имена

#[test]
fn r06_sheet_extras_and_defined_names() {
    let d = tmp("r06");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    let mut b = Book::default();
    b.sheet("Л").extend([(1, 0, s("ФИО")), (2, 0, s("Иванов Иван Иванович")), (3, 0, s("Петров Пётр Петрович"))]);
    b.sheet_tail.insert(0, r#"<conditionalFormatting sqref="A2:A3"><cfRule type="expression" priority="1"><formula>$A2="Иванов Иван Иванович"</formula></cfRule></conditionalFormatting><dataValidations count="1"><dataValidation type="list" sqref="B2" prompt="Выберите Иванов Иван Иванович"><formula1>"Иванов Иван Иванович,Петров Пётр Петрович"</formula1></dataValidation></dataValidations><hyperlinks><hyperlink ref="A2" location="Л!A3" tooltip="Написать Иванов Иван Иванович"/></hyperlinks><headerFooter><oddHeader>Отв. Петров Пётр Петрович</oddHeader></headerFooter>"#.into());
    write_xlsx(&input, &b);
    // Определённое имя со строкой — в workbook.xml.
    let wb_xml = parts(&input)["xl/workbook.xml"].clone().replace("</sheets>", r#"</sheets><definedNames><definedName name="Ответственный">"Иванов Иван Иванович"</definedName></definedNames>"#);
    rezip(&input, "xl/workbook.xml", &wb_xml);
    let rep = go(&input, &output, &prof(vec![Rule::new("Л", 0, 2, None, Kind::FullName)]));
    assert_absent(&output, &["Иванов Иван Иванович", "Петров Пётр Петрович"]);
    assert!(rep.leaks.is_empty(), "{:?}", rep.leaks);
}

/// Заменяет одну часть в готовом xlsx.
fn rezip(path: &Path, name: &str, data: &str) {
    use std::io::Write;
    let mut src = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut files = Vec::new();
    for i in 0..src.len() {
        let mut f = src.by_index(i).unwrap();
        let mut v = Vec::new();
        f.read_to_end(&mut v).unwrap();
        files.push((f.name().to_string(), v));
    }
    drop(src);
    let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    for (n, v) in files {
        z.start_file(n.clone(), zip::write::SimpleFileOptions::default()).unwrap();
        z.write_all(if n == name { data.as_bytes() } else { &v }).unwrap();
    }
    z.finish().unwrap();
}

// 8, 9 ------------------------------------------------------------- автоподбор

#[test]
fn r08_header_with_date_is_detected() {
    let d = tmp("r08");
    let input = d.join("in.xlsx");
    let mut b = Book::default();
    b.sheet("Л").extend([(1, 0, s("№")), (1, 1, s("Сотрудники на 01.10.2024")), (1, 2, s("Сумма")), (2, 0, Cell::N(1.0)), (2, 1, s("Иванов Иван")), (2, 2, Cell::N(5.0))]);
    write_xlsx(&input, &b);
    assert_eq!(detect_all(&input), ["B2:B… FullName"]);
}

#[test]
fn r09_first_value_looking_like_header_is_data() {
    let d = tmp("r09");
    let input = d.join("in.xlsx");
    let mut b = Book::default();
    b.sheet("Л").extend([
        (1, 0, s("Организация")), (1, 1, s("Сумма")),
        (2, 0, s("Компания Вектор")), (2, 1, Cell::N(100.0)),
        (3, 0, s("ООО Ромашка")), (3, 1, Cell::N(200.0)),
    ]);
    write_xlsx(&input, &b);
    assert_eq!(detect_all(&input), ["A2:A… Company"]);
}

// 10 --------------------------------------------------------------- имя в одном фрагменте — остальные не трогаем

#[test]
fn r10_runs_merged_only_when_name_is_split() {
    let d = tmp("r10");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    let mut b = Book::default();
    b.sheet("Л").extend([(1, 0, s("ФИО")), (2, 0, s("Иванов Иван Иванович"))]);
    b.extra.push(("xl/comments1.xml".into(), format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><comments {NS}><authors><author>А</author></authors><commentList><comment ref="B2" authorId="0"><text><r><rPr><b/></rPr><t>Иванов Иван Иванович:</t></r><r><t xml:space="preserve"> перезвонить завтра</t></r></text></comment></commentList></comments>"#)));
    write_xlsx(&input, &b);
    go(&input, &output, &prof(vec![Rule::new("Л", 0, 2, None, Kind::FullName)]));
    let c = parts(&output)["xl/comments1.xml"].clone();
    assert!(c.contains(r#"<t xml:space="preserve"> перезвонить завтра</t>"#), "обычный фрагмент стал частью жирного: {c}");
    assert!(!c.contains("Иванов Иван Иванович"));
}

// 11 --------------------------------------------------------------- длинная цепочка формул

#[test]
fn r11_long_formula_chain_is_fast() {
    let d = tmp("r11");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    let n = 30_000u32;
    let mut b = Book::default();
    let c = b.sheet("Л");
    c.push((1, 0, s("ФИО")));
    c.push((2, 0, s("Иванов Иван Иванович")));
    // C2 = C3, C3 = C4, …, последняя = A2: зависимость доходит до ПДн только через всю цепочку.
    for r in 2..=n {
        let f = if r == n { "A2".to_string() } else { format!("C{}", r + 1) };
        c.push((r, 2, Cell::FS(f, "Иванов Иван Иванович".into())));
    }
    write_xlsx(&input, &b);
    let t = Instant::now();
    go(&input, &output, &prof(vec![Rule::new("Л", 0, 2, Some(2), Kind::FullName)]));
    let secs = t.elapsed().as_secs_f64();
    assert!(secs < 20.0, "цепочка из {n} формул: {secs:.1} с");
    assert_absent(&output, &["Иванов Иван Иванович"]);
}
