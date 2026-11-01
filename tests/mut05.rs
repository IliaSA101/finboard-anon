//! Тесты по выжившим мутантам кода 0.5.0 (tools/mutate.py, «0.5 …»): «Связь» вне разметки, разные фейки
//! разным значениям (и с ключом прошлых запусков), журнал без реальных значений, замок ключа, запись на место.
//! Каждый тест падает на своём мутанте — то есть ловит настоящую ошибку такого вида.

mod common;

use std::path::Path;

use common::*;
use finboard_anon::anon::{run_batch, Batch, FileJob, Report};
use finboard_anon::fake::{phone_key, split_company, strip_id_suffix, Faker, KeyEntry};
use finboard_anon::keyfile::lock_path;
use finboard_anon::profile::{Kind, Profile, Rule, Settings};

const KEY: &str = "КЛЮЧ_ОБЕЗЛИЧИВАНИЯ_НЕ_ПЕРЕДАВАТЬ.csv";

fn settings(salt: &str) -> Settings {
    Settings { salt: salt.into(), ..Settings::default() }
}

fn rule(col: u32, kind: Kind, link: &str, with_id: bool) -> Rule {
    let mut r = Rule::new("Л", col, 2, None, kind);
    r.with_id = with_id;
    r.link = link.into();
    r
}

fn run_files(files: &[(&Path, &Path, Vec<Rule>)], st: Settings, key_in: Option<&Path>, key_out: Option<&Path>) -> anyhow::Result<(Vec<Report>, Vec<String>)> {
    let b = Batch {
        files: files.iter().map(|(i, o, r)| FileJob { input: i.to_path_buf(), output: o.to_path_buf(), rules: r.clone() }).collect(),
        settings: st,
        key_in: key_in.map(Path::to_path_buf),
        key_out: key_out.map(Path::to_path_buf),
    };
    let mut log = Vec::new();
    let reps = run_batch(&b, &mut |m| log.push(m))?;
    Ok((reps, log))
}

/// Один файл, лист «Л», без ключа.
fn run_one(input: &Path, output: &Path, rules: Vec<Rule>, salt: &str) -> Report {
    run_files(&[(input, output, rules)], settings(salt), None, None).unwrap().0.remove(0)
}

fn book_of(path: &Path, cells: Vec<(u32, u32, Cell)>) {
    let mut b = Book::default();
    *b.sheet("Л") = cells;
    write_xlsx(path, &b);
}

fn cell(path: &Path, a1: &str) -> String {
    text(&read_book(path), "Л", a1)
}

fn digits(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).collect()
}

/// Название организации без формы и ID: «ООО «Альфа» (ID 3)» → «Альфа».
fn company_name(s: &str) -> String {
    split_company(strip_id_suffix(s)).1.trim().to_string()
}

// ---------------------------------------------------------------- «Связь» вне разметки

/// Номер внутри текста, записанный иначе, чем в колонке («77 07 083893»), получает фейк по цифрам. Значение
/// в нескольких связях — фейк связи первого по порядку ПРАВИЛА (не первой колонки в данных), а при общей
/// связи — фейк общей. Мутанты: порядок связей по данным; общая связь последней; последняя связь побеждает.
#[test]
fn m01_number_in_text_takes_first_rule_link() {
    let d = tmp("m05_01");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    book_of(
        &input,
        vec![
            (1, 0, s("ИНН (связь а)")),
            (1, 1, s("ИНН (связь б)")),
            (1, 2, s("ИНН")),
            (1, 3, s("ИНН (связь а)")),
            (1, 4, s("Назначение")),
            (2, 0, s("7707083893")),
            (2, 1, s("7707083893")),
            (2, 2, s("5001007322")),
            (2, 3, s("5001007322")),
            (2, 4, s("Оплата ИНН 77 07 083893 за март")),
            (3, 4, s("Возврат ИНН 50 01 007322")),
        ],
    );
    // Правило связи «б» — первым, хотя его колонка B правее колонки A связи «а».
    let rules = vec![rule(1, Kind::Inn, "б", false), rule(0, Kind::Inn, "а", false), rule(2, Kind::Inn, "", false), rule(3, Kind::Inn, "а", false)];
    for round in 0..3 {
        let rep = run_one(&input, &output, rules.clone(), &format!("м01-{round}"));
        let g = read_book(&output);
        let t = |a: &str| text(&g, "Л", a);
        assert_ne!(digits(&t("A2")), digits(&t("B2")), "раунд {round}: связи не разделены");
        assert_eq!(digits(&t("E2")), digits(&t("B2")), "раунд {round}: в тексте — не фейк связи первого правила («б»): {}", t("E2"));
        assert_ne!(digits(&t("C2")), digits(&t("D2")), "раунд {round}");
        assert_eq!(digits(&t("E3")), digits(&t("C2")), "раунд {round}: в тексте — не фейк общей связи: {}", t("E3"));
        assert!(rep.leaks.is_empty(), "{:?}", rep.leaks);
    }
}

