//! Доработки 0.2.0 по тестам клиента на реальных файлах: шаблон «подпись + номер» с подписью из заголовка,
//! настройка «Вне разметки», защищённые области, организация внутри фразы всегда с ID.
//! Каждый тест падал до исправления.

mod common;

use std::path::{Path, PathBuf};

use common::*;
use finboard_anon::anon::{run, Job, Report};
use finboard_anon::detect::{fill_template, template_label};
use finboard_anon::profile::{Kind, Outside, Profile, Rule, Settings};

fn go(input: &Path, output: &Path, rules: Vec<Rule>, settings: Settings) -> Report {
    let job = Job { input: input.into(), output: output.into(), key_in: None, key_out: None, profile: Profile { rules, settings } };
    run(&job, &mut |_| {}).unwrap()
}

fn settings(outside: Outside) -> Settings {
    Settings { salt: "fb".into(), outside, ..Settings::default() }
}

/// Колонки как у клиента: должности и налоги с заголовками.
fn staff_book() -> Book {
    let mut b = Book::default();
    b.sheet("Л").extend([
        (1, 0, s("Должность")),
        (2, 0, s("Бухгалтер")),
        (3, 0, s("Директор")),
        (4, 0, s("Бухгалтер")),
        (1, 1, s("Налог")),
        (2, 1, s("НДФЛ")),
        (3, 1, s("НДС")),
        (4, 1, s("ЕСН")),
        (1, 2, s("Сумма")),
        (2, 2, Cell::N(5.0)),
        (3, 2, Cell::N(7.0)),
        (4, 2, Cell::N(9.0)),
    ]);
    b
}

fn staff(name: &str) -> (PathBuf, PathBuf) {
    let d = tmp(name);
    let input = d.join("in.xlsx");
    write_xlsx(&input, &staff_book());
    (input, d.join("out.xlsx"))
}

/// Подпись берётся из заголовка над диапазоном; шапка, захваченная выделением, не заменяется и становится
/// подписью. У каждой подписи своя нумерация: «Налог 1» не продолжает «Должность 2».
#[test]
fn t01_template_label_from_header() {
    let (input, output) = staff("t01");
    let g = read_book(&input);
    let mut pos = Rule::new("Л", 0, 2, None, Kind::Text);
    let mut tax = Rule::new("Л", 1, 1, Some(4), Kind::Text);
    assert!(fill_template(&mut pos, &g["Л"]).is_some());
    assert!(fill_template(&mut tax, &g["Л"]).is_some());
    assert_eq!((pos.label.as_str(), pos.row_from), ("Должность", 2));
    assert_eq!((tax.label.as_str(), tax.row_from), ("Налог", 2), "шапка в диапазоне — не данные");
    // Подпись, вписанная пользователем, важнее заголовка.
    let mut own = Rule::new("Л", 0, 2, None, Kind::Text);
    own.label = "Роль".into();
    assert_eq!(fill_template(&mut own, &g["Л"]), None);
    assert_eq!(own.label, "Роль");

    go(&input, &output, vec![pos, tax], settings(Outside::All));
    let o = read_book(&output);
    let t = |c: &str| text(&o, "Л", c);
    assert_eq!([t("A1"), t("A2"), t("A3"), t("A4")], ["Должность", "Должность 1", "Должность 2", "Должность 1"]);
    assert_eq!([t("B1"), t("B2"), t("B3"), t("B4")], ["Налог", "Налог 1", "Налог 2", "Налог 3"]);
}

/// Подпись задана вручную, а шапка попала в диапазон: шапка остаётся как есть и не занимает номер.
#[test]
fn t02_header_inside_range_with_manual_label() {
    let (input, output) = staff("t02");
    let mut tax = Rule::new("Л", 1, 1, Some(4), Kind::Text);
    tax.label = "налог".into();
    go(&input, &output, vec![tax], settings(Outside::All));
    let o = read_book(&output);
    assert_eq!(text(&o, "Л", "B1"), "Налог");
    assert_eq!(text(&o, "Л", "B2"), "налог 1");
}

