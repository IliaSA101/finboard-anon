//! Охота на пробелы: несколько файлов заказчика, общие фейки, проверка остатков.

mod common;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use common::*;
use finboard_anon::anon::{preview_batch, run, run_batch, Batch, FileJob, Job, Report};
use finboard_anon::profile::{Kind, Profile, Rule, Settings};

fn settings(salt: &str) -> Settings {
    Settings { salt: salt.into(), ..Settings::default() }
}

fn rule(sheet: &str, col: u32, kind: Kind) -> Rule {
    Rule::new(sheet, col, 2, None, kind)
}

fn batch(files: Vec<(PathBuf, PathBuf, Vec<Rule>)>, key_out: Option<PathBuf>) -> Vec<Report> {
    let b = Batch {
        files: files.into_iter().map(|(input, output, rules)| FileJob { input, output, rules }).collect(),
        settings: settings("h3"),
        key_in: None,
        key_out,
    };
    run_batch(&b, &mut |_| {}).unwrap()
}

fn parts(path: &Path) -> HashMap<String, String> {
    let f = std::fs::File::open(path).unwrap();
    let mut z = zip::ZipArchive::new(f).unwrap();
    let mut out = HashMap::new();
    for i in 0..z.len() {
        let mut e = z.by_index(i).unwrap();
        let mut s = String::new();
        if e.read_to_string(&mut s).is_ok() {
            out.insert(e.name().to_string(), s);
        }
    }
    out
}

fn all_xml(path: &Path) -> String {
    let mut v: Vec<_> = parts(path).into_iter().collect();
    v.sort();
    v.into_iter().map(|(k, s)| format!("== {k}\n{s}\n")).collect()
}

fn leak_samples(r: &Report) -> Vec<String> {
    r.leaks.iter().flat_map(|l| l.samples.clone()).collect()
}

/// Перезаписывает одну часть zip.
fn rezip(path: &Path, name: &str, data: &str) {
    let bytes = std::fs::read(path).unwrap();
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let f = std::fs::File::create(path).unwrap();
    let mut w = zip::ZipWriter::new(f);
    let o = zip::write::SimpleFileOptions::default();
    for i in 0..z.len() {
        let mut e = z.by_index(i).unwrap();
        let n = e.name().to_string();
        let mut d = Vec::new();
        e.read_to_end(&mut d).unwrap();
        w.start_file(n.clone(), o).unwrap();
        if n == name {
            w.write_all(data.as_bytes()).unwrap();
        } else {
            w.write_all(&d).unwrap();
        }
    }
    w.finish().unwrap();
}

/// Файл А: ИНН в правиле, написанный текстом с пробелами. Файл Б: тот же ИНН числом в колонке без правила.
#[test]
fn g01_inn_text_with_spaces_in_rule_number_elsewhere() {
    let d = tmp("g01");
    let mut a = Book::default();
    let sh = a.sheet("Контрагенты");
    sh.push((1, 0, s("ИНН")));
    sh.push((2, 0, s("77 07 083893")));
    let mut b = Book::default();
    let sh = b.sheet("Платежи");
    sh.push((1, 0, s("Сумма")));
    sh.push((1, 1, s("Код")));
    sh.push((2, 0, Cell::N(100.0)));
    sh.push((2, 1, Cell::N(7707083893.0)));
    sh.push((3, 1, s("7707083893")));
    let (ia, ib) = (d.join("a.xlsx"), d.join("b.xlsx"));
    write_xlsx(&ia, &a);
    write_xlsx(&ib, &b);
    let (oa, ob) = (d.join("a_anon.xlsx"), d.join("b_anon.xlsx"));
    let reps = batch(
        vec![(ia, oa.clone(), vec![rule("Контрагенты", 0, Kind::Inn)]), (ib, ob.clone(), vec![rule("Платежи", 0, Kind::Text)])],
        None,
    );
    let ga = read_book(&oa);
    let gb = read_book(&ob);
    let fa = text(&ga, "Контрагенты", "A2");
    let b2 = text(&gb, "Платежи", "B2");
    let b3 = text(&gb, "Платежи", "B3");
    eprintln!("A: {fa}; B2: {b2}; B3: {b3}; leaks B: {:?}", reps[1].leaks);
    let _ = leak_samples;
    let dg = |s: &str| s.chars().filter(|c| c.is_ascii_digit()).collect::<String>();
    assert_eq!(b2, dg(&fa), "ИНН числом в колонке без правила — те же фейковые цифры");
    assert_eq!(b3, dg(&fa), "ИНН текстом в колонке без правила — те же фейковые цифры");
    assert!(gb["Платежи"].get(2, 1).unwrap().numeric, "число осталось числом");
    assert!(reps[1].leaks.is_empty(), "{:?}", reps[1].leaks);
}