/// Телефон внутри текста в другой записи («8 916 123-45-67») при телефоне в общей связи и в связи «а» —
/// фейк общей связи. Мутант: последняя связь побеждает.
#[test]
fn m02_phone_in_text_takes_common_link() {
    let d = tmp("m05_02");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    book_of(
        &input,
        vec![
            (1, 0, s("Телефон контрагента")),
            (1, 1, s("Телефон")),
            (1, 2, s("Примечание")),
            (2, 0, s("+7 (916) 123-45-67")),
            (2, 1, s("+7 (916) 123-45-67")),
            (2, 2, s("Звонить 8 916 123-45-67 после обеда")),
        ],
    );
    let rules = vec![rule(0, Kind::Phone, "а", false), rule(1, Kind::Phone, "", false)];
    let rep = run_one(&input, &output, rules, "м02");
    let ten = |s: &str| phone_key(&digits(s));
    let g = read_book(&output);
    let t = |a: &str| text(&g, "Л", a);
    assert_ne!(ten(&t("A2")), ten(&t("B2")));
    assert_eq!(ten(&t("C2")), ten(&t("B2")), "в тексте — не фейк общей связи: {} / {} / {}", t("A2"), t("B2"), t("C2"));
    assert!(rep.leaks.is_empty(), "{:?}", rep.leaks);
}

/// Название организации внутри текста («Поставщик Ромашка») при организации в связи «а» (колонка левее) и в
/// общей связи — фейк общей. Мутант: словарь названий общий для связей — побеждает встреченная первой.
#[test]
fn m03_company_in_text_takes_common_link() {
    let d = tmp("m05_03");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    book_of(
        &input,
        vec![
            (1, 0, s("Контрагент (связь)")),
            (1, 1, s("Контрагент")),
            (1, 2, s("Назначение")),
            (2, 0, s("ООО «Ромашка»")),
            (2, 1, s("ООО «Ромашка»")),
            (2, 2, s("Поставщик Ромашка, счёт 5")),
        ],
    );
    let rules = vec![rule(0, Kind::Company, "а", false), rule(1, Kind::Company, "", false)];
    let rep = run_one(&input, &output, rules, "м03");
    let g = read_book(&output);
    let t = |a: &str| text(&g, "Л", a);
    let (linked, common) = (company_name(&t("A2")), company_name(&t("B2")));
    assert_ne!(linked, common);
    assert!(t("C2").contains(&common) && !t("C2").contains("Ромашка"), "в тексте — не фейк общей связи: {} / {} / {}", t("A2"), t("B2"), t("C2"));
    assert!(rep.leaks.is_empty(), "{:?}", rep.leaks);
}

/// Значение, которое есть только в ключе прошлых запусков (в связи «а» и в общей), во втором файле стоит вне
/// разметки — ставится фейк общей связи, как бы ни лежали строки в ключе. Мутант: побеждает первая строка ключа.
#[test]
fn m04_key_only_value_outside_rules_takes_common_link() {
    let d = tmp("m05_04");
    let key = d.join(KEY);
    let (in1, out1) = (d.join("in1.xlsx"), d.join("out1.xlsx"));
    // Колонка связи «а» — левее: в ключе её строка идёт первой.
    book_of(&in1, vec![(1, 0, s("Представитель")), (1, 1, s("ФИО")), (2, 0, s("Сидоров Пётр Николаевич")), (2, 1, s("Сидоров Пётр Николаевич"))]);
    run_files(&[(&in1, &out1, vec![rule(0, Kind::FullName, "а", true), rule(1, Kind::FullName, "", true)])], settings("м04"), None, Some(&key)).unwrap();
    let (linked, common) = (cell(&out1, "A2"), cell(&out1, "B2"));
    assert_ne!(linked, common);
    let (in2, out2) = (d.join("in2.xlsx"), d.join("out2.xlsx"));
    book_of(&in2, vec![(1, 0, s("ФИО")), (1, 1, s("Подписал")), (2, 0, s("Ковалёв Игорь Олегович")), (2, 1, s("Сидоров Пётр Николаевич"))]);
    run_files(&[(&in2, &out2, vec![rule(0, Kind::FullName, "", true)])], settings("м04-2"), Some(&key), None).unwrap();
    assert_eq!(cell(&out2, "B2"), common, "вне разметки — не фейк общей связи (фейк связи: {linked})");
}

