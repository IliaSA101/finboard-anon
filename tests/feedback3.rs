//! Доработки 0.3.0 по второму отзыву клиента: даты в просмотре, короткие коды «Дог1» под типом «Номер»,
//! правило короче таблицы, большие листы (потоковое чтение и проверка кусками).
//! Каждый тест падал до исправления.

mod common;

use std::path::Path;

use common::*;
use finboard_anon::anon::{run, scan_leaks, scan_leaks_with, Job, Report};
use finboard_anon::profile::{Kind, Profile, Rule, Settings};

fn go(input: &Path, output: &Path, key_in: Option<&Path>, key_out: Option<&Path>, rules: Vec<Rule>) -> Report {
    let profile = Profile { rules, settings: Settings { salt: "fb".into(), ..Settings::default() } };
    let job = Job { input: input.into(), output: output.into(), key_in: key_in.map(Into::into), key_out: key_out.map(Into::into), profile };
    run(&job, &mut |_| {}).unwrap()
}

const STYLES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><numFmts count="1"><numFmt numFmtId="164" formatCode="dd/mm/yyyy\ h:mm"/></numFmts><cellXfs count="4"><xf numFmtId="0"/><xf numFmtId="14"/><xf numFmtId="4"/><xf numFmtId="164"/></cellXfs></styleSheet>"#;