/// Обратное: ИНН числом под правилом в А; в Б — текстом с пробелами в колонке без правила.
#[test]
fn g02_inn_number_in_rule_text_with_spaces_elsewhere() {
    let d = tmp("g02");
    let mut a = Book::default();
    let sh = a.sheet("Контрагенты");
    sh.push((1, 0, s("ИНН")));
    sh.push((2, 0, Cell::N(7707083893.0)));
    let mut b = Book::default();
    let sh = b.sheet("Платежи");
    sh.push((1, 0, s("Сумма")));
    sh.push((1, 1, s("Реквизиты")));
    sh.push((2, 0, Cell::N(1.0)));
    sh.push((2, 1, s("77 07 083893")));
    sh.push((3, 1, s("ИНН 7707 083893 КПП 770701001")));
    let (ia, ib) = (d.join("a.xlsx"), d.join("b.xlsx"));
    write_xlsx(&ia, &a);
    write_xlsx(&ib, &b);
    let (oa, ob) = (d.join("a_anon.xlsx"), d.join("b_anon.xlsx"));
    let reps = batch(
        vec![(ia, oa.clone(), vec![rule("Контрагенты", 0, Kind::Inn)]), (ib, ob.clone(), vec![rule("Платежи", 0, Kind::Text)])],
        None,
    );
    let gb = read_book(&ob);
    let b2 = text(&gb, "Платежи", "B2");
    let b3 = text(&gb, "Платежи", "B3");
    eprintln!("B2: {b2}; B3: {b3}; leaks B: {:?}", reps[1].leaks);
    let digits = |s: &str| s.chars().filter(|c| c.is_ascii_digit()).collect::<String>();
    let fa = digits(&text(&read_book(&oa), "Контрагенты", "A2"));
    assert_eq!(b2, format!("{} {} {}", &fa[..2], &fa[2..4], &fa[4..]), "формат с пробелами сохранён");
    assert!(b3.starts_with(&format!("ИНН {} {} КПП", &fa[..4], &fa[4..])), "{b3}");
    assert!(b3.ends_with("КПП 770701001"), "КПП не ИНН — не трогаем: {b3}");
    assert!(reps[1].leaks.is_empty(), "{:?}", reps[1].leaks);
}

/// ИНН региона 01–09: текстом «0274062111» в одном файле, числом 274062111 (Excel срезал ноль) в другом —
/// оба под правилом ИНН. Ожидается один фейк.
#[test]
fn g03_inn_leading_zero_number_vs_text() {
    let d = tmp("g03");
    let mut a = Book::default();
    let sh = a.sheet("Л");
    sh.push((1, 0, s("ИНН")));
    sh.push((2, 0, s("0274062111")));
    let mut b = Book::default();
    let sh = b.sheet("Л");
    sh.push((1, 0, s("ИНН")));
    sh.push((2, 0, Cell::N(274062111.0)));
    let (ia, ib) = (d.join("a.xlsx"), d.join("b.xlsx"));
    write_xlsx(&ia, &a);
    write_xlsx(&ib, &b);
    let (oa, ob) = (d.join("a_anon.xlsx"), d.join("b_anon.xlsx"));
    let mut r = rule("Л", 0, Kind::Inn);
    r.with_id = false;
    batch(vec![(ia, oa.clone(), vec![r.clone()]), (ib, ob.clone(), vec![r])], None);
    let fa = text(&read_book(&oa), "Л", "A2");
    let fb = text(&read_book(&ob), "Л", "A2");
    eprintln!("A: {fa}  B: {fb}");
    let da: String = fa.chars().filter(|c| c.is_ascii_digit()).collect();
    let db: String = fb.chars().filter(|c| c.is_ascii_digit()).collect();
    assert_eq!(da.trim_start_matches('0'), db.trim_start_matches('0'), "один ИНН — разные фейки: {fa} / {fb}");
}

/// ИНН региона 01–09 текстом под правилом в одном файле; в другом — числом без ведущего нуля в колонке без правила.
#[test]
fn g03b_inn_leading_zero_number_unruled() {
    let d = tmp("g03b");
    let mut a = Book::default();
    let sh = a.sheet("Л");
    sh.push((1, 0, s("ИНН")));
    sh.push((2, 0, s("0274062111")));
    let mut b = Book::default();
    let sh = b.sheet("Л");
    sh.push((1, 0, s("Сумма")));
    sh.push((1, 1, s("Код")));
    sh.push((2, 0, Cell::N(1.0)));
    sh.push((2, 1, Cell::N(274062111.0)));
    let (ia, ib) = (d.join("a.xlsx"), d.join("b.xlsx"));
    write_xlsx(&ia, &a);
    write_xlsx(&ib, &b);
    let (oa, ob) = (d.join("a_anon.xlsx"), d.join("b_anon.xlsx"));
    let reps = batch(vec![(ia, oa.clone(), vec![rule("Л", 0, Kind::Inn)]), (ib, ob.clone(), vec![rule("Л", 0, Kind::Text)])], None);
    let fa = text(&read_book(&oa), "Л", "A2");
    assert_eq!(text(&read_book(&ob), "Л", "B2"), fa, "ИНН без ведущего нуля в другом файле");
    assert!(reps[1].leaks.is_empty(), "{:?}", reps[1].leaks);
}

