//! Доработки 0.4.0 по третьему отзыву: предпросмотр больших листов, названия колонок в отчёте.
//! Каждый тест падал до исправления.

mod common;

use std::path::Path;

use common::*;
use finboard_anon::anon::{preview, run, Job, Report, PREVIEW_ROWS};
use finboard_anon::profile::{Kind, Profile, Rule, Settings};

fn profile(rules: Vec<Rule>) -> Profile {
    Profile { rules, settings: Settings { salt: "fb".into(), ..Settings::default() } }
}

fn go(input: &Path, output: &Path, rules: Vec<Rule>) -> Report {
    let job = Job { input: input.into(), output: output.into(), key_in: None, key_out: None, profile: profile(rules) };
    run(&job, &mut |_| {}).unwrap()
}

/// Предпросмотр справа читал лист целиком: на 447 тыс. строк — секунды на каждую правку правил, хотя ничего
/// не проверял. Теперь — первые PREVIEW_ROWS строк; обработка по-прежнему меняет весь лист.
#[test]
fn w01_preview_reads_only_first_rows() {
    let d = tmp("w01");
    let input = d.join("in.xlsx");
    let rows = PREVIEW_ROWS + 10;
    let mut b = Book::default();
    let sh = b.sheet("Л");
    sh.push((1, 0, s("Покупатель")));
    for r in 2..=rows {
        sh.push((r, 0, s(&format!("Покупатель {r}"))));
    }
    write_xlsx(&input, &b);
    let rule = || vec![Rule::new("Л", 0, 2, None, Kind::Text)];
    let p = preview(&input, &profile(rule()), None).unwrap();
    assert_eq!(p[0].cells, (PREVIEW_ROWS - 1) as usize, "предпросмотр — только первые строки листа");
    let rep = go(&input, &d.join("out.xlsx"), rule());
    assert_eq!(rep.rule_cells[0], (rows - 1) as usize, "обработка — весь лист");
    assert!(rep.leaks.is_empty(), "{:?}", rep.leaks);
}

/// В отчёте было «qAll_Sales!A2:A… [Организация]: заменено…» — непонятно, что «Организация» — тип правила,
/// и какая это колонка. Теперь — название колонки из шапки и «правило «…»».
#[test]
fn w02_report_names_columns_and_rule_types() {
    let d = tmp("w02");
    let input = d.join("in.xlsx");
    let mut b = Book::default();
    b.sheet("Л").extend([
        (1, 0, s("Ответственный")),
        (1, 1, s("Покупатель")),
        (2, 0, s("Иванов Иван")),
        (2, 1, s("ООО «Ромашка»")),
        (3, 0, s("Петров Пётр")),
        (3, 1, s("ООО «Лютик»")),
    ]);
    write_xlsx(&input, &b);
    let rep = go(&input, &d.join("out.xlsx"), vec![Rule::new("Л", 0, 2, None, Kind::FullName), Rule::new("Л", 1, 2, None, Kind::Company)]);
    let lines = rep.summary(&profile(vec![Rule::new("Л", 0, 2, None, Kind::FullName), Rule::new("Л", 1, 2, None, Kind::Company)]));
    let text = lines.join("\n");
    assert!(text.contains("колонка «Ответственный», правило «ФИО»"), "{text}");
    assert!(text.contains("колонка «Покупатель», правило «Организация»"), "{text}");
}

/// Таблица без шапки: «шапкой» оказывается строка данных. Реальное значение в журнал (его пересылают) не попадает.
#[test]
fn w03_report_never_shows_real_value_as_column_name() {
    let d = tmp("w03");
    let input = d.join("in.xlsx");
    let mut b = Book::default();
    b.sheet("Л").extend([
        (1, 0, s("Иванов Иван")),
        (1, 1, s("Петров Пётр")),
        (2, 0, s("Сидоров Сидор")),
        (2, 1, s("Петров Пётр")),
        (3, 0, s("Иванов Иван")),
        (3, 1, s("Кузнецов Кузьма")),
    ]);
    write_xlsx(&input, &b);
    let rules = || vec![Rule::new("Л", 0, 2, None, Kind::FullName), Rule::new("Л", 1, 2, None, Kind::FullName)];
    let rep = go(&input, &d.join("out.xlsx"), rules());
    let text = rep.summary(&profile(rules())).join("\n");
    for real in ["Иванов", "Петров", "Сидоров", "Кузнецов"] {
        assert!(!text.contains(real), "в журнале реальное значение «{real}»:\n{text}");
    }
}

/// «(М) ООО "Восход"» → «(М) ООО "Прогресс"»: пометка перед формой осталась в файле. Инвариант по всем
/// написаниям: в результате нет ни одного слова исходника, кроме формы.
#[test]
fn w04_company_keeps_only_legal_form() {
    let d = tmp("w04");
    let input = d.join("in.xlsx");
    let names = ["(М) ООО \"Восход\"", "Филиал №5 ООО «Зенит» (Тверь)", "Склад \"Сфера\"", "АО «Статус» г. Казань"];
    let mut b = Book::default();
    let sh = b.sheet("Л");
    sh.push((1, 0, s("Контрагент")));
    for (i, n) in names.iter().enumerate() {
        sh.push((i as u32 + 2, 0, s(n)));
    }
    write_xlsx(&input, &b);
    let rep = go(&input, &d.join("out.xlsx"), vec![Rule::new("Л", 0, 2, None, Kind::Company)]);
    assert!(rep.leaks.is_empty(), "{:?}", rep.leaks);
    let g = read_book(&d.join("out.xlsx"));
    for (i, n) in names.iter().enumerate() {
        let fake = text(&g, "Л", &format!("A{}", i + 2));
        for w in ["(М)", "Филиал", "№5", "Тверь", "Склад", "Казань", "г.", "Восход", "Зенит", "Сфера", "Статус"] {
            assert!(!fake.contains(w), "{n} → {fake}: осталось «{w}»");
        }
    }
}