/// Правило «Очистить», у которого осталась связь от прежнего типа (тип сменили, поле «Связь» на карточке
/// скрылось, а слово в правиле осталось): фейков у него нет — строки «размечено в колонках с разной «Связью»»
/// быть не должно. Мутант: «Очистить» и «Не менять» считаются связью с фейком.
#[test]
fn m05_clear_rule_with_link_is_not_a_link_overlap() {
    let d = tmp("m05_05");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    book_of(&input, vec![(1, 0, s("ФИО")), (1, 1, s("Подписант")), (2, 0, s("Иванов Иван Иванович")), (2, 1, s("Иванов Иван Иванович"))]);
    let rep = run_one(&input, &output, vec![rule(0, Kind::FullName, "", true), rule(1, Kind::Clear, "Контрагент", false)], "м05");
    assert!(!rep.warnings.iter().any(|w| w.contains("разной «Связью»")), "{:?}", rep.warnings);
    assert_eq!(cell(&output, "B2"), "");
}

/// Один телефон в связи «а» в двух записях («+7 (916)…» и «8 (916)…»), а между ними — колонка общей связи:
/// оба получают один фейк. Мутант: при переключении на связь её словари теряются — вторая запись получает
/// новый фейк.
#[test]
fn m23_link_maps_survive_switching() {
    let d = tmp("m05_23");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    book_of(
        &input,
        vec![
            (1, 0, s("Телефон контрагента")),
            (1, 1, s("Телефон")),
            (2, 0, s("+7 (916) 123-45-67")),
            (2, 1, s("+7 (903) 555-11-22")),
            (3, 0, s("8 (916) 123-45-67")),
            (3, 1, s("+7 (926) 777-88-99")),
        ],
    );
    for with_id in [false, true] {
        run_one(&input, &output, vec![rule(0, Kind::Phone, "а", with_id), rule(1, Kind::Phone, "", with_id)], "м23");
        let g = read_book(&output);
        let ten = |a: &str| phone_key(&digits(&text(&g, "Л", a)));
        assert_eq!(ten("A2"), ten("A3"), "с ID: {with_id}: один телефон в связи «а» — разные фейки");
    }
}

/// Ключ со связью: в новом файле тот же телефон в связи «а» записан иначе — фейк из ключа; в общей связи
/// тот же телефон — свой фейк, не связи «а». Мутант: записи ключа со связью попадают в общую связь.
#[test]
fn m24_key_entries_restore_their_link() {
    let d = tmp("m05_24");
    let key = d.join(KEY);
    let (in1, out1) = (d.join("in1.xlsx"), d.join("out1.xlsx"));
    book_of(&in1, vec![(1, 0, s("Телефон контрагента")), (2, 0, s("+7 (916) 123-45-67"))]);
    run_files(&[(&in1, &out1, vec![rule(0, Kind::Phone, "а", false)])], settings("м24"), None, Some(&key)).unwrap();
    let linked = phone_key(&digits(&cell(&out1, "A2")));
    let (in2, out2) = (d.join("in2.xlsx"), d.join("out2.xlsx"));
    book_of(&in2, vec![(1, 0, s("Телефон контрагента")), (1, 1, s("Телефон")), (2, 0, s("8 916 123 45 67")), (2, 1, s("8 916 123 45 67"))]);
    run_files(&[(&in2, &out2, vec![rule(0, Kind::Phone, "а", false), rule(1, Kind::Phone, "", false)])], settings("м24-2"), Some(&key), None).unwrap();
    let g = read_book(&out2);
    let ten = |a: &str| phone_key(&digits(&text(&g, "Л", a)));
    assert_eq!(ten("A2"), linked, "связь «а»: фейк не из ключа");
    assert_ne!(ten("B2"), linked, "общая связь получила фейк связи «а» из ключа");
}