/// Колонка «Дата» показывалась числами 43466: формат ячейки (стиль) не читался.
#[test]
fn v01_dates_shown_as_in_excel() {
    let d = tmp("v01");
    let input = d.join("in.xlsx");
    let mut b = Book { styles: STYLES.into(), ..Default::default() };
    b.sheet("Л").extend([
        (1, 0, s("Дата")),
        (2, 0, Cell::Raw(r#"s="1"><v>43466</v>"#.into())),
        (3, 0, Cell::Raw(r#"s="3"><v>43466.75</v>"#.into())),
        (4, 0, Cell::Raw(r#"s="2"><v>43466</v>"#.into())),
        (5, 0, Cell::N(43466.0)),
        (6, 0, Cell::Raw(r#"s="1" t="inlineStr"><is><t>43466</t></is>"#.into())),
    ]);
    write_xlsx(&input, &b);
    let g = read_book(&input);
    let shown = |r: u32| g["Л"].get(r, 0).and_then(|c| c.date.clone());
    assert_eq!(shown(2).as_deref(), Some("01.01.2019"));
    assert_eq!(shown(3).as_deref(), Some("01.01.2019 18:00"));
    assert_eq!(shown(4), None, "число с форматом #,##0.00 — не дата");
    assert_eq!(shown(5), None, "без стиля — не дата");
    assert_eq!(shown(6), None, "текст «43466» в ячейке с форматом даты — текст");
    assert_eq!(text(&g, "Л", "A2"), "43466", "значение для обработки не меняется — меняется только показ");
}

/// «Дог1» под типом «Номер (счёт/карта/договор)» с ID превращался сам в себя: ID 1 вставал на место цифры 1.
/// Ключ прежних версий с такими записями не должен возвращать их обратно.
#[test]
fn v02_short_codes_never_stay_the_same() {
    let d = tmp("v02");
    let input = d.join("in.xlsx");
    let mut b = Book::default();
    let codes = ["Дог1", "Дог2", "Дог3", "Дог1", "Налог10", "Счет 1"];
    let sh = b.sheet("Л");
    sh.push((1, 0, s("Договор контрагента")));
    for (i, c) in codes.iter().enumerate() {
        sh.push((i as u32 + 2, 0, s(c)));
    }
    write_xlsx(&input, &b);
    let old_key = d.join("старый_ключ.csv");
    std::fs::write(
        &old_key,
        "Тип;ID;Реальное значение;Фейковое значение\nНомер (счёт/карта/договор)+ID;1;Дог1;Дог1\nНомер (счёт/карта/договор)+ID;2;Дог2;Дог2\n",
    )
    .unwrap();
    let rule = || {
        let mut r = Rule::new("Л", 0, 2, None, Kind::Account);
        r.with_id = true;
        vec![r]
    };
    for (n, key_in) in [(1, None), (2, Some(old_key.as_path()))] {
        let (output, key_out) = (d.join(format!("out{n}.xlsx")), d.join(format!("key{n}.csv")));
        let rep = go(&input, &output, key_in, Some(&key_out), rule());
        assert!(rep.leaks.is_empty(), "{:?}", rep.leaks);
        let g = read_book(&output);
        let fakes: Vec<String> = (2..=7).map(|r| text(&g, "Л", &format!("A{r}"))).collect();
        for (real, fake) in codes.iter().zip(&fakes) {
            assert!(!codes.contains(&fake.as_str()), "прогон {n}: {real} → {fake} — совпал с настоящим кодом");
        }
        assert_eq!(fakes[0], fakes[3], "одно значение — один фейк");
        assert!(fakes[0].starts_with("Дог") && fakes[5].starts_with("Счет "), "буквы кода остаются: {fakes:?}");
        let key = std::fs::read_to_string(&key_out).unwrap();
        for line in key.lines().skip(1) {
            let f: Vec<&str> = line.split(';').collect();
            assert_ne!(f[2], f[3], "прогон {n}: в ключе запись «сам в себя»: {line}");
        }
    }
}

/// Правило «A2:A3», а таблица ниже продолжается: на настоящем файле в 500 тыс. строк обезличились бы
/// только первые. Отчёт предупреждает; «Итого» и формулы ниже не считаются.
#[test]
fn v03_rule_shorter_than_table_warns() {
    let d = tmp("v03");
    let input = d.join("in.xlsx");
    let mut b = Book::default();
    b.sheet("Л").extend([
        (1, 0, s("ФИО")),
        (2, 0, s("Иванов Иван")),
        (3, 0, s("Петрова Мария")),
        (4, 0, s("Сидоров Пётр")),
        (5, 0, s("Кузнецова Анна")),
        (6, 0, s("Итого")),
        (7, 0, Cell::F("COUNTA(A2:A5)".into(), 4.0)),
    ]);
    write_xlsx(&input, &b);
    let short = go(&input, &d.join("o1.xlsx"), None, None, vec![Rule::new("Л", 0, 2, Some(3), Kind::FullName)]);
    let w: Vec<&String> = short.warnings.iter().filter(|w| w.contains("кончается на строке 3")).collect();
    assert_eq!(w.len(), 1, "{:?}", short.warnings);
    assert!(w[0].contains("ячеек: 2"), "ниже два человека, «Итого» и формула не в счёт: {}", w[0]);
    let full = go(&input, &d.join("o2.xlsx"), None, None, vec![Rule::new("Л", 0, 2, None, Kind::FullName)]);
    assert!(!full.warnings.iter().any(|w| w.contains("кончается")), "{:?}", full.warnings);
    let exact = go(&input, &d.join("o3.xlsx"), None, None, vec![Rule::new("Л", 0, 2, Some(5), Kind::FullName)]);
    assert!(!exact.warnings.iter().any(|w| w.contains("кончается")), "{:?}", exact.warnings);
}

/// Лист больше куска проверки (16 МБ) проверяется кусками по строкам: остаток реального значения
/// в начале, на стыке кусков и в последней строке находится, ложных — нет.
#[test]
fn v04_leak_scan_of_big_sheet_in_chunks() {
    let d = tmp("v04");
    let input = d.join("big.xlsx");
    let filler = "Платежное поручение исходящее по договору поставки ".repeat(4);
    let rows = 120_000u32;
    let planted = [1u32, 41_000, 82_000, rows];
    let mut b = Book::default();
    let sh = b.sheet("Л");
    for r in 1..=rows {
        let t = if planted.contains(&r) { format!("{filler} Секретов Секрет Секретович") } else { format!("{filler} строка {r}") };
        sh.push((r, 0, Cell::Raw(format!(r#"t="inlineStr"><is><t>{}</t></is>"#, esc(&t)))));
    }
    write_xlsx(&input, &b);
    let size = zip::ZipArchive::new(std::fs::File::open(&input).unwrap()).unwrap().by_name("xl/worksheets/sheet1.xml").unwrap().size();
    assert!(size > 3 * (16 << 20) / 2, "лист должен быть больше куска проверки: {size}");
    let leaks = scan_leaks(&input, &["Секретов Секрет Секретович".to_string(), "строка 7".to_string()], &[]).unwrap();
    let sheet = leaks.iter().find(|l| l.part == "xl/worksheets/sheet1.xml").expect("остаток не найден");
    // «строка 7» находится по границе слова: строки 7, но не 70, 71… — их тысячи.
    assert_eq!(sheet.count, planted.len() + 1, "{:?}", sheet.samples);
}

/// Ключ запомнил «Дог1» → «Дог900001» (ID 1). Новый код в следующем прогоне не должен получить тот же ID
/// и тот же фейк: нумерация коротких кодов восстанавливается из ключа.
#[test]
fn v05_short_code_ids_continue_after_key() {
    let d = tmp("v05");
    let rule = || {
        let mut r = Rule::new("Л", 0, 2, None, Kind::Account);
        r.with_id = true;
        vec![r]
    };
    let book = |codes: &[&str], path: &Path| {
        let mut b = Book::default();
        let sh = b.sheet("Л");
        sh.push((1, 0, s("Договор")));
        for (i, c) in codes.iter().enumerate() {
            sh.push((i as u32 + 2, 0, s(c)));
        }
        write_xlsx(path, &b);
    };
    let (in1, in2, key) = (d.join("in1.xlsx"), d.join("in2.xlsx"), d.join("key.csv"));
    book(&["Дог1"], &in1);
    book(&["Дог1", "Дог5"], &in2);
    go(&in1, &d.join("o1.xlsx"), None, Some(&key), rule());
    go(&in2, &d.join("o2.xlsx"), Some(&key), Some(&d.join("key2.csv")), rule());
    let g1 = read_book(&d.join("o1.xlsx"));
    let g2 = read_book(&d.join("o2.xlsx"));
    assert_eq!(text(&g1, "Л", "A2"), text(&g2, "Л", "A2"), "тот же код — тот же фейк по ключу");
    assert_ne!(text(&g2, "Л", "A2"), text(&g2, "Л", "A3"), "новый код получил чужой ID");
}

/// Один и тот же текст в двух колонках одного листа под разными типами: каждая колонка заменяется
/// своим фейком (значения разных типов не смешиваются при сборе).
#[test]
fn v06_same_value_two_kinds_on_one_sheet() {
    let d = tmp("v06");
    let input = d.join("in.xlsx");
    let mut b = Book::default();
    b.sheet("Л").extend([
        (1, 0, s("Ответственный")),
        (1, 1, s("Отдел")),
        (2, 0, s("Иванов Иван")),
        (2, 1, s("Иванов Иван")),
    ]);
    write_xlsx(&input, &b);
    let mut t = Rule::new("Л", 1, 2, None, Kind::Text);
    t.label = "Отдел".into();
    let rep = go(&input, &d.join("o.xlsx"), None, None, vec![Rule::new("Л", 0, 2, None, Kind::FullName), t]);
    let g = read_book(&d.join("o.xlsx"));
    assert_eq!(text(&g, "Л", "B2"), "Отдел 1");
    assert!(!text(&g, "Л", "A2").contains("Иванов Иван"), "{}", text(&g, "Л", "A2"));
    assert!(rep.leaks.is_empty(), "{:?}", rep.leaks);
}

/// Номер ровно той длины, что самый короткий реальный (телефон — 10 цифр подряд), находится проверкой.
#[test]
fn v07_leak_scan_number_of_minimal_length() {
    let d = tmp("v07");
    let input = d.join("in.xlsx");
    let mut b = Book::default();
    b.sheet("Л").push((1, 0, s("звонить 9161234567 после обеда")));
    write_xlsx(&input, &b);
    let digits: std::collections::HashSet<String> = ["9161234567".to_string()].into();
    let leaks = scan_leaks_with(&input, &["Иванов Иван".to_string()], &[], &digits).unwrap();
    assert_eq!(leaks.iter().map(|l| l.count).sum::<usize>(), 1, "{leaks:?}");
}

/// Бухгалтеры не поняли «размечены разными типами: …!B — ФИО; …!C — Организация»: что такое «Организация»
/// и какие это колонки. Теперь — названия колонок из шапки и «правило «…»»; реальных значений нет.
/// Два «Шаблона» с разными подписями разделены намеренно — о них не предупреждается.
#[test]
fn v08_kind_conflict_names_columns_and_rules() {
    let d = tmp("v08");
    let input = d.join("Продажи.xlsx");
    let mut b = Book::default();
    b.sheet("Л").extend([
        (1, 0, s("Ответственный")),
        (1, 1, s("Покупатель")),
        (2, 0, s("Сидоров Пётр Ильич")),
        (2, 1, s("Сидоров Пётр Ильич")),
        (3, 0, s("Кузнецова Анна Павловна")),
        (3, 1, s("Вектор-Строй")),
    ]);
    write_xlsx(&input, &b);
    let rules = vec![Rule::new("Л", 0, 2, None, Kind::FullName), Rule::new("Л", 1, 2, None, Kind::Company)];
    let profile = Profile { rules: rules.clone(), settings: Settings { salt: "fb".into(), ..Settings::default() } };
    let rep = go(&input, &d.join("o1.xlsx"), None, None, rules);
    let w = rep.warnings.iter().find(|w| w.contains("разные фейки")).unwrap_or_else(|| panic!("нет предупреждения: {:?}", rep.warnings));
    assert!(w.contains("колонка «Ответственный» (Продажи.xlsx, лист Л, A) — правило «ФИО»"), "{w}");
    assert!(w.contains("колонка «Покупатель» (Продажи.xlsx, лист Л, B) — правило «Организация»"), "{w}");
    assert!(w.starts_with("1 одинаковое значение есть"), "одно общее значение: {w}");
    for real in ["Сидоров", "Кузнецова", "Вектор"] {
        assert!(!w.contains(real), "в предупреждении реальное значение: {w}");
    }
    // Итог отчёта прямо называет, какой файл проверен.
    let summary = rep.summary(&profile);
    assert!(summary.iter().any(|l| l == "Проверка обезличенного файла: реальных значений из размеченных колонок в нём не найдено."), "{summary:?}");

    let (mut t1, mut t2) = (Rule::new("Л", 0, 2, None, Kind::Text), Rule::new("Л", 1, 2, None, Kind::Text));
    t1.label = "Отдел".into();
    t2.label = "Склад".into();
    let rep = go(&input, &d.join("o2.xlsx"), None, None, vec![t1, t2]);
    assert!(rep.warnings.iter().all(|w| !w.contains("разные фейки")), "два «Шаблона» с разными подписями — не ошибка: {:?}", rep.warnings);
    let g = read_book(&d.join("o2.xlsx"));
    assert_eq!((text(&g, "Л", "A2"), text(&g, "Л", "B2")), ("Отдел 1".to_string(), "Склад 1".to_string()), "подписи шаблонов разделены");
}