/// Организация в правиле файла А; в файле Б (без правила на эту колонку) — то же название ЗАГЛАВНЫМИ
/// в назначении платежа (как в банковских выписках).
#[test]
fn g04_company_uppercase_in_free_text_other_file() {
    let d = tmp("g04");
    let mut a = Book::default();
    let sh = a.sheet("Договоры");
    sh.push((1, 0, s("Контрагент")));
    sh.push((2, 0, s("ООО «Ромашка»")));
    let mut b = Book::default();
    let sh = b.sheet("Выписка");
    sh.push((1, 0, s("Контрагент")));
    sh.push((1, 1, s("Назначение платежа")));
    sh.push((2, 0, s("ООО \"РОМАШКА\"")));
    sh.push((2, 1, s("Оплата по счету 15 от ООО \"РОМАШКА\", без НДС")));
    let (ia, ib) = (d.join("a.xlsx"), d.join("b.xlsx"));
    write_xlsx(&ia, &a);
    write_xlsx(&ib, &b);
    let (oa, ob) = (d.join("a_anon.xlsx"), d.join("b_anon.xlsx"));
    let reps = batch(
        vec![(ia, oa.clone(), vec![rule("Договоры", 0, Kind::Company)]), (ib, ob.clone(), vec![rule("Выписка", 0, Kind::Company)])],
        None,
    );
    let gb = read_book(&ob);
    let a2 = text(&gb, "Выписка", "A2");
    let b2 = text(&gb, "Выписка", "B2");
    eprintln!("A2: {a2}; B2: {b2}; leaks: {:?}", reps[1].leaks);
    assert!(!b2.to_lowercase().contains("ромашка"), "реальное название осталось в назначении платежа: {b2}");
    assert!(reps[1].leaks.is_empty(), "{:?}", reps[1].leaks);
}

/// ИП в колонке контрагентов (правило «Организация»); ФИО этого человека отдельно — в тексте/колонке без правила.
#[test]
fn g05_ip_person_name_elsewhere() {
    let d = tmp("g05");
    let mut a = Book::default();
    let sh = a.sheet("Договоры");
    sh.push((1, 0, s("Контрагент")));
    sh.push((1, 1, s("Подписант")));
    sh.push((2, 0, s("ИП Козлов Олег Петрович")));
    sh.push((2, 1, s("Козлов Олег Петрович")));
    sh.push((4, 0, s("Ответственный: Козлов Олег Петрович")));
    let ia = d.join("a.xlsx");
    write_xlsx(&ia, &a);
    let oa = d.join("a_anon.xlsx");
    let reps = batch(vec![(ia, oa.clone(), vec![Rule::new("Договоры", 0, 2, Some(2), Kind::Company)])], None);
    let g = read_book(&oa);
    let (a2, b2, a4) = (text(&g, "Договоры", "A2"), text(&g, "Договоры", "B2"), text(&g, "Договоры", "A4"));
    eprintln!("A2: {a2}; B2: {b2}; A4: {a4}; leaks: {:?}", reps[0].leaks);
    let ip = a2.strip_prefix("ИП ").unwrap().rsplit_once(" (ID").map_or(a2.as_str(), |x| x.0).to_string();
    assert_eq!(b2, ip, "ФИО ИП в колонке без правила — то же фейковое ФИО");
    assert_eq!(a4, format!("Ответственный: {ip}"));
    assert!(reps[0].leaks.is_empty(), "{:?}", reps[0].leaks);
}