// ---------------------------------------------------------------- разным значениям — разные фейки

/// Один ИНН в общей связи (из ключа) и в связи «а»: даже когда фейк связи «а» совпал бы с фейком общей
/// из ключа, он другой. Мутант: владелец выданного номера — без связи, и связь «а» «узнаёт» чужой фейк.
#[test]
fn m06_same_number_in_other_link_never_reuses_key_fake() {
    let st = settings("м06");
    let linked = rule(0, Kind::Inn, "а", false);
    let common = rule(0, Kind::Inn, "", false);
    let x = "7707083893";
    // Фейк, который получил бы ИНН в связи «а», — подложен в ключ как фейк общей связи.
    let fa = Faker::new(&st).fake(&linked, x, false);
    let mut f = Faker::new(&st);
    f.load_entries(vec![KeyEntry { kind_key: common.kind_key(), id: None, real: x.into(), fake: fa.clone() }]);
    assert_eq!(f.fake(&common, x, false), fa);
    let got = f.fake(&linked, x, false);
    assert_ne!(digits(&got), digits(&fa), "один ИНН в разных связях — один фейк");
}

/// Фейк из ключа прошлых запусков занят: новое значение того же вида его не получает, даже если по соли
/// ему выпал бы именно он. Иначе в переданных файлах два разных контрагента стали бы одним.
/// Мутанты: при загрузке ключа не занимаются фейки номеров, телефонов, коротких кодов, адресов.
#[test]
fn m07_key_fakes_are_taken_for_new_values() {
    let st = settings("м07");
    let cases: [(Kind, &str, &str); 4] = [
        (Kind::Inn, "5001007322", "7707083893"),
        (Kind::Phone, "+7 (903) 555-11-22", "+7 (916) 123-45-67"),
        (Kind::Account, "Дог7", "Дог1"),
        (Kind::Address, "г. Тверь, ул. Мира, д. 1", "г. Москва, ул. Ленина, д. 5"),
    ];
    let norm = |kind: Kind, s: &str| match kind {
        Kind::Phone => phone_key(&digits(s)),
        Kind::Inn => digits(s),
        _ => s.to_lowercase(),
    };
    for (kind, old, new) in cases {
        let r = rule(0, kind, "", false);
        // Что получило бы новое значение без ключа…
        let would = Faker::new(&st).fake(&r, new, false);
        // …то и записано в ключе как фейк другого значения.
        let mut f = Faker::new(&st);
        f.load_entries(vec![KeyEntry { kind_key: r.kind_key(), id: None, real: old.into(), fake: would.clone() }]);
        assert_eq!(f.fake(&r, old, false), would, "{kind:?}: ключ не восстановлен");
        let got = f.fake(&r, new, false);
        assert_ne!(norm(kind, &got), norm(kind, &would), "{kind:?}: «{new}» получил фейк «{old}» из ключа");
    }
}

/// ID ровно во всю ширину места: ИНН из 10 цифр — «99» и семь цифр ID; ID 1 000 000 — «991000000…».
/// Мутант: ID, занимающий место целиком, считается не поместившимся (номер без ID).
#[test]
fn m08_id_exactly_full_width_is_visible() {
    let r = rule(0, Kind::Inn, "", true);
    let mut f = Faker::new(&settings("м08"));
    f.load_entries(vec![KeyEntry { kind_key: r.kind_key(), id: Some(999_999), real: "5001007322".into(), fake: "9909999993".into() }]);
    let fake = f.fake(&r, "7707083893", false);
    assert!(digits(&fake).starts_with("991000000"), "ID 1000000 не виден в номере: {fake}");
}