/// Без заголовка подпись не выдумывается: число над диапазоном, повтор первой ячейки ниже, подпись уже есть.
#[test]
fn t03_no_label_without_header() {
    let mut b = Book::default();
    b.sheet("Л").extend([
        (1, 0, Cell::N(2024.0)),
        (2, 0, s("Бухгалтер")),
        (3, 0, s("Директор")),
        (1, 1, s("Бухгалтер")),
        (2, 1, s("Бухгалтер")),
        (3, 1, s("Директор")),
    ]);
    let d = tmp("t03");
    let input = d.join("in.xlsx");
    write_xlsx(&input, &b);
    let g = &read_book(&input)["Л"];
    assert_eq!(template_label(g, 0, 2, None), None, "над диапазоном число");
    assert_eq!(template_label(g, 1, 1, None), None, "первая ячейка повторяется ниже — это данные");
    let mut r = Rule::new("Л", 0, 2, None, Kind::Text);
    r.label = "Роль".into();
    assert_eq!(fill_template(&mut r, g), None);
    assert_eq!(r.label, "Роль");
    let mut r = Rule::new("Л", 0, 2, None, Kind::FullName);
    assert_eq!(fill_template(&mut r, g), None, "только для шаблона");
}

/// Ключи прежних версий записаны с типом «Текст + ID» — читаются как шаблон, новые пишутся так же.
#[test]
fn t04_old_template_name_in_key() {
    assert_eq!(Kind::from_title("Текст + ID"), Some(Kind::Text));
    assert_eq!(Kind::from_title(Kind::Text.title()), Some(Kind::Text));
    let mut r = Rule::new("Л", 0, 2, None, Kind::Text);
    r.label = "Налог".into();
    assert!(r.kind_key().starts_with("Текст + ID"), "{}", r.kind_key());
}

/// Человек из размеченной колонки встречается на другом листе: целой ячейкой (та же общая строка,
/// что и в правиле) и внутри фразы.
fn people(name: &str) -> (PathBuf, PathBuf) {
    let mut b = Book::default();
    b.sheet("Л").extend([(1, 0, s("ФИО")), (2, 0, s("Иванов Иван Иванович")), (3, 0, s("Петрова Мария Ивановна"))]);
    b.sheet("Другой").extend([(1, 0, s("Иванов Иван Иванович")), (2, 0, s("Звонил Иванов Иван Иванович"))]);
    let d = tmp(name);
    let input = d.join("in.xlsx");
    write_xlsx(&input, &b);
    (input, d.join("out.xlsx"))
}

fn fio_rule() -> Vec<Rule> {
    vec![Rule::new("Л", 0, 2, None, Kind::FullName)]
}

#[test]
fn t05_outside_all_cells_keep() {
    const REAL: &str = "Иванов Иван Иванович";
    for (mode, whole_changed, inner_changed) in
        [(Outside::All, true, true), (Outside::Cells, true, false), (Outside::Keep, false, false)]
    {
        let (input, output) = people(&format!("t05-{mode:?}"));
        let rep = go(&input, &output, fio_rule(), settings(mode));
        let o = read_book(&output);
        let fake = text(&o, "Л", "A2");
        assert_ne!(fake, REAL, "{mode:?}: правило работает всегда");
        let (a1, a2) = (text(&o, "Другой", "A1"), text(&o, "Другой", "A2"));
        assert_eq!(a1 == fake, whole_changed, "{mode:?}: целая ячейка {a1}");
        assert_eq!(a1 == REAL, !whole_changed, "{mode:?}: целая ячейка {a1}");
        assert_eq!(a2 == format!("Звонил {fake}"), inner_changed, "{mode:?}: внутри текста {a2}");
        assert_eq!(a2 == format!("Звонил {REAL}"), !inner_changed, "{mode:?}: внутри текста {a2}");
        // Оставленное по настройке видно в проверке — и отчёт объясняет почему.
        let explained = rep.warnings.iter().any(|w| w.contains("по вашей настройке"));
        assert_eq!(explained, mode != Outside::All, "{mode:?}: {:?}", rep.warnings);
    }
}