/// Лист назван по контрагенту, у листа область печати (definedName) и диаграмма со ссылкой на лист.
/// Имя листа не меняется, а ссылки на него переписываются на фейк — ссылки ведут в никуда.
#[test]
fn g06_sheet_named_after_company_references_broken() {
    let d = tmp("g06");
    let mut a = Book::default();
    let sh = a.sheet("Контрагенты");
    sh.push((1, 0, s("Контрагент")));
    sh.push((2, 0, s("ООО «Ромашка»")));
    let sh = a.sheet("Ромашка");
    sh.push((1, 0, s("Месяц")));
    sh.push((1, 1, s("Сумма")));
    sh.push((2, 0, s("Январь")));
    sh.push((2, 1, Cell::N(10.0)));
    a.sheet_tail.insert(
        1,
        r#"<conditionalFormatting sqref="B2"><cfRule type="expression" dxfId="0" priority="1"><formula>'Ромашка'!$B$2&gt;5</formula></cfRule></conditionalFormatting>"#.into(),
    );
    a.extra.push((
        "xl/charts/chart1.xml".into(),
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart><c:plotArea><c:barChart><c:ser><c:idx val="0"/><c:val><c:numRef><c:f>'Ромашка'!$B$2:$B$2</c:f></c:numRef></c:val></c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>"#.into(),
    ));
    let ia = d.join("a.xlsx");
    write_xlsx(&ia, &a);
    // Область печати листа «Ромашка».
    let wb = parts(&ia)["xl/workbook.xml"].replace(
        "</sheets>",
        r#"</sheets><definedNames><definedName name="_xlnm.Print_Area" localSheetId="1">'Ромашка'!$A$1:$B$2</definedName></definedNames>"#,
    );
    rezip(&ia, "xl/workbook.xml", &wb);
    let oa = d.join("a_anon.xlsx");
    batch(vec![(ia, oa.clone(), vec![rule("Контрагенты", 0, Kind::Company)])], None);
    let p = parts(&oa);
    let wbx = &p["xl/workbook.xml"];
    let chart = &p["xl/charts/chart1.xml"];
    let sheet2 = &p["xl/worksheets/sheet2.xml"];
    eprintln!("workbook: {wbx}\nchart: {chart}\nsheet2: {sheet2}");
    let sheet_still = wbx.contains(r#"<sheet name="Ромашка""#);
    let name_ok = wbx.contains("'Ромашка'!$A$1:$B$2");
    let chart_ok = chart.contains("'Ромашка'!$B$2:$B$2");
    let cf_ok = sheet2.contains("'Ромашка'!$B$2");
    assert!(
        !sheet_still || (name_ok && chart_ok && cf_ok),
        "лист «Ромашка» остался, а ссылки на него переписаны: область печати {name_ok}, диаграмма {chart_ok}, усл. формат {cf_ok}"
    );
}

/// Предпросмотр пакета против результата: организация в двух написаниях в двух файлах, ИНН в двух видах.
#[test]
fn g07_preview_matches_output_mixed_spellings() {
    let d = tmp("g07");
    let mut a = Book::default();
    let sh = a.sheet("А");
    sh.push((1, 0, s("Контрагент")));
    sh.push((1, 1, s("ИНН")));
    sh.push((2, 0, s("ООО «РОМАШКА»")));
    sh.push((2, 1, s("77 07 083893")));
    sh.push((3, 0, s("ИП Козлов Олег Петрович")));
    sh.push((3, 1, Cell::N(500100732259.0)));
    let mut b = Book::default();
    let sh = b.sheet("Б");
    sh.push((1, 0, s("Плательщик")));
    sh.push((1, 1, s("ИНН")));
    sh.push((2, 0, s("Ромашка, ООО")));
    sh.push((2, 1, Cell::N(7707083893.0)));
    sh.push((3, 0, s("Козлов О.П.")));
    sh.push((3, 1, s("500100732259")));
    let (ia, ib) = (d.join("a.xlsx"), d.join("b.xlsx"));
    write_xlsx(&ia, &a);
    write_xlsx(&ib, &b);
    let ra = vec![rule("А", 0, Kind::Company), rule("А", 1, Kind::Inn)];
    let rb = vec![rule("Б", 0, Kind::FullName), rule("Б", 1, Kind::Inn)];
    let rb = {
        let mut r = rb;
        r[0] = Rule::new("Б", 0, 3, Some(3), Kind::FullName);
        r.push(Rule::new("Б", 0, 2, Some(2), Kind::Company));
        r
    };
    let pv = preview_batch(&[(ia.clone(), ra.clone()), (ib.clone(), rb.clone())], &settings("h3"), None).unwrap();
    let (oa, ob) = (d.join("a_anon.xlsx"), d.join("b_anon.xlsx"));
    batch(vec![(ia, oa.clone(), ra), (ib, ob.clone(), rb)], None);
    let ga = read_book(&oa);
    let gb = read_book(&ob);
    let out: Vec<String> = ["А!A2", "А!B2", "А!A3", "А!B3"]
        .iter()
        .map(|x| text(&ga, "А", &x[x.find('!').unwrap() + 1..]))
        .chain(["A2", "B2", "A3", "B3"].iter().map(|x| text(&gb, "Б", x)))
        .collect();
    eprintln!("out: {out:?}\npreview: {pv:?}");
    for f in &pv {
        for r in f {
            for (_, fake) in &r.samples {
                assert!(out.contains(fake), "фейк из предпросмотра {fake} не попал в файлы {out:?}");
            }
        }
    }
    let dg = |s: &str| s.chars().filter(|c| c.is_ascii_digit()).collect::<String>();
    assert_eq!(dg(&out[1]), dg(&out[5]), "ИНН 7707083893 текстом и числом");
    assert_eq!(dg(&out[3]), dg(&out[7]), "ИНН 500100732259 числом и текстом");
}

/// Последовательные прогоны с общим ключом: название ЗАГЛАВНЫМИ в первом файле, обычным регистром во втором.
#[test]
fn g08_sequential_case_variants_same_fake() {
    let d = tmp("g08");
    let mut a = Book::default();
    let sh = a.sheet("Л");
    sh.push((1, 0, s("Контрагент")));
    sh.push((2, 0, s("ООО «РОМАШКА»")));
    let mut b = Book::default();
    let sh = b.sheet("Л");
    sh.push((1, 0, s("Контрагент")));
    sh.push((2, 0, s("ООО «Ромашка»")));
    let (ia, ib) = (d.join("a.xlsx"), d.join("b.xlsx"));
    write_xlsx(&ia, &a);
    write_xlsx(&ib, &b);
    let key = d.join("key.csv");
    let mut r = rule("Л", 0, Kind::Company);
    r.with_id = false;
    let job = |i: &Path, o: &Path, kin: Option<PathBuf>| Job {
        input: i.to_path_buf(),
        output: o.to_path_buf(),
        key_in: kin,
        key_out: Some(key.clone()),
        profile: Profile { rules: vec![r.clone()], settings: settings("h3") },
    };
    let (oa, ob) = (d.join("a_anon.xlsx"), d.join("b_anon.xlsx"));
    run(&job(&ia, &oa, None), &mut |_| {}).unwrap();
    run(&job(&ib, &ob, Some(key.clone())), &mut |_| {}).unwrap();
    let seq_b = text(&read_book(&ob), "Л", "A2");
    // Тот же набор пакетом.
    let (pa, pb) = (d.join("pa.xlsx"), d.join("pb.xlsx"));
    let bt = Batch {
        files: vec![
            FileJob { input: ia.clone(), output: pa.clone(), rules: vec![r.clone()] },
            FileJob { input: ib.clone(), output: pb.clone(), rules: vec![r.clone()] },
        ],
        settings: settings("h3"),
        key_in: None,
        key_out: None,
    };
    run_batch(&bt, &mut |_| {}).unwrap();
    let batch_b = text(&read_book(&pb), "Л", "A2");
    let seq_a = text(&read_book(&oa), "Л", "A2");
    eprintln!("seq: {seq_a} / {seq_b}; batch b: {batch_b}");
    assert_eq!(seq_b, batch_b, "одни и те же файлы: по очереди и пакетом дают разное написание фейка");
}

/// Ключ, полученный из обезличенного файла: повторный прогон обезличенного результата с общим ключом
/// не должен «обезличивать» фейки (иначе в ключе фейк → новый фейк, и связь с исходником теряется).
#[test]
fn g09_rerun_on_output_with_shared_key() {
    let d = tmp("g09");
    let mut a = Book::default();
    let sh = a.sheet("Л");
    sh.push((1, 0, s("Контрагент")));
    sh.push((1, 1, s("ИНН")));
    sh.push((2, 0, s("ООО «Ромашка»")));
    sh.push((2, 1, Cell::N(7707083893.0)));
    let ia = d.join("a.xlsx");
    write_xlsx(&ia, &a);
    let key = d.join("key.csv");
    let rules = vec![rule("Л", 0, Kind::Company), rule("Л", 1, Kind::Inn)];
    let job = |i: &Path, o: &Path, kin: Option<PathBuf>| Job {
        input: i.to_path_buf(),
        output: o.to_path_buf(),
        key_in: kin,
        key_out: Some(key.clone()),
        profile: Profile { rules: rules.clone(), settings: settings("h3") },
    };
    let oa = d.join("a_anon.xlsx");
    run(&job(&ia, &oa, None), &mut |_| {}).unwrap();
    let first = (text(&read_book(&oa), "Л", "A2"), text(&read_book(&oa), "Л", "B2"));
    let oa2 = d.join("a_anon_anon.xlsx");
    run(&job(&oa, &oa2, Some(key.clone())), &mut |_| {}).unwrap();
    let second = (text(&read_book(&oa2), "Л", "A2"), text(&read_book(&oa2), "Л", "B2"));
    let k = std::fs::read_to_string(&key).unwrap();
    eprintln!("first {first:?} second {second:?}\nkey:\n{k}");
    assert_eq!(first, second, "повторный прогон по результату меняет фейки");
}

/// ИНН числом под правилом в формуле-кэше в другом файле (ВПР по коду возвращает ИНН числом).
#[test]
fn g10_inn_number_in_formula_cache_other_file() {
    let d = tmp("g10");
    let mut a = Book::default();
    let sh = a.sheet("Л");
    sh.push((1, 0, s("ИНН")));
    sh.push((2, 0, Cell::N(7707083893.0)));
    let mut b = Book::default();
    let sh = b.sheet("Л");
    sh.push((1, 0, s("Код")));
    sh.push((1, 1, s("ИНН по справочнику")));
    sh.push((2, 0, Cell::N(1.0)));
    sh.push((2, 1, Cell::F("VLOOKUP(A2,[1]Справочник!$A:$B,2,0)".into(), 7707083893.0)));
    let (ia, ib) = (d.join("a.xlsx"), d.join("b.xlsx"));
    write_xlsx(&ia, &a);
    write_xlsx(&ib, &b);
    let (oa, ob) = (d.join("a_anon.xlsx"), d.join("b_anon.xlsx"));
    let reps = batch(vec![(ia, oa, vec![rule("Л", 0, Kind::Inn)]), (ib, ob.clone(), vec![rule("Л", 0, Kind::Text)])], None);
    let x = all_xml(&ob);
    eprintln!("leaks: {:?}", reps[1].leaks);
    assert!(!x.contains("7707083893"), "реальный ИНН в кэше формулы");
    assert!(reps[1].leaks.is_empty(), "{:?}", reps[1].leaks);
}

/// Как g04, но в файле Б колонка контрагента без правила: название ЗАГЛАВНЫМИ (выписка банка).
#[test]
fn g04b_company_uppercase_unruled_other_file() {
    let d = tmp("g04b");
    let mut a = Book::default();
    let sh = a.sheet("Договоры");
    sh.push((1, 0, s("Контрагент")));
    sh.push((2, 0, s("ООО «Ромашка»")));
    let mut b = Book::default();
    let sh = b.sheet("Выписка");
    sh.push((1, 0, s("Сумма")));
    sh.push((1, 1, s("Назначение платежа")));
    sh.push((2, 0, Cell::N(5.0)));
    sh.push((2, 1, s("Оплата по счету 15, ООО \"РОМАШКА\", без НДС")));
    let (ia, ib) = (d.join("a.xlsx"), d.join("b.xlsx"));
    write_xlsx(&ia, &a);
    write_xlsx(&ib, &b);
    let (oa, ob) = (d.join("a_anon.xlsx"), d.join("b_anon.xlsx"));
    let reps = batch(
        vec![(ia, oa, vec![rule("Договоры", 0, Kind::Company)]), (ib, ob.clone(), vec![rule("Выписка", 0, Kind::Text)])],
        None,
    );
    let b2 = text(&read_book(&ob), "Выписка", "B2");
    eprintln!("B2: {b2}; leaks: {:?}", reps[1].leaks);
    assert!(!b2.contains("РОМАШКА"), "реальное название осталось: {b2}");
    // Организация внутри фразы — всегда с ID, тем же, что в колонке; ID после закрывающей кавычки.
    assert!(b2.starts_with("Оплата по счету 15, ООО \"") && b2.ends_with("\" (ID 1), без НДС"), "{b2}");
    assert!(reps[1].leaks.is_empty(), "{:?}", reps[1].leaks);
}

/// ФИО ЗАГЛАВНЫМИ в тексте другого файла (колонка без правила).
#[test]
fn g11_person_uppercase_unruled_other_file() {
    let d = tmp("g11");
    let mut a = Book::default();
    let sh = a.sheet("Л");
    sh.push((1, 0, s("ФИО")));
    sh.push((2, 0, s("Иванов Иван Иванович")));
    let mut b = Book::default();
    let sh = b.sheet("Л");
    sh.push((1, 0, s("Сумма")));
    sh.push((1, 1, s("Получатель")));
    sh.push((2, 0, Cell::N(5.0)));
    sh.push((2, 1, s("ИВАНОВ ИВАН ИВАНОВИЧ")));
    let (ia, ib) = (d.join("a.xlsx"), d.join("b.xlsx"));
    write_xlsx(&ia, &a);
    write_xlsx(&ib, &b);
    let (oa, ob) = (d.join("a_anon.xlsx"), d.join("b_anon.xlsx"));
    let reps = batch(vec![(ia, oa, vec![rule("Л", 0, Kind::FullName)]), (ib, ob.clone(), vec![rule("Л", 0, Kind::Text)])], None);
    let b2 = text(&read_book(&ob), "Л", "B2");
    eprintln!("B2: {b2}; leaks: {:?}", reps[1].leaks);
    let fa = text(&read_book(&d.join("a_anon.xlsx")), "Л", "A2");
    assert_eq!(b2, fa.to_uppercase(), "ФИО заглавными — тот же фейк заглавными");
    assert!(reps[1].leaks.is_empty(), "{:?}", reps[1].leaks);
}

/// Сбой на втором файле пакета (папки результата нет): первый результат уже записан, ключа нет.
#[test]
fn g12_batch_failure_midway_leaves_output_without_key() {
    let d = tmp("g12");
    let mut a = Book::default();
    let sh = a.sheet("Л");
    sh.push((1, 0, s("ФИО")));
    sh.push((2, 0, s("Иванов Иван Иванович")));
    let (ia, ib) = (d.join("a.xlsx"), d.join("b.xlsx"));
    write_xlsx(&ia, &a);
    write_xlsx(&ib, &a);
    let (oa, ob) = (d.join("a_anon.xlsx"), d.join("нет_папки").join("b_anon.xlsx"));
    let key = d.join("key.csv");
    let b = Batch {
        files: vec![
            FileJob { input: ia, output: oa.clone(), rules: vec![rule("Л", 0, Kind::FullName)] },
            FileJob { input: ib, output: ob, rules: vec![rule("Л", 0, Kind::FullName)] },
        ],
        settings: settings("h3"),
        key_in: None,
        key_out: Some(key.clone()),
    };
    let res = run_batch(&b, &mut |_| {});
    eprintln!("res err: {:?}; a_anon exists {}; key exists {}", res.as_ref().err(), oa.exists(), key.exists());
    assert!(res.is_err());
    assert!(!oa.exists() || key.exists(), "a_anon.xlsx записан, а ключ к нему — нет");
    // Папки проверяются до записи: пакет не записывается наполовину.
    assert!(!oa.exists(), "при сбое пакета записана часть файлов");
}

/// Как g10, но ячейка-аргумент ВПР без правила: кэш не «производный», заменяется как известное число.
#[test]
fn g10b_inn_in_formula_cache_argument_unruled() {
    let d = tmp("g10b");
    let mut a = Book::default();
    let sh = a.sheet("Л");
    sh.push((1, 0, s("ИНН")));
    sh.push((2, 0, Cell::N(7707083893.0)));
    let mut b = Book::default();
    let sh = b.sheet("Л");
    sh.push((1, 0, s("Код")));
    sh.push((1, 1, s("ИНН по справочнику")));
    sh.push((1, 2, s("Ответственный")));
    sh.push((2, 0, Cell::N(1.0)));
    sh.push((2, 1, Cell::F("VLOOKUP(A2,[1]Справочник!$A:$B,2,0)".into(), 7707083893.0)));
    sh.push((2, 2, s("Петрова Анна Сергеевна")));
    let (ia, ib) = (d.join("a.xlsx"), d.join("b.xlsx"));
    write_xlsx(&ia, &a);
    write_xlsx(&ib, &b);
    let (oa, ob) = (d.join("a_anon.xlsx"), d.join("b_anon.xlsx"));
    let reps = batch(vec![(ia, oa, vec![rule("Л", 0, Kind::Inn)]), (ib, ob.clone(), vec![rule("Л", 2, Kind::FullName)])], None);
    assert!(!all_xml(&ob).contains("7707083893"), "реальный ИНН в кэше формулы");
    assert!(reps[1].leaks.is_empty(), "{:?}", reps[1].leaks);
}

/// Страж не пустой: проверка остатков сама видит номер в другом виде и ФИО/организацию ЗАГЛАВНЫМИ.
/// Замена их уже убирает, поэтому без этого теста ослабленная проверка прошла бы незамеченной.
#[test]
fn leak_scan_sees_other_formats() {
    let d = tmp("g_scan");
    let originals = vec!["7707083893".to_string(), "Иванов Иван Иванович".to_string(), "ООО «Ромашка»".to_string()];
    let digits: std::collections::HashSet<String> = ["7707083893".to_string()].into();
    for (name, planted, expect) in [
        ("num", "ИНН 77 07 083893 в примечании", "7707083893"),
        ("fio", "ИВАНОВ ИВАН ИВАНОВИЧ", "Иванов Иван Иванович"),
        ("org", "Оплата ООО «РОМАШКА»", "ООО «Ромашка»"),
    ] {
        let mut b = Book::default();
        b.sheet("Л").push((1, 0, s(planted)));
        let p = d.join(format!("{name}.xlsx"));
        write_xlsx(&p, &b);
        let leaks = finboard_anon::anon::scan_leaks_with(&p, &originals, &[], &digits).unwrap();
        assert!(!leaks.is_empty(), "проверка не видит «{planted}» ({expect})");
    }
}

/// Телефон в двух файлах под правилом «Телефон»: «+7 (916) 123-45-67» и «8 916 123 45 67», число 9161234567.
#[test]
fn g13_phone_formats_across_files() {
    let d = tmp("g13");
    let mut a = Book::default();
    let sh = a.sheet("Л");
    sh.push((1, 0, s("Телефон")));
    sh.push((2, 0, s("+7 (916) 123-45-67")));
    let mut b = Book::default();
    let sh = b.sheet("Л");
    sh.push((1, 0, s("Телефон")));
    sh.push((2, 0, s("8 916 123 45 67")));
    sh.push((3, 0, Cell::N(9161234567.0)));
    let (ia, ib) = (d.join("a.xlsx"), d.join("b.xlsx"));
    write_xlsx(&ia, &a);
    write_xlsx(&ib, &b);
    let mut out = Vec::new();
    for with_id in [true, false] {
        let mut r = rule("Л", 0, Kind::Phone);
        r.with_id = with_id;
        let (oa, ob) = (d.join(format!("a_{with_id}.xlsx")), d.join(format!("b_{with_id}.xlsx")));
        batch(vec![(ia.clone(), oa.clone(), vec![r.clone()]), (ib.clone(), ob.clone(), vec![r])], None);
        let ga = read_book(&oa);
        let gb = read_book(&ob);
        out.push((with_id, text(&ga, "Л", "A2"), text(&gb, "Л", "A2"), text(&gb, "Л", "A3")));
    }
    eprintln!("{out:?}");
    let tail = |s: &str| {
        let d: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
        d[d.len() - 10..].to_string()
    };
    for (w, a, b, c) in &out {
        assert!(tail(a) == tail(b) && tail(b) == tail(c), "with_id={w}: один телефон — разные фейки: {a} / {b} / {c}");
    }
}

/// Ключ прежней версии («<имя>_КЛЮЧ_НЕ_ПЕРЕДАВАТЬ.csv» рядом с файлом): окно его подхватывает (find_key),
/// консоль — нет: повторный запуск консолью даёт другие фейки, чем в уже переданном файле.
#[test]
fn g15_cli_ignores_legacy_key() {
    let d = tmp("g15");
    let mut a = Book::default();
    let sh = a.sheet("Л");
    sh.push((1, 0, s("ФИО")));
    sh.push((2, 0, s("Иванов Иван Иванович")));
    let ia = d.join("a.xlsx");
    write_xlsx(&ia, &a);
    let mut r = rule("Л", 0, Kind::FullName);
    r.with_id = false;
    let legacy = d.join("a_КЛЮЧ_НЕ_ПЕРЕДАВАТЬ.csv");
    let o1 = d.join("a_old.xlsx");
    run(
        &Job { input: ia.clone(), output: o1.clone(), key_in: None, key_out: Some(legacy.clone()), profile: Profile { rules: vec![r.clone()], settings: settings("old") } },
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(finboard_anon::anon::find_key(&ia).as_deref(), Some(legacy.as_path()), "окно подхватит прежний ключ");
    let p = d.join("p.json");
    Profile { rules: vec![r], settings: settings("x") }.save(&p).unwrap();
    let o2 = d.join("a_new.xlsx");
    let st = std::process::Command::new(env!("CARGO_BIN_EXE_finboard-anon-cli"))
        .args(["--profile", p.to_str().unwrap(), "--in", ia.to_str().unwrap(), "--out", o2.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(st.status.success(), "{}", String::from_utf8_lossy(&st.stderr));
    let (f1, f2) = (text(&read_book(&o1), "Л", "A2"), text(&read_book(&o2), "Л", "A2"));
    eprintln!("old: {f1}; cli: {f2}; stderr: {}", String::from_utf8_lossy(&st.stderr));
    assert_eq!(f1, f2, "консоль не подхватила прежний ключ файла");
}

/// Один и тот же файл дважды в пакете и пустая книга.
#[test]
fn g14_duplicate_and_empty_files() {
    let d = tmp("g14");
    let mut a = Book::default();
    let sh = a.sheet("Л");
    sh.push((1, 0, s("ФИО")));
    sh.push((2, 0, s("Иванов Иван Иванович")));
    let ia = d.join("a.xlsx");
    write_xlsx(&ia, &a);
    let mut e = Book::default();
    e.sheet("Л");
    let ie = d.join("e.xlsx");
    write_xlsx(&ie, &e);
    let r = vec![rule("Л", 0, Kind::FullName)];
    let reps = batch(
        vec![(ia.clone(), d.join("a1.xlsx"), r.clone()), (ia.clone(), d.join("a2.xlsx"), r.clone()), (ie, d.join("e1.xlsx"), r)],
        Some(d.join("k.csv")),
    );
    let (x, y) = (text(&read_book(&d.join("a1.xlsx")), "Л", "A2"), text(&read_book(&d.join("a2.xlsx")), "Л", "A2"));
    eprintln!("{x} / {y}; leaks {:?}", reps.iter().map(|r| r.leaks.len()).collect::<Vec<_>>());
    assert_eq!(x, y);
    assert!(read_book(&d.join("e1.xlsx")).contains_key("Л"));
}