/// Фейковый ИНН не совпадает ни с одним настоящим номером книги: ни с числом ниже в той же колонке (числа не
/// в общих строках), ни с текстом в колонке без правила. Иначе одна ячейка показывает чужой настоящий ИНН.
/// Мутанты: значения правил не отмечаются настоящими до выбора фейков; номера из строк книги — тоже.
#[test]
fn m09_fake_number_never_equals_real_number_of_book() {
    let d = tmp("m05_09");
    let x = 7707083893.0;
    let pre = d.join("pre.xlsx");
    book_of(&pre, vec![(1, 0, s("ИНН")), (2, 0, Cell::N(x))]);
    let r = vec![rule(0, Kind::Inn, "", false)];
    run_one(&pre, &d.join("pre_out.xlsx"), r.clone(), "м09");
    let f = digits(&cell(&d.join("pre_out.xlsx"), "A2"));
    assert_eq!(f.len(), 10);
    // Тот самый номер — ниже, числом.
    let a = d.join("a.xlsx");
    book_of(&a, vec![(1, 0, s("ИНН")), (2, 0, Cell::N(x)), (3, 0, Cell::N(f.parse().unwrap()))]);
    run_one(&a, &d.join("a_out.xlsx"), r.clone(), "м09");
    assert_ne!(digits(&cell(&d.join("a_out.xlsx"), "A2")), f, "фейк = настоящий ИНН строкой ниже");
    // Тот самый номер — текстом в колонке без правила.
    let b = d.join("b.xlsx");
    book_of(&b, vec![(1, 0, s("ИНН")), (1, 1, s("ИНН банка")), (2, 0, Cell::N(x)), (2, 1, s(&f))]);
    run_one(&b, &d.join("b_out.xlsx"), r, "м09");
    let g = read_book(&d.join("b_out.xlsx"));
    assert_ne!(digits(&text(&g, "Л", "A2")), f, "фейк = настоящий ИНН соседней колонки");
}

/// Фейковый адрес не совпадает с адресом, который уже есть в книге (в колонке без правила).
/// Мутант: занятые строки книги не проверяются.
#[test]
fn m10_fake_address_never_equals_address_in_book() {
    let d = tmp("m05_10");
    let real = "г. Москва, ул. Ленина, д. 5";
    let r = vec![rule(0, Kind::Address, "", false)];
    let pre = d.join("pre.xlsx");
    book_of(&pre, vec![(1, 0, s("Адрес")), (2, 0, s(real))]);
    run_one(&pre, &d.join("pre_out.xlsx"), r.clone(), "м10");
    let f = cell(&d.join("pre_out.xlsx"), "A2");
    let a = d.join("a.xlsx");
    book_of(&a, vec![(1, 0, s("Адрес")), (1, 1, s("Склад")), (2, 0, s(real)), (2, 1, s(&f))]);
    run_one(&a, &d.join("a_out.xlsx"), r, "м10");
    assert_ne!(cell(&d.join("a_out.xlsx"), "A2"), f, "фейк адреса = адрес склада из книги");
}

/// Одна дата в общей связи и в связи «а»: сдвиги разные у каждой даты (иначе по фейкам видно, что даты равны).
/// Сдвигов 360 — среди тысяч дат случайные совпадения неизбежны. Мутант: совпадение сдвигов не проверяется.
#[test]
fn m11_same_date_in_two_links_shifted_differently() {
    let mut f = Faker::new(&settings("м11"));
    let (common, linked) = (rule(0, Kind::BirthDate, "", false), rule(0, Kind::BirthDate, "а", false));
    let mut same = Vec::new();
    for i in 0..3000u32 {
        let date = format!("{:02}.{:02}.{}", 1 + i % 28, 1 + i / 28 % 12, 1940 + i / 336);
        let (a, b) = (f.fake(&common, &date, false), f.fake(&linked, &date, false));
        assert_ne!(a, date);
        if a == b {
            same.push(format!("{date} → {a}"));
        }
    }
    assert!(same.is_empty(), "одна дата в разных связях — один фейк: {same:?}");
}

// ---------------------------------------------------------------- организации и ключ

/// «Вектор» стоит выше «ООО «Вектор»»: формы собираются до выбора фейков — одна организация, один ID.
/// Мутант: формы не собираются заранее — «Вектор» без формы стал отдельной организацией.
#[test]
fn m12_bare_name_before_form_is_same_company() {
    let d = tmp("m05_12");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    book_of(&input, vec![(1, 0, s("Контрагент")), (2, 0, s("Вектор")), (3, 0, s("ООО «Вектор»"))]);
    run_one(&input, &output, vec![rule(0, Kind::Company, "", true)], "м12");
    let g = read_book(&output);
    let (a, b) = (text(&g, "Л", "A2"), text(&g, "Л", "A3"));
    let id = |s: &str| s.rsplit_once("(ID ").map(|(_, x)| x.to_string()).unwrap_or_default();
    assert_eq!((company_name(&a), id(&a)), (company_name(&b), id(&b)), "{a} / {b}");
}