/// Профиль прежней версии со снятой галочкой «искать внутри текста» = «только целые ячейки».
#[test]
fn t06_legacy_inner_text_off() {
    let (input, output) = people("t06");
    let s = Settings { inner_text: false, ..settings(Outside::All) };
    assert!(!s.inner_search());
    go(&input, &output, fio_rule(), s);
    let o = read_book(&output);
    assert_eq!(text(&o, "Другой", "A1"), text(&o, "Л", "A2"));
    assert_eq!(text(&o, "Другой", "A2"), "Звонил Иванов Иван Иванович");
}

/// Защищённая область: её ячейки остаются как есть, даже если те же значения меняются рядом.
#[test]
fn t07_keep_rule_protects_area() {
    let (input, output) = people("t07");
    let mut rules = fio_rule();
    rules.push(Rule::new("Другой", 0, 1, Some(1), Kind::Keep));
    let rep = go(&input, &output, rules, settings(Outside::All));
    let o = read_book(&output);
    let fake = text(&o, "Л", "A2");
    assert_eq!(text(&o, "Другой", "A1"), "Иванов Иван Иванович", "защищённая ячейка");
    assert_eq!(text(&o, "Другой", "A2"), format!("Звонил {fake}"), "вне защиты — как обычно");
    // Защищённое значение не попадает в ключ и в замены других мест.
    assert_eq!(rep.distinct_values, 2, "{:?}", rep);
}

/// Организация внутри фразы — всегда с ID, в любых кавычках; ID — после закрывающей кавычки.
/// Ячейка с одним названием без формы — тоже с ID.
#[test]
fn t08_company_with_id_inside_text_any_quotes() {
    let mut b = Book::default();
    b.sheet("Л").extend([(1, 0, s("Контрагент")), (2, 0, s("ООО \"Ромашка\"")), (3, 0, s("АО «Вектор»"))]);
    b.sheet("Платежи").extend([
        (1, 0, s("Оплата ООО «Ромашка» по счёту 5")),
        (2, 0, s("Ромашка")),
        (3, 0, s("Возврат от ООО „Ромашка“ без НДС")),
        (4, 0, s("Сверка с Ромашка и АО \"Вектор\"")),
    ]);
    let d = tmp("t08");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    write_xlsx(&input, &b);
    go(&input, &output, vec![Rule::new("Л", 0, 2, None, Kind::Company)], settings(Outside::All));
    let o = read_book(&output);
    let a2 = text(&o, "Л", "A2");
    let a3 = text(&o, "Л", "A3");
    // «ООО "Фирма" (ID 1)» → ядро «Фирма», хвост « (ID 1)».
    let core = |s: &str| s.split('"').nth(1).or_else(|| s.split(['«', '»']).nth(1)).unwrap().to_string();
    let (r, v) = (core(&a2), core(&a3));
    assert!(a2.ends_with("\" (ID 1)") && a3.ends_with("» (ID 2)"), "{a2} / {a3}");
    let p = |c: &str| text(&o, "Платежи", c);
    assert_eq!(p("A1"), format!("Оплата ООО «{r}» (ID 1) по счёту 5"));
    assert_eq!(p("A2"), format!("{r} (ID 1)"));
    assert_eq!(p("A3"), format!("Возврат от ООО „{r}“ (ID 1) без НДС"));
    assert_eq!(p("A4"), format!("Сверка с {r} (ID 1) и АО \"{v}\" (ID 2)"));
}

/// Правило организации без ID — и внутри текста без ID.
#[test]
fn t09_company_without_id_stays_without_id() {
    let mut b = Book::default();
    b.sheet("Л").extend([(1, 0, s("Контрагент")), (2, 0, s("ООО \"Ромашка\""))]);
    b.sheet("Платежи").push((1, 0, s("Оплата ООО «Ромашка» по счёту 5")));
    let d = tmp("t09");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    write_xlsx(&input, &b);
    let mut r = Rule::new("Л", 0, 2, None, Kind::Company);
    r.with_id = false;
    go(&input, &output, vec![r], settings(Outside::All));
    let o = read_book(&output);
    let p = text(&o, "Платежи", "A1");
    assert!(!p.contains("Ромашка") && !p.contains("ID") && p.ends_with("» по счёту 5"), "{p}");
}