/// В ключе — «ООО «Ромашка»» и «АО «Ромашка»» (разные юрлица). В новом файле они записаны полными формами —
/// каждая получает фейк своего юрлица из ключа. Мутант: организации ключа запоминаются без формы — обе
/// сливаются в одну, вторая получает новый фейк.
#[test]
fn m13_key_keeps_companies_with_same_name_apart() {
    let d = tmp("m05_13");
    let key = d.join(KEY);
    let (in1, out1) = (d.join("in1.xlsx"), d.join("out1.xlsx"));
    book_of(&in1, vec![(1, 0, s("Контрагент")), (2, 0, s("ООО «Ромашка»")), (3, 0, s("АО «Ромашка»"))]);
    let r = vec![rule(0, Kind::Company, "", true)];
    run_files(&[(&in1, &out1, r.clone())], settings("м13"), None, Some(&key)).unwrap();
    let (ooo, ao) = (company_name(&cell(&out1, "A2")), company_name(&cell(&out1, "A3")));
    assert_ne!(ooo, ao);
    let (in2, out2) = (d.join("in2.xlsx"), d.join("out2.xlsx"));
    book_of(
        &in2,
        vec![(1, 0, s("Контрагент")), (2, 0, s("Акционерное общество «Ромашка»")), (3, 0, s("Общество с ограниченной ответственностью «Ромашка»"))],
    );
    run_files(&[(&in2, &out2, r)], settings("м13-другая"), Some(&key), None).unwrap();
    assert_eq!(company_name(&cell(&out2, "A2")), ao, "АО из ключа");
    assert_eq!(company_name(&cell(&out2, "A3")), ooo, "ООО из ключа");
}

/// ИНН числом без ведущего нуля (274062111) в ключе; в новом файле он записан внутри текста с нулём
/// («0274062111») — заменяется фейком из ключа. Мутант: ИНН-число из ключа не дополняется нулём.
#[test]
fn m14_key_inn_number_without_zero_found_in_text() {
    let d = tmp("m05_14");
    let key = d.join(KEY);
    let (in1, out1) = (d.join("in1.xlsx"), d.join("out1.xlsx"));
    book_of(&in1, vec![(1, 0, s("ИНН")), (2, 0, Cell::N(274062111.0))]);
    let r = vec![rule(0, Kind::Inn, "", true)];
    run_files(&[(&in1, &out1, r.clone())], settings("м14"), None, Some(&key)).unwrap();
    let (in2, out2) = (d.join("in2.xlsx"), d.join("out2.xlsx"));
    book_of(&in2, vec![(1, 0, s("ИНН")), (1, 1, s("Назначение")), (2, 0, s("7707083893")), (2, 1, s("Оплата по ИНН 0274062111 за март"))]);
    let (reps, _) = run_files(&[(&in2, &out2, r)], settings("м14"), Some(&key), None).unwrap();
    let t = cell(&out2, "B2");
    assert!(!t.contains("274062111"), "ИНН из ключа в тексте остался: {t}");
    assert!(reps[0].leaks.is_empty(), "{:?}", reps[0].leaks);
}

// ---------------------------------------------------------------- журнал

/// Найденные остатки показываются масками; одинаковые маски («И*** И*** П***» у двух людей) — один раз.
#[test]
fn m15_same_masks_shown_once() {
    let d = tmp("m05_15");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    book_of(
        &input,
        vec![
            (1, 0, s("ФИО")),
            (1, 1, s("Примечание")),
            (2, 0, s("Иванов Иван Петрович")),
            (3, 0, s("Ильин Игорь Павлович")),
            (2, 1, s("Звонил Иванов Иван Петрович")),
            (3, 1, s("Звонил Ильин Игорь Павлович")),
        ],
    );
    let rules = vec![rule(0, Kind::FullName, "", true)];
    let st = Settings { inner_text: false, ..settings("м15") };
    let (reps, _) = run_files(&[(&input, &output, rules.clone())], st.clone(), None, None).unwrap();
    assert!(reps[0].leaks.iter().any(|l| l.samples.len() >= 2), "{:?}", reps[0].leaks);
    let lines = reps[0].summary(&Profile { rules, settings: st });
    let line = lines.iter().find(|l| l.contains("вхожд., например:")).unwrap_or_else(|| panic!("{lines:?}"));
    assert_eq!(line.matches("И*** И*** П***").count(), 1, "{line}");
}

// ---------------------------------------------------------------- замок ключа

fn host() -> String {
    std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_default()
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

fn people_batch(d: &Path, n: usize) -> Batch {
    let files = (1..=n)
        .map(|i| {
            let input = d.join(format!("in{i}.xlsx"));
            if !input.exists() {
                book_of(&input, vec![(1, 0, s("ФИО")), (2, 0, s(["Иванов Иван Иванович", "Петрова Мария Сергеевна", "Сидоров Пётр Николаевич"][(i - 1) % 3]))]);
            }
            FileJob { input, output: d.join(format!("out{i}.xlsx")), rules: vec![rule(0, Kind::FullName, "", true)] }
        })
        .collect();
    let key = d.join(KEY);
    Batch { files, settings: settings("замок"), key_in: key.exists().then(|| key.clone()), key_out: Some(key) }
}

fn go(b: &Batch) -> Result<Vec<String>, String> {
    let mut log = Vec::new();
    run_batch(b, &mut |m| log.push(m)).map(|_| log).map_err(|e| format!("{e:#}"))
}

fn dead_pid() -> u32 {
    let mut child = std::process::Command::new(if cfg!(windows) { "cmd" } else { "true" })
        .args(if cfg!(windows) { &["/C", "exit"][..] } else { &[][..] })
        .spawn()
        .unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

/// Замок стоит весь прогон — до записи ключа и результатов, а не только в начале.
/// Мутант: замок снимается сразу после постановки.
#[test]
fn m16_lock_held_until_the_end() {
    let d = tmp("m05_16");
    let b = people_batch(&d, 1);
    let lock = lock_path(&d.join(KEY));
    let mut held = Vec::new();
    run_batch(&b, &mut |m| {
        if m.starts_with("Проверка результата") || m.starts_with("Сбор значений") {
            held.push(lock.exists());
        }
    })
    .unwrap();
    assert_eq!(held, [true, true], "замок снят до конца прогона");
    assert!(!lock.exists(), "замок остался");
}

/// Замок другого живого процесса на этом компьютере — ключ занят. Тот процесс закончился — замок брошен.
/// Мутант: живость процесса не проверяется — замок идущего запуска снимается.
#[test]
fn m17_lock_of_live_process_is_respected() {
    let d = tmp("m05_17");
    let key = d.join(KEY);
    let mut child = if cfg!(windows) {
        std::process::Command::new("cmd").args(["/C", "ping -n 60 127.0.0.1 >NUL"]).spawn().unwrap()
    } else {
        std::process::Command::new("sleep").arg("60").spawn().unwrap()
    };
    std::fs::write(lock_path(&key), format!("pid={}\r\nкомпьютер={}\r\nначат={}\r\n", child.id(), host(), now())).unwrap();
    let res = go(&people_batch(&d, 1));
    let _ = child.kill();
    let _ = child.wait();
    let err = res.expect_err("замок живого процесса снят");
    assert!(err.starts_with("Ключ сейчас занят"), "{err}");
    assert!(!d.join("out1.xlsx").exists());
    go(&people_batch(&d, 1)).expect("замок завершившегося процесса не снят");
    assert!(!lock_path(&key).exists());
}

/// Замок с другого компьютера (ключ в общей папке): номер процесса там здесь не проверить — замок уважается,
/// хотя здесь процесса с таким номером нет. Мутант: процесс проверяется и для чужого компьютера.
#[test]
fn m18_lock_from_other_computer_is_respected() {
    let d = tmp("m05_18");
    let key = d.join(KEY);
    std::fs::write(lock_path(&key), format!("pid={}\r\nкомпьютер=ДРУГОЙ-ПК\r\nначат={}\r\n", dead_pid(), now())).unwrap();
    let err = go(&people_batch(&d, 1)).expect_err("замок другого компьютера снят");
    assert!(err.starts_with("Ключ сейчас занят") && err.contains("на компьютере ДРУГОЙ-ПК"), "{err}");
    assert!(lock_path(&key).exists());
}

/// Замок с непонятным содержимым: свежий — его, возможно, ещё дописывают, ключ занят; старше минуты — брошен.
/// Мутанты: непонятный замок брошен всегда / не брошен никогда.
#[test]
fn m19_unreadable_lock_by_age() {
    let d = tmp("m05_19");
    let lock = lock_path(&d.join(KEY));
    std::fs::write(&lock, "мусор").unwrap();
    let err = go(&people_batch(&d, 1)).expect_err("свежий недописанный замок снят");
    assert!(err.starts_with("Ключ сейчас занят"), "{err}");
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(300);
    std::fs::File::options().write(true).open(&lock).unwrap().set_modified(old).unwrap();
    go(&people_batch(&d, 1)).expect("старый испорченный замок не снят");
    assert!(!lock.exists());
}

// ---------------------------------------------------------------- запись на место

/// Ключ открыт в Excel: ошибка до записи — называет ключ и просит закрыть его; ни копии ключа, ни результатов.
/// Мутант: ключ на «можно заменить» не проверяется — копия ключа делается, запись обрывается на середине.
#[cfg(windows)]
#[test]
fn m20_key_open_in_excel_changes_nothing() {
    use std::os::windows::fs::OpenOptionsExt;
    let d = tmp("m05_20");
    go(&people_batch(&d, 1)).unwrap();
    let key = d.join(KEY);
    let before = std::fs::read(&key).unwrap();
    std::fs::remove_file(d.join("out1.xlsx")).unwrap();
    const FILE_SHARE_READ: u32 = 1;
    let held = std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(&key).unwrap();
    let res = go(&people_batch(&d, 1));
    drop(held);
    let err = res.expect_err("записано при открытом ключе");
    assert!(err.contains("ключ «") && err.contains("закройте файл в Excel") && err.contains("Ничего не записано"), "{err}");
    let names: Vec<String> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert!(!names.iter().any(|n| n.contains("_копия") || n.ends_with(".tmp")), "{names:?}");
    assert!(!d.join("out1.xlsx").exists());
    assert_eq!(std::fs::read(&key).unwrap(), before);
}

/// Результат держит программа, которая разрешает другим запись, но не удаление (замену): заменить его нельзя.
/// Это видно заранее — ничего не записано, и первый файл пакета тоже. Мутант: проверяется только право записи.
#[cfg(windows)]
#[test]
fn m21_result_that_cannot_be_replaced_is_found_before_writing() {
    use std::os::windows::fs::OpenOptionsExt;
    let d = tmp("m05_21");
    go(&people_batch(&d, 2)).unwrap();
    std::fs::write(d.join("out1.xlsx"), "прежний результат").unwrap();
    const FILE_SHARE_READ: u32 = 1;
    const FILE_SHARE_WRITE: u32 = 2;
    let held = std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE).open(d.join("out2.xlsx")).unwrap();
    let res = go(&people_batch(&d, 2));
    drop(held);
    let err = res.expect_err("записано при незаменяемом результате");
    assert!(err.contains("out2.xlsx") && err.contains("Ничего не записано"), "{err}");
    assert_eq!(std::fs::read_to_string(d.join("out1.xlsx")).unwrap(), "прежний результат", "первый файл пакета заменён");
}

/// Ключ не записался (на месте его временного файла — папка): результаты не встают на место — иначе их
/// нечем было бы расшифровать. Мутант: ошибка записи ключа не останавливает запись результатов.
#[test]
fn m22_key_write_failure_keeps_results_out() {
    let d = tmp("m05_22");
    let key = d.join(KEY);
    std::fs::create_dir(key.with_extension("csv.tmp")).unwrap();
    let err = go(&people_batch(&d, 2)).expect_err("ключ не записан, а ошибки нет");
    assert!(err.contains("ключ"), "{err}");
    for i in 1..=2 {
        assert!(!d.join(format!("out{i}.xlsx")).exists(), "out{i} записан без ключа");
        assert!(!d.join(format!("out{i}.xlsx.tmp")).exists(), "временный out{i} остался");
    }
    assert!(!key.exists());
}
